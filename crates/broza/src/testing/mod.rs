//! Test doubles for every port in [`crate::ports`].
//!
//! Compiled for this crate's own unit tests and, for downstream crates, behind the
//! `test-support` feature. No test may touch the real `$HOME`, real disks, or spawn
//! `diskutil` (`AGENTS.md` §7); these fakes are how that rule is kept.

pub mod fake_fs;
pub mod fake_prompter;
pub mod fake_runner;
mod fake_tree;
pub mod fixed_clock;

pub use fake_fs::FakeFileOps;
pub use fake_prompter::{FakePrompter, RecordedPrompt};
pub use fake_runner::{FakeRunner, RecordedCall};
pub use fixed_clock::FixedClock;
