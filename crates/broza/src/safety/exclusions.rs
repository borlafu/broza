//! Exclusion patterns (`--exclude` plus `exclude` from the configuration).
//!
//! A thin wrapper over [`globset::GlobSet`] that keeps the original patterns, so a
//! set can be compared, printed and reported back to the user.
//!
//! Two rules make an exclusion mean what the user expects on macOS:
//!
//! - matching is **case-insensitive**, like the default APFS volume, so
//!   `~/Library/Caches/*` also protects `~/library/caches/x`;
//! - a path is matched under **both firmlink spellings**, so a pattern written as
//!   `/Users/dana/**` also protects `/System/Volumes/Data/Users/dana/...`.
//!
//! Patterns must be absolute; `~` is expanded by the caller, because the core
//! never reads the environment.

use std::fmt;
use std::path::Path;

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

use crate::safety::rejection::GuardRejection;
use crate::safety::roots::firmlink_spellings;

/// A compiled, immutable set of exclusion globs.
#[derive(Clone, Default)]
pub struct Exclusions {
    set: Option<GlobSet>,
    patterns: Vec<String>,
}

impl Exclusions {
    /// A set that excludes nothing.
    pub fn none() -> Self {
        Self::default()
    }

    /// Compiles the patterns, keeping their order.
    ///
    /// Every pattern must be absolute: a relative one would silently match
    /// nothing and give the user a false sense of protection.
    pub fn new<I, S>(patterns: I) -> Result<Self, GuardRejection>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let patterns: Vec<String> = patterns.into_iter().map(|p| p.as_ref().to_owned()).collect();
        if patterns.is_empty() {
            return Ok(Self::none());
        }
        let builder = patterns.iter().try_fold(GlobSetBuilder::new(), |builder, pattern| {
            Ok::<_, GuardRejection>(add(builder, compile(pattern)?))
        })?;
        let set = builder.build().map_err(|error| GuardRejection::InvalidExclusion {
            pattern: patterns.join(", "),
            reason: error.to_string(),
        })?;
        Ok(Self { set: Some(set), patterns })
    }

    /// `true` when `path` matches at least one pattern.
    ///
    /// On the Data volume a path has two spellings for the same directory and both
    /// are tried; anywhere else `/System/Volumes/Data/...` would be a different
    /// place, so only the path as given is matched.
    pub fn matches(&self, path: &Path, on_data_volume: bool) -> bool {
        let Some(set) = self.set.as_ref() else {
            return false;
        };
        if !on_data_volume {
            return set.is_match(path);
        }
        firmlink_spellings(path).iter().any(|spelling| set.is_match(spelling))
    }

    /// The patterns, in the order they were given.
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// `true` when nothing is excluded.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
}

/// One pattern, compiled with the matching rules of a default macOS volume.
fn compile(pattern: &str) -> Result<Glob, GuardRejection> {
    let invalid = |reason: &str| GuardRejection::InvalidExclusion {
        pattern: pattern.to_owned(),
        reason: reason.to_owned(),
    };
    if !Path::new(pattern).is_absolute() {
        return Err(invalid("an exclusion must be an absolute path pattern"));
    }
    GlobBuilder::new(pattern)
        .case_insensitive(true)
        .literal_separator(false)
        .build()
        .map_err(|error| invalid(&error.to_string()))
}

/// `GlobSetBuilder::add` takes `&mut self`; this keeps the call site immutable.
fn add(builder: GlobSetBuilder, glob: Glob) -> GlobSetBuilder {
    let mut builder = builder;
    builder.add(glob);
    builder
}

impl fmt::Debug for Exclusions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Exclusions").field("patterns", &self.patterns).finish_non_exhaustive()
    }
}

impl PartialEq for Exclusions {
    fn eq(&self, other: &Self) -> bool {
        self.patterns == other.patterns
    }
}

impl Eq for Exclusions {}

#[cfg(test)]
mod tests {
    use super::Exclusions;
    use crate::safety::rejection::GuardRejection;
    use std::path::Path;

