//! Detection: what Broza can find, and what it can say about it.
//!
//! The detectors themselves land in M3 (`docs/implementation-plan.md` §4). What
//! exists today is the prose side of the module: the paragraphs `broza explain`
//! prints for a volume, a category or a path, and the payload shape that
//! carries them into the JSON contract (`docs/cli-spec.md` §4.7).

mod category_text;
pub mod explain;

pub use explain::{
    ExplainKind, ExplainReport, Explanation, category_summary, explain_category, explain_volume,
};
