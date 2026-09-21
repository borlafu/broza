//! Running every detector and folding what they return into one report.
//!
//! Detectors run in parallel; a detector that fails becomes a warning in the
//! report, never an abort (`AGENTS.md` §6). The order of the findings is fixed
//! by category and then by id, so two runs over one machine read the same.

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::model::{Category, Diagnostic, Finding};

use super::detector::{DetectContext, Detector};
use super::detectors;

/// Warning code for a detector that could not run.
pub const DETECTOR_FAILED_CODE: &str = "detector_failed";

/// The detectors Broza ships, in category order.
pub struct Registry {
    detectors: Vec<Box<dyn Detector>>,
}

/// What a detection run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectReport {
    /// Every finding, in category then id order.
    pub findings: Vec<Finding>,
    /// Detectors that could not run, one warning each.
    pub warnings: Vec<Diagnostic>,
}

impl Registry {
    /// Every built-in detector.
    pub fn builtin() -> Self {
        Self { detectors: detectors::builtin() }
    }

    /// A registry of exactly these detectors (for tests and for `--category`).
    pub fn of(detectors: Vec<Box<dyn Detector>>) -> Self {
        Self { detectors }
    }

    /// Keep only the detectors of `categories`; `None` keeps them all.
    #[must_use]
    pub fn restricted_to(self, categories: Option<&[Category]>) -> Self {
        let Some(wanted) = categories else { return self };
        Self { detectors: self.detectors.into_iter().filter(|d| wanted.contains(&d.category())).collect() }
    }

    /// The categories this registry covers.
    pub fn categories(&self) -> Vec<Category> {
        self.detectors.iter().map(|d| d.category()).collect()
    }

    /// Run every detector against `context`.
    pub fn run(&self, context: &DetectContext<'_>) -> DetectReport {
        let outcomes: Vec<Result<Vec<Finding>, Diagnostic>> = self
            .detectors
            .par_iter()
            .map(|detector| detector.detect(context).map_err(|error| failed(detector.category(), &error)))
            .collect();
        let mut findings = Vec::new();
        let mut warnings = Vec::new();
        for outcome in outcomes {
            match outcome {
                Ok(found) => findings.extend(found),
                Err(warning) => warnings.push(warning),
            }
        }
        findings.sort_by(|a, b| a.category().cmp(&b.category()).then_with(|| a.id().cmp(b.id())));
        DetectReport { findings, warnings }
    }
}

/// The warning a detector that could not run leaves behind.
fn failed(category: Category, error: &crate::BrozaError) -> Diagnostic {
    Diagnostic {
        code: DETECTOR_FAILED_CODE.to_owned(),
        message: format!("the {} detector could not run: {error}", category.as_str()),
        path: None,
    }
}
