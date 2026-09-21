//! The two verdicts that write nothing, and the check that the quarantine store
//! is a place Broza may write at all.

use std::path::{Path, PathBuf};

use super::item::resolve_volume;
use super::{Verdict, WriteRequest};
use crate::model::{CleanItem, CleanPlan, CleanPlanRepr};
use crate::ports::FileOps;
use crate::safety::path::{CanonicalPath, canonicalize_no_follow};
use crate::safety::rejection::GuardRejection;
use crate::safety::roots::validate_quarantine_root;
use crate::scan::MountTable;

/// Check 0b: a quarantine store outside the allowlist would be a second, unchecked
/// write target.
///
/// Returns the store as the guard resolved it, so the token can carry the only
/// spelling of it that was actually checked.
pub(super) fn check_quarantine_root(
    req: &WriteRequest,
    mounts: &MountTable,
    fs: &dyn FileOps,
) -> Result<Option<std::path::PathBuf>, GuardRejection> {
    let Some(store_root) = req.quarantine_root.as_ref() else {
        return Ok(None);
    };
    let roots = req.allowed_roots()?;
    let (checked, intended) = canonicalize_store_root(store_root, fs)?;
    let mount = resolve_volume(&checked, mounts)?;
    validate_quarantine_root(&intended, &roots, mount, mounts)?;
    Ok(Some(intended))
}

/// The store root as `lstat` sees it, or — before the first cleanup, when it
/// does not exist yet — its nearest existing ancestor checked in its place.
///
/// The mover creates the store under the root the guard approved, so what has
/// to be safe is the place it will be created in. The components that do not
/// exist yet must be plain names: nothing to follow, nothing to climb.
fn canonicalize_store_root(
    store_root: &Path,
    fs: &dyn FileOps,
) -> Result<(CanonicalPath, PathBuf), GuardRejection> {
    let invalid = |error: &GuardRejection| GuardRejection::InvalidRoot {
        path: store_root.to_path_buf(),
        reason: error.to_string(),
    };
    let mut probe = store_root;
    loop {
        match canonicalize_no_follow(probe, fs) {
            Ok(checked) => {
                // `canonicalize_no_follow` hands back the spelling it was given, so
                // today `checked.path == probe`; the join is kept for a future
                // canonicaliser that rewrites the prefix.
                let intended = match store_root.strip_prefix(probe) {
                    Ok(rest) if !rest.as_os_str().is_empty() => checked.path.join(rest),
                    _ => checked.path.clone(),
                };
                return Ok((checked, intended));
            }
            Err(error) if error.is_missing_path() => {
                let Some(parent) = probe.parent() else { return Err(invalid(&error)) };
                // Defence in depth: the raw-byte check of `canonicalize_no_follow`
                // already refused `.`, `..` and empty components on the first
                // iteration, so a missing component here is always a plain name.
                if !is_plain_name(probe) {
                    return Err(invalid(&error));
                }
                probe = parent;
            }
            Err(error) => return Err(invalid(&error)),
        }
    }
}

/// `true` when the final component of `path` is an ordinary name.
fn is_plain_name(path: &Path) -> bool {
    matches!(path.components().next_back(), Some(std::path::Component::Normal(_)))
}

/// Check 1: without `--apply` the plan must already be, and stay, a dry run.
pub(super) fn dry_run(plan: &CleanPlan, req: &WriteRequest) -> Result<Verdict, GuardRejection> {
    if !plan.is_dry_run() {
        return Err(GuardRejection::Inconsistent(format!(
            "clean plan `{}` is marked applied but `--apply` was not given",
            plan.session_id()
        )));
    }
    if plan.items().is_empty() {
        return nothing(plan, req);
    }
    Ok(Verdict::DryRun(plan.clone()))
}

/// Nothing to write: the plan is rebuilt with every counter at zero, so a caller
/// cannot have the report echo bytes it invented.
pub(super) fn nothing(plan: &CleanPlan, req: &WriteRequest) -> Result<Verdict, GuardRejection> {
    let repr = CleanPlanRepr {
        dry_run: !req.apply,
        session_id: plan.session_id().clone(),
        planned_bytes: 0,
        quarantined_bytes: 0,
        reclaimed_bytes: 0,
        quarantine_path: None,
        expired_sessions: Vec::new(),
        items: plan.items().iter().map(|item| CleanItem { size_bytes: 0, ..item.clone() }).collect(),
    };
    CleanPlan::new(repr)
        .map(Verdict::Nothing)
        .map_err(|error| GuardRejection::Inconsistent(format!("empty plan: {error}")))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::canonicalize_store_root;
    use crate::testing::FakeFileOps;

    #[test]
    fn a_store_that_does_not_exist_yet_is_checked_at_its_nearest_existing_ancestor() {
        let fs = FakeFileOps::new().with_root("/", 1).with_dir("/Users/dana/.local");

        let (checked, intended) =
            canonicalize_store_root(Path::new("/Users/dana/.local/share/broza/quarantine"), &fs)
                .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(checked.path, PathBuf::from("/Users/dana/.local"));
        assert_eq!(intended, PathBuf::from("/Users/dana/.local/share/broza/quarantine"));
    }

    #[test]
    fn an_existing_store_is_checked_as_itself() {
        let fs = FakeFileOps::new().with_root("/", 1).with_dir("/Users/dana/q");

        let (checked, intended) = canonicalize_store_root(Path::new("/Users/dana/q"), &fs)
            .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(checked.path, intended);
    }

    #[test]
    fn a_missing_suffix_with_a_parent_component_is_refused() {
        let fs = FakeFileOps::new().with_root("/", 1).with_dir("/Users/dana");

        let refused = canonicalize_store_root(Path::new("/Users/dana/missing/../q"), &fs);

        assert!(refused.is_err());
    }

    #[test]
    fn a_symlinked_ancestor_is_still_refused() {
        let fs =
            FakeFileOps::new().with_root("/", 1).with_dir("/real").with_symlink("/Users/dana/link", "/real");

        let refused = canonicalize_store_root(Path::new("/Users/dana/link/q"), &fs);

        assert!(refused.is_err());
    }
}
