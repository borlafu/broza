//! The interactive [`Prompter`]: the only place Broza reads from a person.
//!
//! Two rules shape it. Prompts are conversation, so they go to stderr
//! (`docs/cli-spec.md` §0, principle 5). And the answer is read from `/dev/tty`
//! rather than stdin, so a pipeline on stdin cannot answer a confirmation on
//! the user's behalf — if there is no terminal the answer is
//! [`Answer::NoTty`], which the caller turns into exit `7`.
//!
//! Everything that decides *what* is printed and *what an answer means* is a
//! pure function tested below. The read itself is four lines and is the one
//! thing a test in CI cannot exercise.

use std::fmt::Write as _;
use std::io::Write as _;

use broza::ports::{Answer, ConfirmationRequest, Prompter};

use crate::output::format_bytes;
use crate::output::human::risk_label;

/// The terminal Broza reads confirmations from, whatever stdin is connected to.
const TTY_PATH: &str = "/dev/tty";
/// How many paths a prompt lists before it stops.
const PREVIEW_LIMIT: usize = 10;
/// The `y/N` question. The capital `N` is the default, as everywhere else.
const PROCEED_QUESTION: &str = "Proceed? [y/N] ";
/// Answers that count as yes. Anything else, including an empty line, is no.
const YES_ANSWERS: [&str; 2] = ["y", "yes"];

/// Asks on stderr and reads from `/dev/tty`.
///
/// `interactive` comes from [`crate::env::RuntimeEnv::is_interactive`], so the
/// decision "may Broza prompt at all" is taken once, from one snapshot of the
/// environment, and not rediscovered here.
#[derive(Debug, Clone, Copy)]
pub struct TtyPrompter {
    /// `false` when there is no terminal, or `CI` is set.
    interactive: bool,
}

impl TtyPrompter {
    /// A prompter that asks only when `interactive`.
    pub const fn new(interactive: bool) -> Self {
        Self { interactive }
    }

    /// Print `prompt` on stderr and return the line the user typed.
    ///
    /// `None` when there is no terminal, or when reading from it fails: both
    /// mean Broza did not get an answer, and a missing answer is never a yes.
    fn ask(self, prompt: &str) -> Option<String> {
        use std::io::BufRead as _;

        if !self.interactive {
            return None;
        }
        let tty = std::fs::File::options().read(true).write(true).open(TTY_PATH).ok()?;
        let mut stderr = std::io::stderr();
        stderr.write_all(prompt.as_bytes()).ok()?;
        stderr.flush().ok()?;
        let mut line = String::new();
        std::io::BufReader::new(tty).read_line(&mut line).ok()?;
        Some(line)
    }
}

impl Prompter for TtyPrompter {
    fn confirm(&self, request: &ConfirmationRequest) -> Answer {
        match self.ask(&format!("{}{PROCEED_QUESTION}", summarize(request))) {
            None => Answer::NoTty,
            Some(line) if is_yes(&line) => Answer::Yes,
            Some(_) => Answer::No,
        }
    }

    fn confirm_literal(&self, request: &ConfirmationRequest, expected: &str) -> Answer {
        match self.ask(&format!("{}{}", summarize(request), literal_question(expected))) {
            None => Answer::NoTty,
            Some(line) if is_literal(&line, expected) => Answer::Yes,
            Some(_) => Answer::No,
        }
    }
}

/// The lines shown above the question: what, how much, and a few examples.
pub fn summarize(request: &ConfirmationRequest) -> String {
    let mut text = format!(
        "\n{} {} ({}), risk {}{}.\n",
        request.item_count,
        if request.item_count == 1 { "item" } else { "items" },
        format_bytes(request.total_bytes),
        risk_label(request.max_risk),
        if request.irreversible { ", irreversible" } else { "" }
    );
    for path in request.preview.iter().take(PREVIEW_LIMIT) {
        let _ignored = writeln!(text, "  {path}");
    }
    let hidden = request.preview.len().saturating_sub(PREVIEW_LIMIT);
    if hidden > 0 {
        let _ignored = writeln!(text, "  … and {hidden} more");
    }
    text
}

