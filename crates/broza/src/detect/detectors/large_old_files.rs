//! `large-old-files`: big files nobody has opened in a long time
//! (`docs/cli-spec.md` §3.3).
//!
//! One finding, `large-old-files.home`, amber and `quarantine`. The candidates
//! are the files of at least [`LARGE_FILE_MIN_BYTES`] among those the home walk
//! reported, outside `~/Library` (application-managed, other detectors' ground)
//! and `~/.Trash` (the trash detector's). For each, `last_used` is
//! `max(atime, kMDItemLastUsedDate)`, Spotlight asked through `mdls` on the
//! process port for the candidates only. A file is proposed when that date and
//! its modification time are both older than `--unused-after`: a file appended
//! to daily has an old access time and is not old. Spotlight is asked only
//! about files whose modification and access times already qualify, and not at all once
//! `mdls` has failed or timed out — a stuck Spotlight costs one timeout, not
//! one per file. A file the filesystem and Spotlight say nothing about is left
//! alone, and so is a file with more than one name — quarantining one hard
//! link frees nothing. When Spotlight has no date for some of the proposed
//! files, the reasoning counts them and states low confidence, as the
//! specification requires. Every date comes from the walk, taken before any
//! detector read a file.

use std::path::PathBuf;

use crate::BrozaError;
use crate::model::{Category, FindingPath};
use crate::scan::FileEntry;

use super::support::{by_size_then_path, finish, path_with, start};
use crate::detect::detector::{DetectContext, Detected, Detector};
use crate::detect::spotlight::{Spotlight, later_of};

/// A file is large from 1 GB, counted the way Finder counts.
pub const LARGE_FILE_MIN_BYTES: u64 = 1_000_000_000;
/// Directories under the home whose files belong to other detectors.
const SKIPPED_UNDER_HOME: [&str; 2] = ["Library", ".Trash"];

/// The `large-old-files` detector.
pub struct LargeOldFiles;

impl Detector for LargeOldFiles {
    fn category(&self) -> Category {
        Category::LargeOldFiles
    }

    fn detect(&self, context: &DetectContext<'_>) -> Result<Detected, BrozaError> {
        let skipped: Vec<PathBuf> = SKIPPED_UNDER_HOME.iter().map(|dir| context.under_home(dir)).collect();
        let mut detected = Detected::default();
        let mut spotlight = Spotlight::new(context.process, "files");
        let judged: Vec<Judged> = context
            .home_files
            .iter()
            .filter(|file| is_candidate(file, &skipped))
            .filter_map(|file| judge(file, context, &mut spotlight))
            .collect();
        detected.warnings.extend(spotlight.warning());
        let low_confidence = judged.iter().filter(|unused| !unused.spotlight_knows).count();
        let mut paths: Vec<FindingPath> = judged.into_iter().map(|unused| unused.path).collect();
        paths.sort_by(by_size_then_path);
        let builder = start(Category::LargeOldFiles, "home", "Large files not opened in a long time")?
            .description(
                "Files of 1 GB or more under your home, outside ~/Library, that nothing has opened or \
                 changed for longer than the unused-after threshold.",
            )
            .reasoning(reasoning(low_confidence));
        Ok(detected.with_finding(finish(builder, paths)?))
    }
}

/// Big enough, with one name, freeing something, and not on another
/// detector's ground.
///
/// Symlinks and cloud placeholders never reach here: the walk reports a
/// placeholder as nothing but a count, and a symlink's own `lstat` size is
/// its target path, nowhere near the threshold. A clone whose family is
/// counted elsewhere arrives with no allocated bytes, and proposing a file
/// whose removal frees nothing is not a suggestion.
fn is_candidate(file: &FileEntry, skipped: &[PathBuf]) -> bool {
    file.size_bytes >= LARGE_FILE_MIN_BYTES
        && file.allocated_bytes > 0
        && file.link_count <= 1
        && !skipped.iter().any(|dir| file.path.starts_with(dir))
}

/// One proposed file, and whether Spotlight had a say in it.
struct Judged {
    path: FindingPath,
    spotlight_knows: bool,
}

