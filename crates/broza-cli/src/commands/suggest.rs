//! `broza suggest` (`docs/cli-spec.md` §3.3 and §4.3).
//!
//! One walk of the home (through the same cache `scan` uses), every detector
//! the flags allow, the risk and size filters, and the three renderings. It
//! writes nothing: the plan it leads to is `clean`'s business.

use std::path::{Path, PathBuf};
use std::time::Duration;

use broza::detect::{DetectContext, Registry, RiskFilter as CoreRiskFilter, filter};
use broza::model::{Category, Envelope, Finding, Host, Risk, SuggestReport, Warning};
use broza::ports::Ports;
use broza::scan::{MountTable, scan_paths};
use broza::units::{ByteSize, DurationSpec};
use broza::{BrozaError, ExitCode};
use jiff::Timestamp;

use crate::args::{RiskFilter, SuggestArgs};
use crate::commands::Outcome;
use crate::commands::mount::mount_table;
use crate::commands::scan::folders::{FolderSettings, request_for_home};
use crate::output::human::suggest as human_suggest;
use crate::output::{ColorPolicy, OutputFormat, Renderer, csv, envelope_to_json};

/// Command name in the JSON envelope.
const COMMAND: &str = "suggest";
/// Header of the `--csv` table (`docs/cli-spec.md` §3.3).
const CSV_HEADER: [&str; 8] =
    ["id", "category", "title", "risk", "action", "actionable", "reclaimable_bytes", "item_count"];

/// Everything `run` needs beyond the arguments.
pub struct SuggestContext<'a> {
    /// The wired adapters.
    pub ports: &'a Ports,
    /// The `suggest` flags as parsed.
    pub args: &'a SuggestArgs,
    /// `host` block of the envelope.
    pub host: Host,
    /// `generated_at` of the envelope.
    pub generated_at: Timestamp,
    /// Warnings raised before the command ran.
    pub warnings: Vec<Warning>,
    /// Whether the human rendering may use colour.
    pub policy: ColorPolicy,
    /// Format the caller asked for.
    pub format: OutputFormat,
    /// Home, cache and progress settings shared with `scan`.
    pub folders: FolderSettings,
    /// `min-size` from the configuration, when the flag is absent.
    pub config_min_size: ByteSize,
    /// `unused-after` from the configuration, when the flag is absent.
    pub config_unused_after: DurationSpec,
}

/// Walk, detect, filter and render.
///
/// # Errors
///
/// A `--category` that names no category or a `--min-size`/`--unused-after`
/// that does not parse is a usage error (exit `2`); `HOME` unset is one too.
pub fn run(context: &SuggestContext<'_>) -> Result<Outcome, BrozaError> {
    let home = context.folders.home.as_deref().ok_or_else(|| {
        BrozaError::Usage("HOME is not set: suggest needs a home directory to look at".into())
    })?;
    let categories = parse_categories(&context.args.categories)?;
    let min_size = size_or(context.args.min_size.as_deref(), context.config_min_size)?;
    let unused_after = duration_or(context.args.unused_after.as_deref(), context.config_unused_after)?;

    let enumeration = context.ports.disks.enumerate()?;
    let mount = mount_table(context.ports, &enumeration.disks)?;
    let (nodes, walk_warnings) = walk_home(home, &mount.table, context)?;
    let detect_context = DetectContext::new(
        home,
        context.ports.fs.as_ref(),
        &mount.table,
        context.generated_at,
        unused_after,
        &nodes,
    );
    let report = Registry::builtin().restricted_to(categories.as_deref()).run(&detect_context);
    let findings = filter::apply(report.findings, core_risk(context.args.risk), min_size.bytes());
    let warnings =
        [context.warnings.clone(), enumeration.warnings, mount.warnings, walk_warnings, report.warnings]
            .concat();
    let output = SuggestOutput {
        data: SuggestReport::from_findings(findings),
        explain: context.args.explain,
        home: home.to_path_buf(),
        host: context.host.clone(),
        generated_at: context.generated_at,
        warnings: warnings.clone(),
        policy: context.policy,
    };
    Ok(Outcome::ok(output.render(context.format)?).with_warnings(warnings).with_code(ExitCode::Ok))
}

