//! `build-cache`: what a build tool produces and can produce again
//! (`docs/cli-spec.md` §3.3, category notes).
//!
//! Findings, each from the shared home walk:
//!
//! - `build-cache.xcode-deriveddata` — `~/Library/Developer/Xcode/DerivedData/*`, green;
//! - `build-cache.xcode-archives` — `~/Library/Developer/Xcode/Archives/*`, amber
//!   (an archive is a shipped build, not a scratch file);
//! - `build-cache.orphan-node-modules` — leaf `node_modules` whose parent has no
//!   `package.json`, or whose parent has not changed for `--unused-after`, amber;
//! - `build-cache.pycache` — every `__pycache__`, green;
//! - `build-cache.gradle-caches` — `~/.gradle/caches`, green;
//! - `build-cache.cargo-target` — `target/` beside a `Cargo.toml`, amber;
//! - `build-cache.docker-raw` — the Docker Desktop virtual disk, **inform only**
//!   with its allocated size; Broza never talks to the Docker daemon.

use std::path::Path;

use crate::BrozaError;
use crate::model::{Action, Category, Finding, Instructions, Risk};
use crate::scan::DirNode;

use super::support::{finish, path_of, path_with, start};
use crate::detect::detector::{DetectContext, Detector};

/// Where Docker Desktop keeps its virtual disk, relative to the home.
const DOCKER_RAW: &str = "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw";

/// The `build-cache` detector.
pub struct BuildCache;

impl Detector for BuildCache {
    fn category(&self) -> Category {
        Category::BuildCache
    }

    fn detect(&self, context: &DetectContext<'_>) -> Result<Vec<Finding>, BrozaError> {
        let mut findings = Vec::new();
        findings.extend(children_finding(
            context,
            "Library/Developer/Xcode/DerivedData",
            "xcode-deriveddata",
            "Xcode DerivedData",
            "Build products and indexes Xcode regenerates on the next build.",
            Risk::Green,
        )?);
        findings.extend(children_finding(
            context,
            "Library/Developer/Xcode/Archives",
            "xcode-archives",
            "Xcode Archives",
            "Archived builds kept for distribution; each one is a build you may still need to ship.",
            Risk::Amber,
        )?);
        findings.extend(orphan_node_modules(context)?);
        findings.extend(named_dirs(
            context,
            "__pycache__",
            "pycache",
            "Python bytecode caches",
            Risk::Green,
        )?);
        findings.extend(gradle_caches(context)?);
        findings.extend(cargo_targets(context)?);
        findings.extend(docker_raw(context)?);
        Ok(findings)
    }
}

/// One finding over the direct children of `~/<relative>`.
fn children_finding(
    context: &DetectContext<'_>,
    relative: &str,
    detector: &str,
    title: &str,
    description: &str,
    risk: Risk,
) -> Result<Option<Finding>, BrozaError> {
    let root = context.under_home(relative);
    let mut paths: Vec<_> =
        context.children_of(&root).filter(|node| node.allocated_bytes > 0).map(path_of).collect();
    paths.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then_with(|| a.path.cmp(&b.path)));
    let builder = start(Category::BuildCache, detector, title)?
        .description(description)
        .risk(risk)
        .reasoning(format!("{title} under {} is rebuilt by Xcode when needed.", root.display()));
    finish(builder, paths)
}

/// Every directory called `name`, wherever it is under the home.
fn named_dirs(
    context: &DetectContext<'_>,
    name: &str,
    detector: &str,
    title: &str,
    risk: Risk,
) -> Result<Option<Finding>, BrozaError> {
    let mut paths: Vec<_> =
        context.nodes_named(name).filter(|node| node.allocated_bytes > 0).map(path_of).collect();
    paths.sort_by(|a, b| a.path.cmp(&b.path));
    let builder = start(Category::BuildCache, detector, title)?
        .description(format!("`{name}` directories, regenerated the next time the code runs."))
        .risk(risk);
    finish(builder, paths)
}

