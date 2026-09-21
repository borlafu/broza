//! `build-cache`: what a build tool produces and can produce again
//! (`docs/cli-spec.md` §3.3, category notes).
//!
//! Findings, each from the shared home walk:
//!
//! - `build-cache.xcode-deriveddata` — `~/Library/Developer/Xcode/DerivedData/*`, green;
//! - `build-cache.xcode-archives` — `~/Library/Developer/Xcode/Archives/*`, amber
//!   (an archive is a shipped build, not a scratch file);
//! - `build-cache.orphan-node-modules` — leaf `node_modules` of a project that
//!   is gone or idle (the rule lives in `node_modules.rs`), amber;
//! - `build-cache.pycache` — every `__pycache__`, green;
//! - `build-cache.gradle-caches` — `~/.gradle/caches`, green;
//! - `build-cache.cargo-target` — `target/` beside a `Cargo.toml`, amber;
//! - `build-cache.docker-raw` — the Docker Desktop virtual disk, **inform only**
//!   with its allocated size; Broza never talks to the Docker daemon.

use crate::BrozaError;
use crate::model::{Action, Category, Finding, Instructions, Risk};

use super::node_modules::orphan_node_modules;
use super::support::{by_size_then_path, finish, path_of, path_with, start};
use crate::detect::detector::{DetectContext, Detected, Detector};

/// Where Docker Desktop keeps its virtual disk, relative to the home.
const DOCKER_RAW: &str = "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw";

/// The `build-cache` detector.
pub struct BuildCache;

impl Detector for BuildCache {
    fn category(&self) -> Category {
        Category::BuildCache
    }

    fn detect(&self, context: &DetectContext<'_>) -> Result<Detected, BrozaError> {
        Ok(Detected::default()
            .with_finding(children_finding(context, &XCODE_DERIVED_DATA)?)
            .with_finding(children_finding(context, &XCODE_ARCHIVES)?)
            .with_finding(orphan_node_modules(context)?)
            .with_finding(named_dirs(
                context,
                "__pycache__",
                "pycache",
                "Python bytecode caches",
                Risk::Green,
            )?)
            .with_finding(gradle_caches(context)?)
            .with_finding(cargo_targets(context)?)
            .with_finding(docker_raw(context)?))
    }
}

/// The words of a finding over the children of one directory.
struct ChildrenSpec {
    relative: &'static str,
    detector: &'static str,
    title: &'static str,
    description: &'static str,
    reasoning: &'static str,
    risk: Risk,
}

const XCODE_DERIVED_DATA: ChildrenSpec = ChildrenSpec {
    relative: "Library/Developer/Xcode/DerivedData",
    detector: "xcode-deriveddata",
    title: "Xcode DerivedData",
    description: "Build products and indexes Xcode regenerates on the next build.",
    reasoning: "Xcode rebuilds DerivedData from the sources the next time a project opens.",
    risk: Risk::Green,
};

const XCODE_ARCHIVES: ChildrenSpec = ChildrenSpec {
    relative: "Library/Developer/Xcode/Archives",
    detector: "xcode-archives",
    title: "Xcode Archives",
    description: "Archived builds kept for distribution; each one is a build you may still need to ship.",
    reasoning: "An archive is a shipped build and its debug symbols; nothing recreates it, so review before quarantining.",
    risk: Risk::Amber,
};

/// One finding over the direct children of `~/<spec.relative>`.
fn children_finding(context: &DetectContext<'_>, spec: &ChildrenSpec) -> Result<Option<Finding>, BrozaError> {
    let root = context.under_home(spec.relative);
    let mut paths: Vec<_> =
        context.children_of(&root).filter(|node| node.allocated_bytes > 0).map(path_of).collect();
    paths.sort_by(by_size_then_path);
    let builder = start(Category::BuildCache, spec.detector, spec.title)?
        .description(spec.description)
        .risk(spec.risk)
        .reasoning(spec.reasoning);
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
    paths.sort_by(by_size_then_path);
    let builder = start(Category::BuildCache, detector, title)?
        .description(format!("`{name}` directories, regenerated the next time the code runs."))
        .risk(risk)
        .reasoning(format!("A `{name}` directory is derived from the code beside it and rebuilt on demand."));
    finish(builder, paths)
}

