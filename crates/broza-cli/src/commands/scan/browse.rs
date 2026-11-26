//! `broza scan -i` (`docs/cli-spec.md` §3.1): open the largest consumers one
//! folder at a time.
//!
//! The first screen is the usual report with its directories numbered. Typing
//! a number walks that folder — warm from the scan cache — and lists its own
//! largest consumers, numbered again; `..` goes back one screen and prints its
//! list again, and `q` (or end of input) quits. Listings go to stdout; the prompt and every message go
//! to stderr through [`LineInput`]. Nothing is ever written to disk.

use std::io::Write;
use std::path::Path;

use broza::BrozaError;
use broza::model::{LargestItem, Warning};

use crate::output::human::scan::folders::{abbreviate, render_listing};

/// What the user is asked after every screen.
pub const PROMPT: &str = "Open a number, .. to go back, q to quit: ";

/// Where the loop reads the user's answers and says what it has to say.
pub trait LineInput {
    /// Show `prompt` and read one line; `None` at end of input.
    fn read_line(&mut self, prompt: &str) -> Option<String>;
    /// Tell the user something that is not a listing.
    fn say(&mut self, message: &str);
}

/// The terminal: prompt and messages on stderr, answers from stdin.
pub struct Terminal;

impl LineInput for Terminal {
    fn read_line(&mut self, prompt: &str) -> Option<String> {
        let mut stderr = std::io::stderr();
        let _ignored = write!(stderr, "{prompt}");
        let _ignored = stderr.flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    }

    fn say(&mut self, message: &str) {
        let _ignored = writeln!(std::io::stderr(), "{message}");
    }
}

/// The largest consumers of one folder and the warnings walking it raised.
pub type Walked = (Vec<LargestItem>, Vec<Warning>);

/// One screen: the list as printed, and the directories numbered on it.
struct Screen {
    listing: String,
    dirs: Vec<LargestItem>,
}

/// Run the loop from a first screen whose numbered directories are `first`,
/// printed as `first_listing`. `walk_into` lists a folder; the warnings of
/// every walk are returned.
pub fn browse(
    first: Vec<LargestItem>,
    first_listing: String,
    input: &mut dyn LineInput,
    out: &mut dyn Write,
    walk_into: &mut dyn FnMut(&Path) -> Result<Walked, BrozaError>,
    home: Option<&Path>,
) -> Vec<Warning> {
    let mut stack = vec![Screen { listing: first_listing, dirs: first }];
    let mut warnings = Vec::new();
    while let Some(line) = input.read_line(PROMPT) {
        match line.trim() {
            "q" | "quit" => break,
            ".." => {
                if stack.len() > 1 {
                    stack.pop();
                    if let Some(screen) = stack.last() {
                        let _ignored = writeln!(out, "\n{}\n", screen.listing);
                    }
                } else {
                    input.say("Already at the top.");
                }
            }
            "" => {}
            answer => match answer.parse::<usize>().ok().and_then(|n| pick(&stack, n)) {
                Some(dir) => match walk_into(&dir.path) {
                    Ok((items, walked)) => {
                        warnings.extend(walked);
                        let screen = screen_of(&dir.path, &items, home);
                        let _ignored = writeln!(out, "\n{}\n", screen.listing);
                        stack.push(screen);
                    }
                    Err(error) => input.say(&format!("Cannot open {}: {error}", abbreviate(&dir.path, home))),
                },
                None => input.say("Type the number of a folder listed above, .. or q."),
            },
        }
    }
    warnings
}

/// The `n`th directory of the current screen, counting from 1.
fn pick(stack: &[Screen], n: usize) -> Option<LargestItem> {
    stack.last()?.dirs.get(n.checked_sub(1)?).cloned()
}