/// Leaf `node_modules` directories nobody is using.
///
/// Orphan means: the parent has no `package.json`, or the parent has not changed
/// for `--unused-after`. Nested `node_modules` (inside another `node_modules`)
/// are never listed on their own: their parent's line covers them.
fn orphan_node_modules(context: &DetectContext<'_>) -> Result<Option<Finding>, BrozaError> {
    let mut paths = Vec::new();
    for node in context.nodes_named("node_modules") {
        if is_nested_node_modules(&node.path) || node.allocated_bytes == 0 {
            continue;
        }
        let Some(parent) = node.path.parent() else { continue };
        let has_manifest = context.fs.exists(&parent.join("package.json"));
        let parent_idle =
            context.node(parent).and_then(|p| p.mtime).is_some_and(|m| context.is_unused_since(m));
        if !has_manifest || parent_idle {
            paths.push(path_of(node));
        }
    }
    paths.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then_with(|| a.path.cmp(&b.path)));
    let builder = start(Category::BuildCache, "orphan-node-modules", "Orphan node_modules")?
        .description("Dependency trees whose project is gone or has not changed in a long time.")
        .risk(Risk::Amber)
        .reasoning("`npm install` recreates them; a project that has not changed since --unused-after is unlikely to need them soon.");
    finish(builder, paths)
}

/// `true` when another `node_modules` sits above `path`.
fn is_nested_node_modules(path: &Path) -> bool {
    path.ancestors().skip(1).any(|ancestor| ancestor.file_name().is_some_and(|n| n == "node_modules"))
}

/// `~/.gradle/caches`, as one line.
fn gradle_caches(context: &DetectContext<'_>) -> Result<Option<Finding>, BrozaError> {
    let root = context.under_home(".gradle/caches");
    let paths: Vec<_> =
        context.node(&root).filter(|node| node.allocated_bytes > 0).map(path_of).into_iter().collect();
    let builder = start(Category::BuildCache, "gradle-caches", "Gradle caches")?
        .description("Downloaded dependencies and build outputs Gradle fetches again on demand.")
        .risk(Risk::Green);
    finish(builder, paths)
}

/// `target/` directories that belong to a Cargo project.
fn cargo_targets(context: &DetectContext<'_>) -> Result<Option<Finding>, BrozaError> {
    let mut paths: Vec<_> = context
        .nodes_named("target")
        .filter(|node| node.allocated_bytes > 0)
        .filter(|node| node.path.parent().is_some_and(|p| context.fs.exists(&p.join("Cargo.toml"))))
        .map(path_of)
        .collect();
    paths.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then_with(|| a.path.cmp(&b.path)));
    let builder = start(Category::BuildCache, "cargo-target", "Cargo build directories")?
        .description("`target/` of Rust projects; `cargo build` recreates them.")
        .risk(Risk::Amber)
        .reasoning(
            "Rebuilding a large workspace takes time, so this is review-level even though nothing is lost.",
        );
    finish(builder, paths)
}

/// The Docker Desktop disk image: reported, never touched.
fn docker_raw(context: &DetectContext<'_>) -> Result<Option<Finding>, BrozaError> {
    let path = context.under_home(DOCKER_RAW);
    let Ok(meta) = context.fs.metadata(&path) else { return Ok(None) };
    let builder = start(Category::BuildCache, "docker-raw", "Docker Desktop disk image")?
        .description("The virtual disk every Docker image, container and volume lives in.")
        .risk(Risk::Amber)
        .action(Action::InformOnly)
        .reasoning("Deleting the file would destroy every container; only Docker can shrink it safely.")
        .instructions(Instructions {
            provider: "Docker Desktop".to_owned(),
            summary: "Let Docker reclaim the space it no longer uses.".to_owned(),
            steps: vec![
                "Run `docker system prune` to remove unused images, containers and build cache.".to_owned(),
                "In Docker Desktop, open Settings → Resources → Advanced and lower the disk image size."
                    .to_owned(),
            ],
        });
    finish(builder, vec![path_with(&path, meta.allocated_bytes, meta.modified)])
}