/// The question asked when a literal word has to be typed (`--purge`).
pub fn literal_question(expected: &str) -> String {
    format!("Type {expected} exactly to confirm, anything else cancels: ")
}

/// `true` only for an explicit yes; an empty line is a no.
pub fn is_yes(answer: &str) -> bool {
    let trimmed = answer.trim().to_ascii_lowercase();
    YES_ANSWERS.contains(&trimmed.as_str())
}

/// `true` only when `answer` is `expected`, character for character.
///
/// Surrounding whitespace is forgiven because a terminal always appends a
/// newline; case is not, because typing `purge` is not typing `PURGE`
/// (`AGENTS.md` §2.2).
pub fn is_literal(answer: &str, expected: &str) -> bool {
    answer.trim() == expected
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use broza::model::Risk;
    use broza::safety::PURGE_LITERAL;

    use super::*;

    fn request(item_count: usize, preview: &[&str]) -> ConfirmationRequest {
        ConfirmationRequest {
            max_risk: Risk::Green,
            item_count,
            total_bytes: 138_200_000_000,
            irreversible: false,
            preview: preview.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    #[test]
    fn a_summary_reports_the_count_the_size_and_the_risk_label() {
        let text = summarize(&request(3, &["/a", "/b"]));

        assert!(text.contains("3 items"), "{text}");
        assert!(text.contains("138.2 GB"), "{text}");
        assert!(text.contains("SAFE"), "{text}");
        assert!(text.contains("/a") && text.contains("/b"), "{text}");
    }

    #[test]
    fn a_single_item_is_not_called_items() {
        assert!(summarize(&request(1, &[])).contains("1 item ("), "{}", summarize(&request(1, &[])));
    }

    #[test]
    fn a_long_preview_is_cut_and_says_how_much_it_hid() {
        let paths: Vec<String> = (0..25).map(|i| format!("/p{i}")).collect();
        let borrowed: Vec<&str> = paths.iter().map(String::as_str).collect();

        let text = summarize(&request(25, &borrowed));

        assert!(text.contains("/p9"), "{text}");
        assert!(!text.contains("/p10"), "only ten paths are shown: {text}");
        assert!(text.contains("… and 15 more"), "{text}");
    }

    #[test]
    fn an_irreversible_request_says_so_before_the_question() {
        let irreversible = ConfirmationRequest { irreversible: true, ..request(1, &[]) };
        assert!(summarize(&irreversible).contains("irreversible"));
        assert!(!summarize(&request(1, &[])).contains("irreversible"));
    }

    #[test]
    fn the_risk_label_of_the_request_reaches_the_prompt() {
        let amber = ConfirmationRequest { max_risk: Risk::Amber, ..request(1, &[]) };
        assert!(summarize(&amber).contains("REVIEW"), "{}", summarize(&amber));
    }

    #[test]
    fn only_an_explicit_yes_is_a_yes() {
        for yes in ["y", "Y", "yes", "YES", " yes \n"] {
            assert!(is_yes(yes), "{yes:?}");
        }
        for no in ["", "\n", "n", "no", "yep", "yes please", "1"] {
            assert!(!is_yes(no), "{no:?}");
        }
    }

    #[test]
    fn the_literal_confirmation_is_case_sensitive() {
        assert!(is_literal("PURGE\n", PURGE_LITERAL));
        assert!(is_literal("  PURGE  ", PURGE_LITERAL));
        for wrong in ["purge", "Purge", "PURGE!", "", "y"] {
            assert!(!is_literal(wrong, PURGE_LITERAL), "{wrong:?}");
        }
    }

    #[test]
    fn the_literal_question_names_the_word_to_type() {
        let question = literal_question(PURGE_LITERAL);
        assert!(question.contains(PURGE_LITERAL), "{question}");
        assert!(question.ends_with(": "), "{question}");
    }

    #[test]
    fn without_a_terminal_nothing_is_asked_and_nothing_is_confirmed() {
        let prompter = TtyPrompter::new(false);

        assert_eq!(prompter.confirm(&request(1, &[])), Answer::NoTty);
        assert_eq!(prompter.confirm_literal(&request(1, &[]), PURGE_LITERAL), Answer::NoTty);
    }
}