    fn set(patterns: &[&str]) -> Exclusions {
        Exclusions::new(patterns).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn an_empty_set_matches_nothing() {
        let exclusions = Exclusions::none();
        assert!(exclusions.is_empty());
        assert!(!exclusions.matches(Path::new("/Users/dana/anything"), true));
    }

    #[test]
    fn a_recursive_pattern_matches_nested_paths() {
        let exclusions = set(&["/Users/dana/Projects/**/node_modules/**"]);
        assert!(
            exclusions.matches(Path::new("/Users/dana/Projects/app/node_modules/left-pad/index.js"), true)
        );
        assert!(!exclusions.matches(Path::new("/Users/dana/Library/Caches/node_modules"), true));
    }

    #[test]
    fn an_exclusion_protects_both_firmlink_spellings() {
        let exclusions = set(&["/Users/dana/Library/Caches/**"]);
        assert!(exclusions.matches(Path::new("/Users/dana/Library/Caches/app.cache"), true));
        assert!(
            exclusions.matches(Path::new("/System/Volumes/Data/Users/dana/Library/Caches/app.cache"), true),
            "the Data-volume spelling is the same directory"
        );
    }

    #[test]
    fn a_pattern_written_on_the_data_volume_protects_the_short_spelling() {
        let exclusions = set(&["/System/Volumes/Data/Users/dana/Library/Caches/**"]);
        assert!(exclusions.matches(Path::new("/System/Volumes/Data/Users/dana/Library/Caches/a"), true));
        assert!(exclusions.matches(Path::new("/Users/dana/Library/Caches/a"), true));
    }

    #[test]
    fn matching_ignores_case_like_the_default_volume() {
        let exclusions = set(&["/Users/dana/Library/Caches/**"]);
        assert!(exclusions.matches(Path::new("/users/dana/library/caches/app.cache"), true));
    }

    #[test]
    fn several_patterns_are_all_considered() {
        let exclusions = set(&["/Library/Caches/keep/*", "/Users/dana/Library/Caches/com.mycompany.*"]);
        assert!(exclusions.matches(Path::new("/Library/Caches/keep/thing"), true));
        assert!(exclusions.matches(Path::new("/Users/dana/Library/Caches/com.mycompany.tool"), true));
        assert!(!exclusions.matches(Path::new("/Users/dana/Library/Caches/com.other.tool"), true));
        assert_eq!(exclusions.patterns().len(), 2);
    }

    #[test]
    fn an_invalid_glob_is_reported_with_its_pattern() {
        let error = Exclusions::new(["/Users/dana/**/["]);
        assert!(
            matches!(error, Err(GuardRejection::InvalidExclusion { ref pattern, .. }) if pattern == "/Users/dana/**/["),
            "{error:?}"
        );
    }

    #[test]
    fn a_relative_pattern_is_refused_instead_of_silently_matching_nothing() {
        let error = Exclusions::new(["~/Library/Caches/**"]);
        assert!(
            matches!(error, Err(GuardRejection::InvalidExclusion { ref reason, .. }) if reason.contains("absolute")),
            "{error:?}"
        );
        assert!(Exclusions::new(["node_modules/**"]).is_err());
    }

    /// Outside the Data volume `/System/Volumes/Data/...` is just another path,
    /// so the twin spelling must not be invented for it.
    #[test]
    fn the_twin_spelling_is_only_tried_on_the_data_volume() {
        let exclusions = set(&["/Users/dana/Library/Caches/**"]);
        let twin = Path::new("/System/Volumes/Data/Users/dana/Library/Caches/app.cache");
        assert!(exclusions.matches(twin, true));
        assert!(!exclusions.matches(twin, false));
        let plain = Path::new("/Users/dana/Library/Caches/app.cache");
        assert!(exclusions.matches(plain, false), "the path as written always counts");
    }

    #[test]
    fn sets_compare_by_their_patterns() {
        assert_eq!(set(&["/a/*"]), set(&["/a/*"]));
        assert_ne!(set(&["/a/*"]), set(&["/b/*"]));
        assert_eq!(format!("{:?}", set(&["/a/*"])), "Exclusions { patterns: [\"/a/*\"], .. }");
    }
}
