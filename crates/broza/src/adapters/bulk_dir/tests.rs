//! What the reader must do with a real directory, and when it must give up.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use rayon::iter::{IntoParallelIterator, ParallelIterator};

use super::{
    BULK_STATE, Ordering, STATE_REFUSED, STATE_TRUSTED, STATE_UNTESTED, read_dir_with_attributes,
    reset_bulk_state_for_tests,
};

/// Bytes written into the file of the scratch directory.
const FILE_BYTES: usize = 1234;
/// How many threads hammer the reader at once.
const THREADS: usize = 16;

/// Whoever is exercising the reader's one global state, holds this.
static STATE_LOCK: Mutex<()> = Mutex::new(());

/// Exclusive use of the reader's state, restored when the test ends.
///
/// The state is process-wide by design — it is a decision about the kernel,
/// not about a directory — so the tests that drive it have to take turns.
struct KeepState {
    /// What the state was before the test.
    previous: u8,
    /// Kept for its lifetime, not read.
    _turn: MutexGuard<'static, ()>,
}

impl Drop for KeepState {
    fn drop(&mut self) {
        BULK_STATE.store(self.previous, Ordering::SeqCst);
    }
}

fn keep_state() -> KeepState {
    let turn = STATE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    KeepState { previous: BULK_STATE.load(Ordering::SeqCst), _turn: turn }
}

/// A directory with a file and a subdirectory in it.
fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(dir.path().join("file"), vec![0_u8; FILE_BYTES]).unwrap_or_else(|e| panic!("{e}"));
    std::fs::create_dir(dir.path().join("sub")).unwrap_or_else(|e| panic!("{e}"));
    dir
}

#[test]
fn a_real_directory_comes_back_with_the_same_entries_as_read_dir() {
    let _state = keep_state();
    reset_bulk_state_for_tests();
    let dir = scratch();

    let Some(entries) = read_dir_with_attributes(dir.path()) else {
        eprintln!("skipped: getattrlistbulk is not usable here");
        return;
    };

    let mut names: Vec<String> = entries
        .iter()
        .filter_map(|(path, _)| path.file_name().map(|name| name.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    assert_eq!(names, vec!["file".to_owned(), "sub".to_owned()]);
    let file =
        entries.iter().find(|(path, _)| path.ends_with("file")).unwrap_or_else(|| panic!("no file entry"));
    let meta = file.1.as_ref().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(meta.size_bytes, FILE_BYTES as u64);
    assert!(!meta.is_dir);
    assert!(!meta.is_dataless);
}

#[test]
fn reading_one_directory_is_enough_to_settle_whether_the_reader_is_trusted() {
    let _state = keep_state();
    reset_bulk_state_for_tests();
    let dir = scratch();

    let _ = read_dir_with_attributes(dir.path());

    let settled = BULK_STATE.load(Ordering::SeqCst);
    assert!(settled == STATE_TRUSTED || settled == STATE_REFUSED, "still undecided: {settled}");
}

#[test]
fn a_directory_of_directories_leaves_the_question_open() {
    let _state = keep_state();
    reset_bulk_state_for_tests();
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    std::fs::create_dir(dir.path().join("sub")).unwrap_or_else(|e| panic!("{e}"));

    let entries = read_dir_with_attributes(dir.path());

    // Directories carry no file attributes: every entry was stated the plain
    // way, so nothing about the buffer was actually proven.
    assert!(entries.is_some_and(|entries| entries.len() == 1));
    assert_eq!(BULK_STATE.load(Ordering::SeqCst), STATE_UNTESTED);
}

#[test]
fn an_empty_directory_leaves_the_question_open_too() {
    let _state = keep_state();
    reset_bulk_state_for_tests();
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));

    let entries = read_dir_with_attributes(dir.path());

    assert!(entries.is_some_and(|entries| entries.is_empty()));
    assert_eq!(BULK_STATE.load(Ordering::SeqCst), STATE_UNTESTED);
}