/// The screen of `folder`: its numbered list and its directories.
fn screen_of(folder: &Path, items: &[LargestItem], home: Option<&Path>) -> Screen {
    let refs: Vec<&LargestItem> = items.iter().collect();
    let heading = format!("Largest consumers in {}:", abbreviate(folder, home));
    let mut next = 1;
    let listing = render_listing(&heading, &refs, home, Some(&mut next));
    let dirs = items.iter().filter(|item| item.kind == broza::model::ItemKind::Directory).cloned().collect();
    Screen { listing, dirs }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::VecDeque;
    use std::path::PathBuf;

    use broza::model::ItemKind;

    use super::*;

    /// Scripted answers, and everything said back.
    struct Script {
        answers: VecDeque<&'static str>,
        said: Vec<String>,
    }

    impl LineInput for Script {
        fn read_line(&mut self, _prompt: &str) -> Option<String> {
            self.answers.pop_front().map(str::to_owned)
        }

        fn say(&mut self, message: &str) {
            self.said.push(message.to_owned());
        }
    }

    fn script(answers: &[&'static str]) -> Script {
        Script { answers: answers.iter().copied().collect(), said: Vec::new() }
    }

    fn dir(path: &str, size: u64) -> LargestItem {
        LargestItem {
            path: PathBuf::from(path),
            size_bytes: size,
            kind: ItemKind::Directory,
            volume_id: "disk3s5".parse().unwrap(),
        }
    }

    #[test]
    fn a_number_opens_that_folder_and_dot_dot_goes_back_to_the_previous_numbers() {
        let mut input = script(&["1", "1", "..", "2", "q", "never read"]);
        let mut out = Vec::new();
        let mut opened = Vec::new();
        let mut walk = |path: &Path| -> Result<Walked, BrozaError> {
            opened.push(path.to_path_buf());
            let child = path.join("inner");
            Ok((vec![dir(child.to_str().unwrap(), 10), dir(&format!("{}2", child.display()), 5)], Vec::new()))
        };

        browse(
            vec![dir("/Users/dana/Library", 300)],
            "FIRST".to_owned(),
            &mut input,
            &mut out,
            &mut walk,
            Some(Path::new("/Users/dana")),
        );

        assert_eq!(
            opened,
            vec![
                PathBuf::from("/Users/dana/Library"),
                PathBuf::from("/Users/dana/Library/inner"),
                PathBuf::from("/Users/dana/Library/inner2"),
            ],
            "after .., number 2 is the second folder of the screen above"
        );
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Largest consumers in ~/Library:\n   1  10 B  ~/Library/inner/"), "{text}");
        assert_eq!(
            text.matches("Largest consumers in ~/Library:").count(),
            2,
            "printed when opened, and again after `..` from the folder below: {text}"
        );
        assert_eq!(input.answers.len(), 1, "q stops reading");
    }

    #[test]
    fn a_wrong_answer_or_a_failed_walk_is_said_and_the_loop_goes_on() {
        let mut input = script(&["7", "abc", "..", "1"]);
        let mut out = Vec::new();
        let mut walk = |_: &Path| -> Result<Walked, BrozaError> {
            Err(BrozaError::PermissionDenied { path: PathBuf::from("/x") })
        };

        let warnings = browse(vec![dir("/x", 1)], "FIRST".to_owned(), &mut input, &mut out, &mut walk, None);

        assert!(warnings.is_empty());
        assert_eq!(input.said[0], "Type the number of a folder listed above, .. or q.");
        assert_eq!(input.said[1], "Type the number of a folder listed above, .. or q.");
        assert_eq!(input.said[2], "Already at the top.");
        assert!(input.said[3].starts_with("Cannot open /x:"), "{:?}", input.said);
        assert!(out.is_empty(), "nothing was listed");
    }

    #[test]
    fn the_warnings_of_every_walk_are_kept_for_the_end() {
        let mut input = script(&["1"]);
        let warning = Warning { code: "permission_denied".into(), message: "m".into(), path: None };
        let mut walk = |_: &Path| -> Result<Walked, BrozaError> { Ok((Vec::new(), vec![warning.clone()])) };

        let warnings =
            browse(vec![dir("/x", 1)], "FIRST".to_owned(), &mut input, &mut Vec::new(), &mut walk, None);

        assert_eq!(warnings.len(), 1);
    }
}
