//! Detection: what Broza can find, and what it can say about it.
//!
//! [`Detector`]s look at the machine and return [`Finding`](crate::model::Finding)s;
//! the [`Registry`] runs them and folds failures into warnings; [`filter`]
//! narrows the result to what `suggest` asked for. Nothing here writes: a finding
//! is a candidate the safety kernel checks again before anything moves. The
//! prose side — what `broza explain` prints for a volume, a category or a path —
//! lives in [`explain`] (`docs/cli-spec.md` §4.7).

mod category_text;
pub mod detector;
pub mod detectors;
pub mod explain;
pub mod filter;
pub mod registry;
#[cfg(test)]
pub(crate) mod test_support;

pub use detector::{DetectContext, Detector};
pub use explain::{
    ExplainKind, ExplainReport, Explanation, category_summary, explain_category, explain_volume,
};
pub use filter::RiskFilter;
pub use registry::{DETECTOR_FAILED_CODE, DetectReport, Registry};