/// The finding path for `file` when it is unused; `None` when it is in use or
/// of unknown age. The walk's dates are judged first: Spotlight can only make
/// `last_used` later, so it is asked only about a file that could be proposed.
fn judge(file: &FileEntry, context: &DetectContext<'_>, spotlight: &mut Spotlight<'_>) -> Option<Judged> {
    let modified = file.modified?;
    if !context.is_unused_since(modified) {
        return None;
    }
    if file.accessed.is_some_and(|accessed| !context.is_unused_since(accessed)) {
        return None;
    }
    let spotlight_date = spotlight.last_used(&file.path);
    let last_used = later_of(file.accessed, spotlight_date)?;
    if !context.is_unused_since(last_used) {
        return None;
    }
    Some(Judged {
        path: path_with(&file.path, file.allocated_bytes, Some(last_used)),
        spotlight_knows: spotlight_date.is_some(),
    })
}

fn reasoning(low_confidence: usize) -> String {
    const BASE: &str = "Neither you nor any app has opened or changed these files since before the \
                        unused-after threshold; they are your own data, so review the list before \
                        applying.";
    if low_confidence == 0 {
        return BASE.to_owned();
    }
    format!(
        "{BASE} Low confidence for {low_confidence} of them: Spotlight has no last-used date, so the \
         judgement rests on the access time alone."
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use jiff::Timestamp;

    use super::*;
    use crate::detect::spotlight::{MDLS, MDLS_ARGS, SPOTLIGHT_UNAVAILABLE_CODE};
    use crate::detect::test_support::{context_over, home};
    use crate::model::{Action, Finding, Risk};
    use crate::testing::{FakeFileOps, FakeRunner};

    const H: &str = "/System/Volumes/Data/Users/dana";
    const OLD: &str = "/System/Volumes/Data/Users/dana/Movies/holiday-2019.mov";
    const OLD_TOO: &str = "/System/Volumes/Data/Users/dana/Movies/wedding-2018.mov";
    const RECENT: &str = "/System/Volumes/Data/Users/dana/Movies/last-week.mov";
    const APPENDED: &str = "/System/Volumes/Data/Users/dana/vm/disk.img";
    const SMALL: &str = "/System/Volumes/Data/Users/dana/Documents/notes.txt";
    const IN_LIBRARY: &str = "/System/Volumes/Data/Users/dana/Library/Containers/x/big.bin";
    const IN_TRASH: &str = "/System/Volumes/Data/Users/dana/.Trash/big.dmg";
    const LINKED: &str = "/System/Volumes/Data/Users/dana/Movies/holiday-copy.mov";
    const LONG_AGO: &str = "2019-08-10T14:00:00Z";
    const LATELY: &str = "2026-09-10T08:00:00Z";

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn fs() -> FakeFileOps {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_dir(H);
        for (path, size, modified, accessed) in [
            (OLD, 4_200_000_000_u64, LONG_AGO, LONG_AGO),
            (OLD_TOO, 2_100_000_000, LONG_AGO, LONG_AGO),
            (RECENT, 3_000_000_000, LONG_AGO, LATELY),
            (APPENDED, 8_000_000_000, LATELY, LONG_AGO),
            (SMALL, 500_000_000, LONG_AGO, LONG_AGO),
            (IN_LIBRARY, 2_000_000_000, LONG_AGO, LONG_AGO),
            (IN_TRASH, 2_000_000_000, LONG_AGO, LONG_AGO),
        ] {
            fs.add_file(path, &[]);
            fs.set_size(path, size);
            fs.set_times(path, at(modified), at(accessed));
        }
        fs
    }

    fn runner(answers: &[(&str, &str)]) -> FakeRunner {
        let runner = FakeRunner::new();
        for (path, answer) in answers {
            let args: Vec<&str> = MDLS_ARGS.iter().copied().chain([*path]).collect();
            runner.script_output(
                MDLS,
                &args,
                crate::ports::ProcessOutput {
                    success: true,
                    code: Some(0),
                    stdout: format!("{answer}\n").into_bytes(),
                    stderr: Vec::new(),
                },
            );
        }
        runner
    }

    fn detect(fs: &FakeFileOps, runner: FakeRunner) -> Detected {
        detect_counting(fs, runner).0
    }

    /// The detection, and how many times `mdls` was run.
    fn detect_counting(fs: &FakeFileOps, runner: FakeRunner) -> (Detected, usize) {
        let world = context_over(fs, home()).with_process(runner);
        let detected = LargeOldFiles.detect(&world.context()).unwrap();
        (detected, world.process().calls().len())
    }

    fn proposed(detected: &Detected) -> Vec<String> {
        detected.findings.iter().flat_map(Finding::paths).map(|p| p.path.display().to_string()).collect()
    }

    #[test]
    fn only_big_old_untouched_files_outside_library_and_trash_are_proposed() {
        // Neither the recently read nor the recently appended file is scripted:
        // Spotlight is asked about files that are old by every other measure.
        let answers = [(OLD, "(null)"), (OLD_TOO, "(null)")];

        let (detected, asked) = detect_counting(&fs(), runner(&answers));
        assert_eq!(asked, 2, "one question per file that could be proposed");

        assert_eq!(proposed(&detected), vec![OLD.to_owned(), OLD_TOO.to_owned()], "{detected:?}");
        let finding = &detected.findings[0];
        assert_eq!(finding.id().to_string(), "large-old-files.home");
        assert_eq!((finding.risk(), finding.action()), (Risk::Amber, Action::Quarantine));
        assert_eq!(finding.paths()[0].last_used, Some(at(LONG_AGO)));
        assert!(finding.reasoning().unwrap().contains("Low confidence for 2 of them"), "{finding:?}");
        assert!(detected.warnings.is_empty(), "{:?}", detected.warnings);
    }

    #[test]
    fn spotlight_keeps_a_file_it_saw_opened_lately_and_dates_one_it_saw_opened_long_ago() {
        let answers = [(OLD, "2026-06-01 10:00:00 +0000"), (OLD_TOO, "2020-02-02 12:00:00 +0000")];

        let detected = detect(&fs(), runner(&answers));

        assert_eq!(proposed(&detected), vec![OLD_TOO.to_owned()], "{detected:?}");
        let finding = &detected.findings[0];
        assert_eq!(finding.paths()[0].last_used, Some(at("2020-02-02T12:00:00Z")), "max(atime, Spotlight)");
        assert!(!finding.reasoning().unwrap().contains("Low confidence"), "{finding:?}");
    }

    #[test]
    fn a_failing_mdls_is_asked_once_is_one_warning_and_the_files_are_judged_by_access_time() {
        let runner = FakeRunner::new();
        for path in [OLD, OLD_TOO] {
            let args: Vec<&str> = MDLS_ARGS.iter().copied().chain([path]).collect();
            runner.script_failure(MDLS, &args, "mdls: command not found");
        }

        let (detected, asked) = detect_counting(&fs(), runner);

        assert_eq!(asked, 1, "after one failure Spotlight is not asked again");
        assert_eq!(proposed(&detected), vec![OLD.to_owned(), OLD_TOO.to_owned()], "{detected:?}");
        assert_eq!(detected.warnings.len(), 1, "{:?}", detected.warnings);
        assert_eq!(detected.warnings[0].code, SPOTLIGHT_UNAVAILABLE_CODE);
        assert!(detected.warnings[0].message.contains("command not found"), "{:?}", detected.warnings);
        assert!(detected.findings[0].reasoning().unwrap().contains("Low confidence for 2"));
    }

    #[test]
    fn a_file_with_a_second_name_is_never_proposed() {
        let fs = fs();
        fs.add_hard_link(OLD, LINKED);
        let answers = [(OLD_TOO, "(null)")];

        let detected = detect(&fs, runner(&answers));

        assert_eq!(proposed(&detected), vec![OLD_TOO.to_owned()], "{detected:?}");
    }

    #[test]
    fn a_home_without_big_files_asks_spotlight_nothing() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_file(SMALL, &[]);
        fs.set_size(SMALL, 500_000_000);
        let runner = FakeRunner::new();

        let detected = detect(&fs, runner);

        assert!(detected.findings.is_empty() && detected.warnings.is_empty(), "{detected:?}");
    }
}
