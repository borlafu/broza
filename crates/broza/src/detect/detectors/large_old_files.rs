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
//! to daily has an old access time and is not old. A file the filesystem and
//! Spotlight say nothing about is left alone, and so is a file with more than
//! one name — quarantining one hard link frees nothing. When Spotlight has no
//! date for some of the proposed files, the reasoning counts them and states
//! low confidence, as the specification requires.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;

use crate::BrozaError;
use crate::model::{Category, Diagnostic, FindingPath};
use crate::ports::{EntryMetadata, ProcessRunner};
use crate::scan::FileEntry;

use super::support::{by_size_then_path, finish, path_with, start};
use crate::detect::detector::{DetectContext, Detected, Detector, first_line};

/// A file is large from 1 GB, counted the way Finder counts.
pub const LARGE_FILE_MIN_BYTES: u64 = 1_000_000_000;
/// Spotlight's metadata query tool.
pub const MDLS: &str = "/usr/bin/mdls";
/// The one attribute Broza asks for, raw: one line, or `(null)`. The path
/// comes last; it is absolute, so it can never be read as an option.
pub const MDLS_ARGS: [&str; 3] = ["-name", "kMDItemLastUsedDate", "-raw"];
/// Warning code when Spotlight could not be asked at all.
pub const SPOTLIGHT_UNAVAILABLE_CODE: &str = "spotlight_unavailable";
/// `mdls` answers in milliseconds; a stuck Spotlight must not stall `suggest`.
const MDLS_TIMEOUT: Duration = Duration::from_secs(5);
/// What `mdls -raw` prints when the attribute is unset.
const MDLS_NULL: &str = "(null)";
/// How `mdls -raw` prints a date: `2026-09-21 10:48:47 +0000`.
const MDLS_DATE_FORMAT: &str = "%Y-%m-%d %H:%M:%S %z";
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
        let mut spotlight = Spotlight::new(context.process);
        let mut judged: Vec<Judged> = Vec::new();
        for file in context.home_files.iter().filter(|file| is_candidate(file, &skipped)) {
            match judge(file, context, &mut spotlight) {
                Ok(Some(unused)) => judged.push(unused),
                Ok(None) => {}
                Err(error) => detected.warnings.push(Detected::unreadable(
                    Category::LargeOldFiles,
                    &file.path,
                    "its dates",
                    &error,
                )),
            }
        }
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

/// Big enough, and not on another detector's ground.
fn is_candidate(file: &FileEntry, skipped: &[PathBuf]) -> bool {
    file.size_bytes >= LARGE_FILE_MIN_BYTES && !skipped.iter().any(|dir| file.path.starts_with(dir))
}

/// One proposed file, and whether Spotlight had a say in it.
struct Judged {
    path: FindingPath,
    spotlight_knows: bool,
}

/// The finding path for `file` when it is unused; `None` when it is in use,
/// not a plain single-named file, or of unknown age.
fn judge(
    file: &FileEntry,
    context: &DetectContext<'_>,
    spotlight: &mut Spotlight<'_>,
) -> Result<Option<Judged>, BrozaError> {
    let meta = context.fs.metadata(&file.path)?;
    if !is_plain_single_file(&meta) {
        return Ok(None);
    }
    let spotlight_date = spotlight.last_used(&file.path);
    let Some(last_used) = later_of(meta.accessed, spotlight_date) else {
        return Ok(None);
    };
    let last_touched = later_of(Some(last_used), meta.modified).unwrap_or(last_used);
    if !context.is_unused_since(last_touched) {
        return Ok(None);
    }
    Ok(Some(Judged {
        path: path_with(&file.path, file.allocated_bytes, Some(last_used)),
        spotlight_knows: spotlight_date.is_some(),
    }))
}

/// A regular file with one name whose bytes are on this disk.
fn is_plain_single_file(meta: &EntryMetadata) -> bool {
    !meta.is_dir && !meta.is_symlink && !meta.is_dataless && meta.link_count <= 1
}

/// The later of two optional instants; whichever exists when only one does.
fn later_of(a: Option<Timestamp>, b: Option<Timestamp>) -> Option<Timestamp> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
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

