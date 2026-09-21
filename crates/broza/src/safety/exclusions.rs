//! Exclusion patterns (`--exclude` plus `exclude` from the configuration).
//!
//! A thin wrapper over [`globset::GlobSet`] that keeps the original patterns, so a
//! set can be compared, printed and reported back to the user. Patterns are matched
//! against the canonical path; `~` must already be expanded by the caller, because
//! the core never reads the environment.

use std::fmt;
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::safety::rejection::GuardRejection;

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
            let glob = Glob::new(pattern).map_err(|error| GuardRejection::InvalidExclusion {
                pattern: pattern.clone(),
                reason: error.to_string(),
            })?;
            Ok::<_, GuardRejection>(clone_with(builder, glob))
        })?;
        let set = builder.build().map_err(|error| GuardRejection::InvalidExclusion {
            pattern: patterns.join(", "),
            reason: error.to_string(),
        })?;
        Ok(Self { set: Some(set), patterns })
    }

    /// `true` when `path` matches at least one pattern.
    pub fn matches(&self, path: &Path) -> bool {
        self.set.as_ref().is_some_and(|set| set.is_match(path))
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

/// `GlobSetBuilder::add` takes `&mut self`; this keeps the call site immutable.
fn clone_with(builder: GlobSetBuilder, glob: Glob) -> GlobSetBuilder {
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
        assert!(!exclusions.matches(Path::new("/Users/dana/anything")));
    }

    #[test]
    fn a_recursive_pattern_matches_nested_paths() {
        let exclusions = set(&["/Users/dana/Projects/**/node_modules/**"]);
        assert!(exclusions.matches(Path::new("/Users/dana/Projects/app/node_modules/left-pad/index.js")));
        assert!(!exclusions.matches(Path::new("/Users/dana/Library/Caches/node_modules")));
    }

    #[test]
    fn several_patterns_are_all_considered() {
        let exclusions = set(&["/Library/Caches/keep/*", "/Users/dana/Library/Caches/com.mycompany.*"]);
        assert!(exclusions.matches(Path::new("/Library/Caches/keep/thing")));
        assert!(exclusions.matches(Path::new("/Users/dana/Library/Caches/com.mycompany.tool")));
        assert!(!exclusions.matches(Path::new("/Users/dana/Library/Caches/com.other.tool")));
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
    fn sets_compare_by_their_patterns() {
        assert_eq!(set(&["/a/*"]), set(&["/a/*"]));
        assert_ne!(set(&["/a/*"]), set(&["/b/*"]));
        assert_eq!(format!("{:?}", set(&["/a/*"])), "Exclusions { patterns: [\"/a/*\"], .. }");
    }
}