/// The home walk detectors read, through `scan`'s cache.
fn walk_home(
    home: &Path,
    mounts: &MountTable,
    context: &SuggestContext<'_>,
) -> Result<(Vec<broza::scan::DirNode>, Vec<Warning>), BrozaError> {
    let request = request_for_home(&context.folders)?;
    let mut scans = scan_paths(&[home.to_path_buf()], &request, context.ports, mounts, None)?;
    let scan = scans.pop().ok_or_else(|| BrozaError::Other("the home walk produced nothing".into()))?;
    Ok((scan.nodes, scan.warnings))
}

/// `--category` values as categories; an unknown id is a usage error.
fn parse_categories(raw: &[String]) -> Result<Option<Vec<Category>>, BrozaError> {
    if raw.is_empty() {
        return Ok(None);
    }
    raw.iter()
        .map(|id| {
            Category::all().into_iter().find(|c| c.as_str() == id.trim()).ok_or_else(|| {
                BrozaError::Usage(format!("unknown category `{id}`; `broza explain <category>` lists them"))
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn size_or(flag: Option<&str>, fallback: ByteSize) -> Result<ByteSize, BrozaError> {
    flag.map_or(Ok(fallback), str::parse)
}

fn duration_or(flag: Option<&str>, fallback: DurationSpec) -> Result<Duration, BrozaError> {
    Ok(flag.map_or(Ok(fallback), str::parse::<DurationSpec>)?.to_duration())
}

/// The CLI's `--risk` enum as the core filter.
const fn core_risk(risk: RiskFilter) -> CoreRiskFilter {
    match risk {
        RiskFilter::Green => CoreRiskFilter::Only(Risk::Green),
        RiskFilter::Amber => CoreRiskFilter::Only(Risk::Amber),
        RiskFilter::Red => CoreRiskFilter::Only(Risk::Red),
        RiskFilter::All => CoreRiskFilter::All,
    }
}

/// A rendered `suggest`, in any of the three formats.
#[derive(Debug, Clone)]
pub struct SuggestOutput {
    /// The payload of `docs/cli-spec.md` §4.3.
    pub data: SuggestReport,
    /// `--explain`: print each finding's reasoning.
    pub explain: bool,
    /// Home directory, so paths print as `~/…`.
    pub home: PathBuf,
    /// `host` block of the envelope.
    pub host: Host,
    /// `generated_at` of the envelope.
    pub generated_at: Timestamp,
    /// Warnings carried inside the envelope.
    pub warnings: Vec<Warning>,
    /// Whether the human rendering may use colour.
    pub policy: ColorPolicy,
}

impl SuggestOutput {
    fn envelope(&self) -> Envelope<SuggestReport> {
        let envelope = Envelope::new(COMMAND, self.host.clone(), self.generated_at, self.data.clone());
        self.warnings.iter().cloned().fold(envelope, Envelope::with_warning)
    }
}

impl Renderer for SuggestOutput {
    fn to_human(&self) -> String {
        human_suggest::render(&self.data, self.explain, &self.home, self.policy)
    }

    fn to_json(&self) -> Result<String, BrozaError> {
        envelope_to_json(&self.envelope())
    }

    fn to_csv(&self) -> Result<String, BrozaError> {
        let rows = self.data.findings.iter().map(csv_row);
        Ok(std::iter::once(csv::row(&CSV_HEADER)).chain(rows).collect::<Vec<_>>().join("\n"))
    }
}

/// One finding as a CSV row, in the contract's own tokens.
fn csv_row(finding: &Finding) -> String {
    csv::row(&[
        finding.id().to_string(),
        finding.category().as_str().to_owned(),
        finding.title().to_owned(),
        token(&finding.risk()),
        token(&finding.action()),
        finding.is_actionable().to_string(),
        finding.reclaimable_bytes().to_string(),
        finding.item_count().map_or_else(String::new, |n| n.to_string()),
    ])
}

/// The serde token of a contract enum (`green`, `inform_only`, …).
fn token<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value).ok().and_then(|v| v.as_str().map(ToOwned::to_owned)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;

    use broza::model::{Container, Disk, FsKind, Volume, VolumeId, VolumeRole};

    use super::*;

    fn id(raw: &str) -> VolumeId {
        raw.parse().unwrap()
    }

    fn data_disk() -> Vec<Disk> {
        vec![Disk {
            id: id("disk0"),
            model: "SSD".into(),
            size_bytes: 1_000_000_000_000,
            internal: true,
            containers: vec![Container {
                id: id("disk3"),
                kind: FsKind::Apfs,
                size_bytes: 1_000_000_000_000,
                used_bytes: 500_000_000_000,
                free_bytes: 500_000_000_000,
                purgeable_bytes: 0,
                volumes: vec![Volume {
                    id: id("disk3s5"),
                    name: "Data".into(),
                    uuid: None,
                    role: VolumeRole::Data,
                    mount_point: Some(PathBuf::from("/System/Volumes/Data")),
                    used_bytes: 500_000_000_000,
                    writable_by_broza: true,
                    purpose: String::new(),
                }],
            }],
        }]
    }

    fn args() -> SuggestArgs {
        SuggestArgs {
            categories: Vec::new(),
            risk: RiskFilter::All,
            min_size: None,
            unused_after: None,
            explain: false,
        }
    }

    fn run_with(args: &SuggestArgs, format: OutputFormat) -> Result<Outcome, BrozaError> {
        let (ports, handles) = broza::testing::fake_ports();
        handles.disks.set_disks(data_disk());
        handles.fs.add_root("/", 1);
        handles.fs.add_root("/System/Volumes/Data", 2);
        handles.fs.add_file("/System/Volumes/Data/Users/dana/Library/Caches/App/c.db", &[]);
        handles.fs.set_size("/System/Volumes/Data/Users/dana/Library/Caches/App/c.db", 900_000_000);
        handles.fs.add_file("/System/Volumes/Data/Users/dana/Library/Developer/Xcode/DerivedData/A/x.o", &[]);
        handles.fs.set_size(
            "/System/Volumes/Data/Users/dana/Library/Developer/Xcode/DerivedData/A/x.o",
            5_000_000_000,
        );
        run(&SuggestContext {
            ports: &ports,
            args,
            host: Host { macos_version: "26.1".into(), arch: "arm64".into() },
            generated_at: "2026-09-21T10:36:08Z".parse().unwrap(),
            warnings: Vec::new(),
            policy: ColorPolicy::Never,
            format,
            folders: FolderSettings {
                home: Some(PathBuf::from("/System/Volumes/Data/Users/dana")),
                cache_ttl: Duration::from_secs(60),
                no_cache: true,
                show_progress: false,
                verbose: false,
                own_stores: Vec::new(),
            },
            config_min_size: ByteSize::new(50_000_000),
            config_unused_after: "1y".parse().unwrap(),
        })
    }

    #[test]
    fn a_run_finds_the_green_categories_and_renders_them() {
        let outcome = run_with(&args(), OutputFormat::Human).unwrap();

        assert!(outcome.rendered.contains("SAFE (green)"), "{}", outcome.rendered);
        assert!(outcome.rendered.contains("build-cache"), "{}", outcome.rendered);
        assert!(outcome.rendered.contains("user-cache"), "{}", outcome.rendered);
        assert_eq!(outcome.code, ExitCode::Ok);
    }

    #[test]
    fn category_and_risk_flags_narrow_the_report() {
        let only_caches = SuggestArgs { categories: vec!["user-cache".into()], ..args() };
        let outcome = run_with(&only_caches, OutputFormat::Csv).unwrap();

        assert!(
            outcome.rendered.lines().skip(1).all(|line| line.starts_with("user-cache.")),
            "{}",
            outcome.rendered
        );
        let unknown =
            run_with(&SuggestArgs { categories: vec!["nope".into()], ..args() }, OutputFormat::Human);
        assert!(matches!(unknown, Err(BrozaError::Usage(_))));
    }

    #[test]
    fn the_json_is_an_envelope_with_totals_that_add_up() {
        let outcome = run_with(&args(), OutputFormat::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&outcome.rendered).unwrap();

        assert_eq!(value["command"], "suggest");
        let findings = value["data"]["findings"].as_array().unwrap();
        let sum: u64 = findings.iter().map(|f| f["reclaimable_bytes"].as_u64().unwrap()).sum();
        assert_eq!(value["data"]["total_reclaimable_bytes"].as_u64(), Some(sum));
    }
}