#[test]
fn a_reader_that_was_given_up_on_answers_nothing_and_stays_that_way() {
    let _state = keep_state();
    BULK_STATE.store(STATE_REFUSED, Ordering::SeqCst);
    let dir = scratch();

    assert!(read_dir_with_attributes(dir.path()).is_none());
    assert_eq!(BULK_STATE.load(Ordering::SeqCst), STATE_REFUSED, "a refusal is never taken back");
}

#[test]
fn many_threads_reading_at_once_settle_on_one_answer() {
    let _state = keep_state();
    reset_bulk_state_for_tests();
    let dir = scratch();

    let answers: Vec<bool> =
        (0..THREADS).into_par_iter().map(|_| read_dir_with_attributes(dir.path()).is_some()).collect();

    let settled = BULK_STATE.load(Ordering::SeqCst);
    assert!(settled == STATE_TRUSTED || settled == STATE_REFUSED, "still undecided: {settled}");
    assert!(answers.iter().any(|answered| *answered), "nobody got an answer");
}

#[test]
fn a_refusal_sticks_and_a_claim_is_handed_back() {
    let _state = keep_state();
    reset_bulk_state_for_tests();

    super::release_claim();
    assert_eq!(BULK_STATE.load(Ordering::SeqCst), STATE_UNTESTED, "nothing was claimed");

    BULK_STATE.store(super::STATE_TESTING, Ordering::SeqCst);
    super::release_claim();
    assert_eq!(BULK_STATE.load(Ordering::SeqCst), STATE_UNTESTED, "the claim went back");

    assert!(super::refuse::<()>().is_none());
    assert_eq!(BULK_STATE.load(Ordering::SeqCst), STATE_REFUSED);
    super::release_claim();
    assert_eq!(BULK_STATE.load(Ordering::SeqCst), STATE_REFUSED, "a refusal is not a claim");
}

#[test]
fn a_filesystem_that_cannot_do_this_at_all_is_refused_once() {
    let _state = keep_state();
    reset_bulk_state_for_tests();
    BULK_STATE.store(super::STATE_TESTING, Ordering::SeqCst);

    set_errno(libc::ENOTSUP);
    assert!(super::unsupported_or_none::<()>().is_none());
    assert_eq!(
        BULK_STATE.load(Ordering::SeqCst),
        STATE_REFUSED,
        "a filesystem without the call will not grow one"
    );

    reset_bulk_state_for_tests();
    BULK_STATE.store(super::STATE_TESTING, Ordering::SeqCst);
    set_errno(libc::EIO);
    assert!(super::unsupported_or_none::<()>().is_none());
    assert_eq!(
        BULK_STATE.load(Ordering::SeqCst),
        STATE_UNTESTED,
        "one unhappy directory says nothing about the next"
    );
}

/// Set the thread's `errno`, which is what the reader reads after a failure.
fn set_errno(value: i32) {
    // SAFETY: `__error()` returns this thread's own errno slot, valid for the
    // life of the thread, and nothing else is reading it here.
    unsafe { *libc::__error() = value };
}

#[test]
fn a_path_that_is_not_a_directory_is_not_read() {
    let _state = keep_state();
    reset_bulk_state_for_tests();
    let dir = scratch();

    assert!(read_dir_with_attributes(&dir.path().join("file")).is_none());
}

#[test]
fn a_fifo_is_refused_instead_of_waiting_for_a_writer() {
    let _state = keep_state();
    reset_bulk_state_for_tests();
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let fifo = dir.path().join("pipe");
    make_fifo(&fifo);

    // Without O_DIRECTORY | O_NONBLOCK this call would block until somebody
    // opened the other end, which on a scan is for ever.
    let started = std::time::Instant::now();
    let answer = read_dir_with_attributes(&fifo);

    assert!(answer.is_none());
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "the open blocked");
}

/// Create a FIFO, which `std` cannot do.
fn make_fifo(path: &Path) {
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap_or_else(|e| panic!("{e}"));
    // SAFETY: `name` is a live, NUL-terminated C string for the whole call.
    let made = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
    assert_eq!(made, 0, "mkfifo: {}", std::io::Error::last_os_error());
}
