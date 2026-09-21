//! The three renderings of a `clean` run: human, `--json`; no `--csv` (§1.1).

use std::path::PathBuf;

use broza::BrozaError;
use broza::model::{CleanPlan, Envelope, Host, SessionId, Warning};
use jiff::Timestamp;

use crate::output::human::clean as human_clean;
use crate::output::{Renderer, envelope_to_json};

/// Command name in the JSON envelope.
const COMMAND: &str = "clean";

/// A finished `clean`, ready to render.
#[derive(Debug, Clone)]
pub struct CleanOutput {
    /// The payload of `docs/cli-spec.md` §4.4.
    pub plan: CleanPlan,
    /// Dry run only: sessions `--apply` would expire, with their bytes.
    pub due: Vec<(SessionId, u64)>,
    /// The `broza clean …` line that reproduces the selection, for the footer.
    pub rerun: String,
    /// `errors[]` of the envelope.
    pub errors: Vec<Warning>,
    /// `warnings[]` of the envelope.
    pub warnings: Vec<Warning>,
    /// `host` block of the envelope.
    pub host: Host,
    /// `generated_at` of the envelope.
    pub generated_at: Timestamp,
    /// Home directory, so paths print as `~/…`.
    pub home: PathBuf,
}

impl CleanOutput {
    fn envelope(&self) -> Envelope<CleanPlan> {
        let envelope = Envelope::new(COMMAND, self.host.clone(), self.generated_at, self.plan.clone());
        let envelope = self.warnings.iter().cloned().fold(envelope, Envelope::with_warning);
        self.errors.iter().cloned().fold(envelope, Envelope::with_error)
    }
}

impl Renderer for CleanOutput {
    fn to_human(&self) -> String {
        human_clean::render(&self.plan, &self.due, &self.errors, &self.home, &self.rerun)
    }

    fn to_json(&self) -> Result<String, BrozaError> {
        envelope_to_json(&self.envelope())
    }
}
