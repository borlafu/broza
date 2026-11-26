//! `broza completions <shell>` and `broza man`: text generated from the CLI
//! definition itself, so it can never describe a flag that does not exist.

use broza::BrozaError;
use clap::CommandFactory as _;

use crate::cli::Cli;
use crate::commands::Outcome;
use crate::output::OutputFormat;

/// Name the scripts and the page are generated for.
const BIN: &str = "broza";

/// The completion script for `shell`.
///
/// # Errors
///
/// [`BrozaError::Usage`] with `--json` or `--csv`: a script has no envelope.
pub fn completions(shell: clap_complete::Shell, format: OutputFormat) -> Result<Outcome, BrozaError> {
    plain_only(format, "completions")?;
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), BIN, &mut script);
    Ok(Outcome::ok(String::from_utf8_lossy(&script).into_owned()))
}

/// The manual page, in roff.
///
/// # Errors
///
/// [`BrozaError::Usage`] with `--json` or `--csv`, and [`BrozaError::Other`]
/// if the page cannot be rendered.
pub fn man(format: OutputFormat) -> Result<Outcome, BrozaError> {
    plain_only(format, "man")?;
    let mut page = Vec::new();
    clap_mangen::Man::new(Cli::command())
        .render(&mut page)
        .map_err(|error| BrozaError::Other(format!("the manual page could not be rendered: {error}")))?;
    Ok(Outcome::ok(String::from_utf8_lossy(&page).into_owned()))
}

fn plain_only(format: OutputFormat, command: &str) -> Result<(), BrozaError> {
    if format.is_machine_readable() {
        return Err(BrozaError::Usage(format!(
            "`{command}` prints text for a shell and has no --json or --csv form"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn every_supported_shell_gets_a_script_that_names_the_commands() {
        for shell in [clap_complete::Shell::Bash, clap_complete::Shell::Zsh, clap_complete::Shell::Fish] {
            let script = completions(shell, OutputFormat::Human).unwrap().rendered;
            assert!(script.contains("broza"), "{shell}");
            assert!(script.contains("suggest") && script.contains("doctor"), "{shell}");
        }
    }

    #[test]
    fn the_manual_page_is_roff_for_broza() {
        let page = man(OutputFormat::Human).unwrap().rendered;

        assert!(page.starts_with(".ie") || page.contains(".TH broza"), "{}", &page[..page.len().min(200)]);
    }

    #[test]
    fn neither_has_a_machine_readable_form() {
        assert!(matches!(
            completions(clap_complete::Shell::Zsh, OutputFormat::Json),
            Err(BrozaError::Usage(_))
        ));
        assert!(matches!(man(OutputFormat::Csv), Err(BrozaError::Usage(_))));
    }
}
