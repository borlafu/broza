//! Broza command-line interface: argument parsing, rendering, TTY prompting.

use std::ffi::OsString;

use broza::ExitCode;

/// Parse `args` and run the requested command, returning the process exit code.
pub fn run<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let _ = args.into_iter().count();
    ExitCode::Ok
}