/// Spotlight, asked once per candidate; remembers whether it could be asked.
struct Spotlight<'a> {
    process: &'a dyn ProcessRunner,
    /// Why `mdls` could not answer, the first time it failed.
    failure: Option<String>,
}

impl<'a> Spotlight<'a> {
    fn new(process: &'a dyn ProcessRunner) -> Self {
        Self { process, failure: None }
    }

    /// `kMDItemLastUsedDate` of `path`, when Spotlight has one.
    fn last_used(&mut self, path: &Path) -> Option<Timestamp> {
        let path_text = path.to_str()?;
        let args: Vec<&str> = MDLS_ARGS.iter().copied().chain([path_text]).collect();
        let output = match self.process.run(MDLS, &args, MDLS_TIMEOUT) {
            Ok(output) if output.success => output,
            Ok(output) => {
                self.note_failure(&output.stderr_text());
                return None;
            }
            Err(error) => {
                self.note_failure(&error.to_string());
                return None;
            }
        };
        parse_mdls_date(output.stdout_text().trim())
    }

    fn note_failure(&mut self, reason: &str) {
        if self.failure.is_none() {
            self.failure = Some(first_line(reason));
        }
    }

    /// One warning for the whole run, when `mdls` failed at least once.
    fn warning(&self) -> Option<Diagnostic> {
        self.failure.as_ref().map(|reason| Diagnostic {
            code: SPOTLIGHT_UNAVAILABLE_CODE.to_owned(),
            message: format!(
                "Spotlight could not be asked when files were last opened ({reason}); large old files \
                 were judged by access time alone"
            ),
            path: None,
        })
    }
}

/// A date as `mdls -raw` prints it, or nothing for `(null)` and anything else.
fn parse_mdls_date(raw: &str) -> Option<Timestamp> {
    if raw.is_empty() || raw == MDLS_NULL {
        return None;
    }
    Timestamp::strptime(MDLS_DATE_FORMAT, raw).ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
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
        let world = context_over(fs, home()).with_process(runner);
        LargeOldFiles.detect(&world.context()).unwrap()
    }

    fn proposed(detected: &Detected) -> Vec<String> {
        detected.findings.iter().flat_map(Finding::paths).map(|p| p.path.display().to_string()).collect()
    }

    #[test]
    fn only_big_old_untouched_files_outside_library_and_trash_are_proposed() {
        let answers = [(OLD, "(null)"), (OLD_TOO, "(null)"), (RECENT, "(null)"), (APPENDED, "(null)")];

        let detected = detect(&fs(), runner(&answers));

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
        let answers = [
            (OLD, "2026-06-01 10:00:00 +0000"),
            (OLD_TOO, "2020-02-02 12:00:00 +0000"),
            (RECENT, "(null)"),
            (APPENDED, "(null)"),
        ];

        let detected = detect(&fs(), runner(&answers));

        assert_eq!(proposed(&detected), vec![OLD_TOO.to_owned()], "{detected:?}");
        let finding = &detected.findings[0];
        assert_eq!(finding.paths()[0].last_used, Some(at("2020-02-02T12:00:00Z")), "max(atime, Spotlight)");
        assert!(!finding.reasoning().unwrap().contains("Low confidence"), "{finding:?}");
    }

    #[test]
    fn a_failing_mdls_is_one_warning_and_the_files_are_judged_by_access_time() {
        let runner = FakeRunner::new();
        for path in [OLD, OLD_TOO, RECENT, APPENDED] {
            let args: Vec<&str> = MDLS_ARGS.iter().copied().chain([path]).collect();
            runner.script_failure(MDLS, &args, "mdls: command not found");
        }

        let detected = detect(&fs(), runner);

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
        let answers = [
            (OLD, "(null)"),
            (OLD_TOO, "(null)"),
            (RECENT, "(null)"),
            (APPENDED, "(null)"),
            (LINKED, "(null)"),
        ];

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

    #[test]
    fn mdls_dates_parse_and_null_does_not() {
        assert_eq!(parse_mdls_date("2026-09-21 10:48:47 +0000"), Some(at("2026-09-21T10:48:47Z")));
        assert_eq!(parse_mdls_date("(null)"), None);
        assert_eq!(parse_mdls_date(""), None);
        assert_eq!(parse_mdls_date("yesterday"), None);
    }
}
