//! Re-checking a path inside the store against the token that approved it.
//!
//! `restore`, `expire` and `purge` all write inside the quarantine store, and
//! all three hold an
//! [`Approved<QuarantineWrite>`](crate::safety::guard::QuarantineWrite) listing
//! the paths the guard validated. Holding the token is not enough: the path must
//! still be the object the guard saw, so the `(device, inode)` pair is compared
//! again immediately before the write (`AGENTS.md` §4).

use std::path::Path;

use crate::BrozaError;
use crate::model::ItemErrorCode;
use crate::ports::{EntryMetadata, FileOps};
use crate::quarantine::codes::changed_since_check;
use crate::safety::guard::ApprovedItem;

/// Re-`lstat` an approved item and compare its identity with the token's.
///
/// The one predicate that stands between a token and a write: every mutating
/// path (move, purge, restore) calls this right before acting, so a path that
/// was replaced since the guard looked at it is refused as
/// `changed_since_check`, and a path that cannot be read carries the
/// filesystem's own reason.
///
/// # Errors
///
/// The item error code to record against the item.
pub fn recheck_identity(item: &ApprovedItem, fs: &dyn FileOps) -> Result<EntryMetadata, ItemErrorCode> {
    let current = fs.metadata(item.path()).map_err(|error| io_code(&error))?;
    if current.device != item.device() || current.inode != item.inode() {
        return Err(changed_since_check());
    }
    Ok(current)
}

/// What a re-check concluded about one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recheck {
    /// The path is covered by the token and is still the same object.
    Unchanged,
    /// The path must be left alone; record this code against the item.
    Refused(ItemErrorCode),
}

/// Re-`lstat` `path` and compare it with the evidence the token carries.
///
/// # Errors
///
/// [`BrozaError::Other`] when the token does not cover `path` at all. That is a
/// caller bug rather than an item failure: writing to a path nobody approved is
/// exactly what the token exists to prevent, so it stops the operation instead
/// of being recorded as a skipped item.
pub fn recheck(items: &[ApprovedItem], path: &Path, fs: &dyn FileOps) -> Result<Recheck, BrozaError> {
    let approved = items.iter().find(|item| item.path() == path).ok_or_else(|| {
        BrozaError::Other(format!(
            "quarantine write: `{}` is not one of the paths the guard approved",
            path.display()
        ))
    })?;
    let current = match fs.metadata(path) {
        Ok(current) => current,
        Err(error) => return Ok(Recheck::Refused(io_code(&error))),
    };
    if current.device != approved.device() || current.inode != approved.inode() {
        return Ok(Recheck::Refused(changed_since_check()));
    }
    Ok(Recheck::Unchanged)
}

/// The item error code of `docs/cli-spec.md` §4.1 for an I/O failure.
pub fn io_code(error: &BrozaError) -> ItemErrorCode {
    match error {
        BrozaError::PermissionDenied { .. } => ItemErrorCode::PermissionDenied,
        BrozaError::TargetNotFound(_) => ItemErrorCode::NotFound,
        _ => ItemErrorCode::IoError,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Recheck, io_code, recheck};
    use crate::BrozaError;
    use crate::model::ItemErrorCode;
    use crate::ports::FileOps;
    use crate::quarantine::codes::changed_since_check;
    use crate::quarantine::fixtures::{ROOT, store_fs};
    use crate::safety::guard::{Approved, ApprovedItem, QuarantineWrite, approve_quarantine_write};
    use crate::testing::{FakeFileOps, mac_mount_table};

    const STORED: &str = "/Users/dana/.local/share/broza/quarantine/cln_20260921103608_a1b2/items/0001/a";

    fn tree() -> FakeFileOps {
        store_fs().with_sized_file(STORED, 10)
    }

    fn token(fs: &FakeFileOps) -> Approved<QuarantineWrite> {
        approve_quarantine_write(&[STORED.into()], Path::new(ROOT), &mac_mount_table(), fs)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn items(token: &Approved<QuarantineWrite>) -> Vec<ApprovedItem> {
        token.items().to_vec()
    }

    #[test]
    fn an_unchanged_path_is_approved() {
        let fs = tree();

        assert_eq!(recheck(&items(&token(&fs)), Path::new(STORED), &fs).ok(), Some(Recheck::Unchanged));
    }

    #[test]
    fn a_path_the_token_does_not_cover_stops_the_operation() {
        let fs = tree();
        let elsewhere = Path::new("/Users/dana/Documents/report.pdf");

        let error = recheck(&items(&token(&fs)), elsewhere, &fs);

        assert!(matches!(error, Err(BrozaError::Other(_))), "{error:?}");
    }

    #[test]
    fn a_path_that_was_replaced_is_refused() {
        let fs = tree();
        let approved = items(&token(&fs));
        fs.remove_tree(Path::new(STORED)).unwrap_or_else(|error| panic!("{error}"));
        fs.add_file(STORED, b"an impostor");

        let verdict = recheck(&approved, Path::new(STORED), &fs);

        assert_eq!(verdict.ok(), Some(Recheck::Refused(changed_since_check())));
    }

    #[test]
    fn a_path_that_vanished_is_refused_as_missing() {
        let fs = tree();
        let approved = items(&token(&fs));
        fs.remove_tree(Path::new(STORED)).unwrap_or_else(|error| panic!("{error}"));

        let verdict = recheck(&approved, Path::new(STORED), &fs);

        assert_eq!(verdict.ok(), Some(Recheck::Refused(ItemErrorCode::NotFound)));
    }

    #[test]
    fn an_io_failure_keeps_the_reason_the_filesystem_gave() {
        assert_eq!(
            io_code(&BrozaError::PermissionDenied { path: "/x".into() }),
            ItemErrorCode::PermissionDenied
        );
        assert_eq!(io_code(&BrozaError::TargetNotFound("/x".to_owned())), ItemErrorCode::NotFound);
        assert_eq!(io_code(&BrozaError::Other("boom".to_owned())), ItemErrorCode::IoError);
    }
}
