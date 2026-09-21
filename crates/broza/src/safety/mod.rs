//! Safety kernel: exit codes, confirmation policy, and the `Approved` write token.
//!
//! Every function that mutates the disk requires an `Approved<_>` token that only
//! this module can construct. See `docs/adr/0003-approved-token-safety-kernel.md`.

pub mod exit_code;