/// A node's allocated size, for tests and callers that want one number.
pub fn allocated(node: &DirNode) -> u64 {
    node.allocated_bytes
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::detect::test_support::{context_over, home};
    use crate::testing::FakeFileOps;
    use jiff::Timestamp;

    fn fs() -> FakeFileOps {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        let h = "/System/Volumes/Data/Users/dana";
        for (path, size) in [
            (format!("{h}/Library/Developer/Xcode/DerivedData/App-abc/Build/x.o"), 90_000_u64),
            (format!("{h}/Library/Developer/Xcode/Archives/2026/App.xcarchive/a"), 20_000),
            (format!("{h}/code/live/package.json"), 100),
            (format!("{h}/code/live/node_modules/left-pad/index.js"), 5000),
            (format!("{h}/code/live/node_modules/left-pad/node_modules/inner/i.js"), 5000),
            (format!("{h}/code/gone/node_modules/x/index.js"), 7000),
            (format!("{h}/code/py/__pycache__/m.pyc"), 3000),
            (format!("{h}/.gradle/caches/jars/a.jar"), 8000),
            (format!("{h}/code/rusty/Cargo.toml"), 50),
            (format!("{h}/code/rusty/target/debug/bin"), 60_000),
            (format!("{h}/code/notrust/target/out"), 60_000),
            (format!("{h}/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw"), 500_000),
        ] {
            fs.add_file(&path, &[]);
            fs.set_size(&path, size);
        }
        fs
    }

    fn ids(findings: &[Finding]) -> Vec<String> {
        findings.iter().map(|f| f.id().to_string()).collect()
    }

    fn by_id<'a>(findings: &'a [Finding], id: &str) -> &'a Finding {
        findings
            .iter()
            .find(|f| f.id().to_string() == id)
            .unwrap_or_else(|| panic!("{id} in {:?}", ids(findings)))
    }

    #[test]
    fn every_build_cache_kind_is_found_once() {
        let fs = fs();
        let world = context_over(&fs, home());

        let findings = BuildCache.detect(&world.context()).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(
            ids(&findings),
            vec![
                "build-cache.xcode-deriveddata",
                "build-cache.xcode-archives",
                "build-cache.orphan-node-modules",
                "build-cache.pycache",
                "build-cache.gradle-caches",
                "build-cache.cargo-target",
                "build-cache.docker-raw",
            ]
        );
    }

    #[test]
    fn only_orphan_leaf_node_modules_are_listed() {
        let fs = fs();
        let world = context_over(&fs, home());

        let findings = BuildCache.detect(&world.context()).unwrap_or_else(|e| panic!("{e}"));

        let orphans = by_id(&findings, "build-cache.orphan-node-modules");
        let paths: Vec<String> = orphans.paths().iter().map(|p| p.path.display().to_string()).collect();
        assert_eq!(paths, vec!["/System/Volumes/Data/Users/dana/code/gone/node_modules"]);
        assert_eq!(orphans.risk(), Risk::Amber);
    }

    #[test]
    fn a_live_project_left_untouched_for_the_threshold_becomes_an_orphan() {
        let fs = fs();
        let old = "2020-01-01T00:00:00Z".parse::<Timestamp>().unwrap();
        fs.set_times("/System/Volumes/Data/Users/dana/code/live", old, old);
        let world = context_over(&fs, home());

        let findings = BuildCache.detect(&world.context()).unwrap_or_else(|e| panic!("{e}"));

        let orphans = by_id(&findings, "build-cache.orphan-node-modules");
        assert_eq!(orphans.paths().len(), 2, "{:?}", orphans.paths());
    }

    #[test]
    fn cargo_targets_need_a_cargo_toml_beside_them() {
        let fs = fs();
        let world = context_over(&fs, home());

        let findings = BuildCache.detect(&world.context()).unwrap_or_else(|e| panic!("{e}"));

        let targets = by_id(&findings, "build-cache.cargo-target");
        assert_eq!(targets.paths().len(), 1);
        assert!(targets.paths()[0].path.ends_with("rusty/target"));
    }

    #[test]
    fn the_docker_disk_is_inform_only_with_its_allocated_size() {
        let fs = fs();
        let world = context_over(&fs, home());

        let findings = BuildCache.detect(&world.context()).unwrap_or_else(|e| panic!("{e}"));

        let docker = by_id(&findings, "build-cache.docker-raw");
        assert_eq!(docker.action(), Action::InformOnly);
        assert!(!docker.is_actionable());
        assert!(docker.instructions().is_some());
        assert_eq!(docker.reclaimable_bytes(), 500_000_u64.div_ceil(4096) * 4096);
    }
}
