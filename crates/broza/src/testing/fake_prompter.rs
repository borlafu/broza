//! Scripted [`Prompter`](crate::ports::Prompter).

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::ports::{Answer, ConfirmationRequest, Prompter};

/// Answer given once the scripted queue is empty.
///
/// Declining is the safe default: a test that forgot to script an answer must never
/// silently authorise a deletion (`AGENTS.md` §2.4).
const EXHAUSTED_ANSWER: Answer = Answer::No;

/// A prompt a [`FakePrompter`] was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedPrompt {
    /// What the user was asked to confirm.
    pub request: ConfirmationRequest,
    /// Literal the user had to type, when the prompt demanded one (`PURGE`).
    pub expected_literal: Option<String>,
}

/// A [`Prompter`] that replays scripted answers and records what it was asked.
#[derive(Debug, Default)]
pub struct FakePrompter {
    /// Answers still to be given, in order.
    answers: Mutex<VecDeque<Answer>>,
    /// Every prompt, in order.
    prompts: Mutex<Vec<RecordedPrompt>>,
    /// Answer repeated forever once set, ignoring the queue.
    constant: Option<Answer>,
}

impl FakePrompter {
    /// A prompter that answers `answers` in order, then declines.
    pub fn scripted(answers: &[Answer]) -> Self {
        Self { answers: Mutex::new(answers.iter().copied().collect()), ..Self::default() }
    }

    /// A prompter that always gives the same answer.
    pub fn always(answer: Answer) -> Self {
        Self { constant: Some(answer), ..Self::default() }
    }

    /// Append `answers` to the queue, for a prompter already wired into a bundle.
    pub fn queue(&self, answers: &[Answer]) {
        lock(&self.answers).extend(answers.iter().copied());
    }

    /// Every prompt received, in order.
    pub fn prompts(&self) -> Vec<RecordedPrompt> {
        lock(&self.prompts).clone()
    }

    /// Every confirmation request received, in order.
    pub fn requests(&self) -> Vec<ConfirmationRequest> {
        lock(&self.prompts).iter().map(|prompt| prompt.request.clone()).collect()
    }

    /// Every literal the prompter was asked to demand, in order.
    pub fn literals(&self) -> Vec<String> {
        lock(&self.prompts).iter().filter_map(|prompt| prompt.expected_literal.clone()).collect()
    }

    /// Answers left in the queue.
    pub fn remaining(&self) -> usize {
        lock(&self.answers).len()
    }

    /// Record the prompt and take the next scripted answer.
    fn answer(&self, request: &ConfirmationRequest, expected_literal: Option<String>) -> Answer {
        lock(&self.prompts).push(RecordedPrompt { request: request.clone(), expected_literal });
        match self.constant {
            Some(answer) => answer,
            None => lock(&self.answers).pop_front().unwrap_or(EXHAUSTED_ANSWER),
        }
    }
}

impl Prompter for FakePrompter {
    fn confirm(&self, request: &ConfirmationRequest) -> Answer {
        self.answer(request, None)
    }

    fn confirm_literal(&self, request: &ConfirmationRequest, expected: &str) -> Answer {
        self.answer(request, Some(expected.to_owned()))
    }
}

/// Lock a mutex, recovering the value when another test thread poisoned it.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::{EXHAUSTED_ANSWER, FakePrompter};
    use crate::model::Risk;
    use crate::ports::{Answer, ConfirmationRequest, Prompter};

    fn request() -> ConfirmationRequest {
        ConfirmationRequest {
            max_risk: Risk::Green,
            item_count: 3,
            total_bytes: 1024,
            irreversible: false,
            preview: vec!["/tmp/a".to_owned()],
        }
    }

    #[test]
    fn gives_the_scripted_answers_in_order() {
        let prompter = FakePrompter::scripted(&[Answer::Yes, Answer::No]);

        assert_eq!(prompter.confirm(&request()), Answer::Yes);
        assert_eq!(prompter.confirm(&request()), Answer::No);
        assert_eq!(prompter.remaining(), 0);
    }

    #[test]
    fn declines_once_the_queue_is_empty() {
        let prompter = FakePrompter::scripted(&[]);

        assert_eq!(prompter.confirm(&request()), EXHAUSTED_ANSWER);
        assert_eq!(EXHAUSTED_ANSWER, Answer::No);
    }

    #[test]
    fn answers_can_be_queued_after_construction() {
        let prompter = FakePrompter::scripted(&[]);

        prompter.queue(&[Answer::Yes]);

        assert_eq!(prompter.confirm(&request()), Answer::Yes);
        assert_eq!(prompter.confirm(&request()), EXHAUSTED_ANSWER);
    }

    #[test]
    fn a_constant_prompter_ignores_the_queue() {
        let prompter = FakePrompter::always(Answer::NoTty);

        assert_eq!(prompter.confirm(&request()), Answer::NoTty);
        assert_eq!(prompter.confirm_literal(&request(), "PURGE"), Answer::NoTty);
    }

    #[test]
    fn records_requests_and_the_literals_demanded() {
        let prompter = FakePrompter::scripted(&[Answer::Yes, Answer::Yes]);

        let _ = prompter.confirm(&request());
        let _ = prompter.confirm_literal(&request(), "PURGE");

        assert_eq!(prompter.requests(), vec![request(), request()]);
        assert_eq!(prompter.literals(), vec!["PURGE".to_owned()]);
        assert_eq!(prompter.prompts()[0].expected_literal, None);
    }
}
