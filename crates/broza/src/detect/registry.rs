//! Running every detector and folding what they return into one report.
//!
//! Detectors run in parallel; a detector that fails becomes a warning in the
//! report, never an abort (`AGENTS.md` §6). The order of the findings is fixed
//! by category and then by id, so two runs over one machine read the same.
//! Paths that two findings both claim are credited once (`AGENTS.md` §2.7):
//! see the private `overlap` module.

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::model::{Category, Diagnostic, Finding};

use super::detector::{DetectContext, Detected, Detector};
use super::{detectors, overlap};

/// Warning code for a detector that could not run.
pub const DETECTOR_FAILED_CODE: &str = "detector_failed";

/// The detectors Broza ships, in category order.
pub struct Registry {
    detectors: Vec<Box<dyn Detector>>,
}

/// What a detection run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectReport {
    /// Every finding, in category then id order, no path claimed twice.
    pub findings: Vec<Finding>,
    /// Detectors that could not run and locations they could not read.
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
        let outcomes: Vec<Detected> = self
            .detectors
            .par_iter()
            .map(|detector| {
                detector.detect(context).unwrap_or_else(|error| Detected {
                    findings: Vec::new(),
                    warnings: vec![failed(detector.category(), &error)],
                })
            })
            .collect();
        let Detected { mut findings, mut warnings } =
            outcomes.into_iter().fold(Detected::default(), Detected::merged);
        findings.sort_by(|a, b| a.category().cmp(&b.category()).then_with(|| a.id().cmp(b.id())));
        let (findings, overlap_warnings) = overlap::credit_once(findings);
        warnings.extend(overlap_warnings);
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;

    use crate::BrozaError;
    use crate::detect::test_support::{context_over, home};
    use crate::model::FindingPath;
    use crate::testing::FakeFileOps;

    use super::*;

    /// A detector that returns fixed findings, or fails, on demand.
    struct Stub {
        category: Category,
        ids: Vec<&'static str>,
        fails: bool,
    }

    impl Detector for Stub {
        fn category(&self) -> Category {
            self.category
        }

        fn detect(&self, _: &DetectContext<'_>) -> Result<Detected, BrozaError> {
            if self.fails {
                return Err(BrozaError::Other("boom".into()));
            }
            let findings =
                self.ids.iter().map(|id| finding(id, self.category, &format!("/x/{id}"))).collect();
            Ok(Detected { findings, warnings: Vec::new() })
        }
    }

    fn finding(id: &str, category: Category, path: &str) -> Finding {
        Finding::builder(id.parse().unwrap(), category, id)
            .paths(vec![FindingPath { path: PathBuf::from(path), size_bytes: 10, last_used: None }])
            .reclaimable_bytes(10)
            .item_count(1)
            .build()
            .unwrap()
    }

    fn stub(category: Category, ids: &[&'static str]) -> Box<dyn Detector> {
        Box::new(Stub { category, ids: ids.to_vec(), fails: false })
    }

    fn ids(report: &DetectReport) -> Vec<String> {
        report.findings.iter().map(|f| f.id().to_string()).collect()
    }

    #[test]
    fn a_failing_detector_becomes_a_warning_and_the_others_still_report() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_dir(home());
        let world = context_over(&fs, home());
        let registry = Registry::of(vec![
            Box::new(Stub { category: Category::Trash, ids: vec!["trash.bins"], fails: true }),
            stub(Category::UserCache, &["user-cache.logs"]),
        ]);

        let report = registry.run(&world.context());

        assert_eq!(ids(&report), vec!["user-cache.logs"]);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].code, DETECTOR_FAILED_CODE);
        assert!(report.warnings[0].message.contains("the trash detector could not run: boom"));
    }

    #[test]
    fn findings_are_ordered_by_category_then_id_whatever_the_registration_order() {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        fs.add_dir(home());
        let world = context_over(&fs, home());
        let registry = Registry::of(vec![
            stub(Category::Trash, &["trash.z", "trash.a"]),
            stub(Category::UserCache, &["user-cache.logs", "user-cache.caches"]),
        ]);

        let report = registry.run(&world.context());

        assert_eq!(ids(&report), vec!["user-cache.caches", "user-cache.logs", "trash.a", "trash.z"]);
        assert!(report.warnings.is_empty());
        assert!(report.findings.iter().all(|f| f.risk() == f.category().base_risk()));
    }

    #[test]
    fn restricting_keeps_only_the_categories_asked_for() {
        let registry = Registry::of(vec![
            stub(Category::UserCache, &["user-cache.logs"]),
            stub(Category::BuildCache, &["build-cache.pycache"]),
            stub(Category::Trash, &["trash.bins"]),
        ]);

        let all = Registry::of(vec![stub(Category::Trash, &[])]).restricted_to(None).categories();
        let some = registry.restricted_to(Some(&[Category::Trash, Category::UserCache])).categories();

        assert_eq!(all, vec![Category::Trash]);
        assert_eq!(some, vec![Category::UserCache, Category::Trash]);
    }
}