/// `~/.gradle/caches`, as one line.
fn gradle_caches(context: &DetectContext<'_>) -> Result<Option<Finding>, BrozaError> {
    let root = context.under_home(".gradle/caches");
    let paths: Vec<_> =
        context.node(&root).filter(|node| node.allocated_bytes > 0).map(path_of).into_iter().collect();
    let builder = start(Category::BuildCache, "gradle-caches", "Gradle caches")?
        .description("Downloaded dependencies and build outputs Gradle fetches again on demand.")
        .risk(Risk::Green)
        .reasoning("Gradle downloads what a build needs again; only the first build afterwards is slower.");
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
    paths.sort_by(by_size_then_path);
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::detect::test_support::{context_over, home};
    use crate::testing::FakeFileOps;

    const H: &str = "/System/Volumes/Data/Users/dana";

    fn fs() -> FakeFileOps {
        let fs = FakeFileOps::new().with_root("/System/Volumes/Data", 2);
        for (path, size) in [
            (format!("{H}/Library/Developer/Xcode/DerivedData/App-abc/Build/x.o"), 90_000_u64),
            (format!("{H}/Library/Developer/Xcode/Archives/2026/App.xcarchive/a"), 20_000),
            (format!("{H}/code/live/package.json"), 100),
            (format!("{H}/code/live/node_modules/left-pad/index.js"), 5000),
            (format!("{H}/code/live/node_modules/left-pad/node_modules/inner/i.js"), 5000),
            (format!("{H}/code/gone/node_modules/x/index.js"), 7000),
            (format!("{H}/code/py/__pycache__/m.pyc"), 3000),
            (format!("{H}/.gradle/caches/jars/a.jar"), 8000),
            (format!("{H}/code/rusty/Cargo.toml"), 50),
            (format!("{H}/code/rusty/target/debug/bin"), 60_000),
            (format!("{H}/code/notrust/target/out"), 60_000),
            (format!("{H}/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw"), 500_000),
        ] {
            fs.add_file(&path, &[]);
            fs.set_size(&path, size);
        }
        fs
    }

    fn detect(fs: &FakeFileOps) -> Vec<Finding> {
        let world = context_over(fs, home());
        BuildCache.detect(&world.context()).unwrap_or_else(|e| panic!("{e}")).findings
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

    fn listed(finding: &Finding) -> Vec<String> {
        finding.paths().iter().map(|p| p.path.display().to_string()).collect()
    }

    #[test]
    fn every_build_cache_kind_is_found_once() {
        let findings = detect(&fs());

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
        assert!(findings.iter().all(|f| f.reasoning().is_some()), "every finding explains itself");
    }

    #[test]
    fn archives_do_not_claim_to_be_rebuilt() {
        let findings = detect(&fs());

        let archives = by_id(&findings, "build-cache.xcode-archives");
        assert_eq!(archives.risk(), Risk::Amber);
        assert!(archives.reasoning().unwrap().contains("nothing recreates it"), "{:?}", archives.reasoning());
    }

    #[test]
    fn pycache_dirs_are_listed_biggest_first() {
        let fs = fs();
        fs.add_file(format!("{H}/code/aaa/__pycache__/big.pyc"), &[]);
        fs.set_size(format!("{H}/code/aaa/__pycache__/big.pyc"), 900_000);

        let findings = detect(&fs);

        let caches = by_id(&findings, "build-cache.pycache");
        assert!(listed(caches)[0].ends_with("aaa/__pycache__"), "{:?}", listed(caches));
    }

    #[test]
    fn cargo_targets_need_a_cargo_toml_beside_them() {
        let findings = detect(&fs());

        let targets = by_id(&findings, "build-cache.cargo-target");
        assert_eq!(targets.paths().len(), 1);
        assert!(targets.paths()[0].path.ends_with("rusty/target"));
    }

    #[test]
    fn the_docker_disk_is_inform_only_with_its_allocated_size() {
        let findings = detect(&fs());

        let docker = by_id(&findings, "build-cache.docker-raw");
        assert_eq!(docker.action(), Action::InformOnly);
        assert!(!docker.is_actionable());
        assert!(docker.instructions().is_some());
        assert_eq!(docker.reclaimable_bytes(), 500_000_u64.div_ceil(4096) * 4096);
    }
}
