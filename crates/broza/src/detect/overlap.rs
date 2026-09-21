//! Crediting a path to one finding when two of them claim it.
//!
//! A `__pycache__` inside a `target/` inside `~/Library/Caches/x` is three
//! findings' worth of the same bytes. Summing them promises space that exists
//! once (`AGENTS.md` §2.7, "honest numbers"), so a path that lies under another
//! finding's path is dropped from its finding and the finding's numbers are
//! recomputed. The ancestor always wins: removing it removes the descendant
//! anyway. Two findings naming the *same* path keep it in the first, in the
//! category-then-id order the registry established.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::model::{Diagnostic, Finding, FindingPath};

/// Warning code for a finding that could not be rebuilt after narrowing.
pub const FINDING_DROPPED_CODE: &str = "finding_dropped";

/// Drop every path that another finding already covers, recompute the totals.
///
/// Findings without paths (inform-only prose, snapshots) pass through
/// unchanged. A finding left with no path is removed, since it would otherwise
/// promise zero bytes for nothing.
pub fn credit_once(findings: Vec<Finding>) -> (Vec<Finding>, Vec<Diagnostic>) {
    let claimed: BTreeSet<PathBuf> =
        findings.iter().flat_map(|finding| finding.paths().iter().map(|p| p.path.clone())).collect();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut kept = Vec::with_capacity(findings.len());
    let mut warnings = Vec::new();
    for finding in findings {
        if finding.paths().is_empty() {
            kept.push(finding);
            continue;
        }
        let paths: Vec<FindingPath> = finding
            .paths()
            .iter()
            .filter(|path| !covered_by_ancestor(&path.path, &claimed) && seen.insert(path.path.clone()))
            .cloned()
            .collect();
        if paths.is_empty() {
            continue;
        }
        if paths.len() == finding.paths().len() {
            kept.push(finding);
            continue;
        }
        match finding.with_paths(paths) {
            Ok(narrowed) => kept.push(narrowed),
            Err(error) => warnings.push(Diagnostic {
                code: FINDING_DROPPED_CODE.to_owned(),
                message: format!("a finding could not be narrowed and was dropped: {error}"),
                path: None,
            }),
        }
    }
    (kept, warnings)
}

/// `true` when a strict ancestor of `path` is itself a claimed path.
fn covered_by_ancestor(path: &Path, claimed: &BTreeSet<PathBuf>) -> bool {
    path.ancestors().skip(1).any(|ancestor| claimed.contains(ancestor))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use crate::model::Category;

    use super::*;

    fn finding(id: &str, category: Category, paths: &[(&str, u64)]) -> Finding {
        let paths: Vec<FindingPath> = paths
            .iter()
            .map(|(path, size)| FindingPath { path: PathBuf::from(path), size_bytes: *size, last_used: None })
            .collect();
        let bytes = paths.iter().map(|p| p.size_bytes).sum();
        Finding::builder(id.parse().unwrap(), category, id)
            .reclaimable_bytes(bytes)
            .item_count(paths.len() as u64)
            .paths(paths)
            .build()
            .unwrap()
    }

    #[test]
    fn a_path_under_another_findings_path_is_credited_to_the_ancestor() {
        let findings = vec![
            finding("user-cache.library-caches", Category::UserCache, &[("/h/Library/Caches/ts", 100)]),
            finding(
                "build-cache.orphan-node-modules",
                Category::BuildCache,
                &[("/h/Library/Caches/ts/node_modules", 60), ("/h/code/old/node_modules", 40)],
            ),
        ];

        let (kept, warnings) = credit_once(findings);

        assert!(warnings.is_empty());
        assert_eq!(kept[0].reclaimable_bytes(), 100);
        assert_eq!(kept[1].reclaimable_bytes(), 40, "the nested node_modules is the cache's bytes");
        assert_eq!(kept[1].item_count(), Some(1));
        assert_eq!(kept[1].paths()[0].path, PathBuf::from("/h/code/old/node_modules"));
    }

    #[test]
    fn a_finding_whose_every_path_is_covered_disappears() {
        let findings = vec![
            finding("build-cache.cargo-target", Category::BuildCache, &[("/h/code/r/target", 500)]),
            finding("build-cache.pycache", Category::BuildCache, &[("/h/code/r/target/x/__pycache__", 5)]),
        ];

        let (kept, _) = credit_once(findings);

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id().to_string(), "build-cache.cargo-target");
    }

    #[test]
    fn the_same_path_in_two_findings_stays_with_the_first() {
        let findings = vec![
            finding("user-cache.a", Category::UserCache, &[("/h/x", 7)]),
            finding("build-cache.b", Category::BuildCache, &[("/h/x", 7), ("/h/y", 1)]),
        ];

        let (kept, _) = credit_once(findings);

        assert_eq!(kept[0].reclaimable_bytes(), 7);
        assert_eq!(kept[1].reclaimable_bytes(), 1);
    }

    #[test]
    fn disjoint_findings_and_pathless_ones_pass_through_untouched() {
        let disjoint = vec![
            finding("user-cache.a", Category::UserCache, &[("/h/a", 1)]),
            finding("build-cache.b", Category::BuildCache, &[("/h/b", 2)]),
        ];

        let (kept, warnings) = credit_once(disjoint.clone());

        assert_eq!(kept, disjoint);
        assert!(warnings.is_empty());
    }
}
