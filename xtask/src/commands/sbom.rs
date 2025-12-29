use crate::error::{TaskError, TaskResult};
#[cfg(test)]
use crate::util::is_help_flag;
use crate::workspace::{WorkspaceBranding, load_workspace_branding};
use cargo_metadata::{CargoOpt, Metadata, MetadataCommand, Package};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

/// Options accepted by the `sbom` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SbomOptions {
    /// Optional override for the SBOM output path.
    pub output: Option<PathBuf>,
}

/// Parses CLI arguments for the `sbom` command.
#[cfg(test)]
pub fn parse_args<I>(args: I) -> TaskResult<SbomOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut output = None;

    while let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        if arg == "--output" {
            let value = args.next().ok_or_else(|| {
                TaskError::Usage(String::from(
                    "--output requires a path argument; see `cargo xtask sbom --help`",
                ))
            })?;

            if output.is_some() {
                return Err(TaskError::Usage(String::from(
                    "--output specified multiple times",
                )));
            }

            output = Some(PathBuf::from(value));
            continue;
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for sbom command",
            arg.to_string_lossy()
        )));
    }

    Ok(SbomOptions { output })
}

/// Executes the `sbom` command.
pub fn execute(workspace: &Path, options: SbomOptions) -> TaskResult<()> {
    let default_output = PathBuf::from("target/sbom/rsync.cdx.json");
    let raw_output = options.output.unwrap_or(default_output);
    let output_path = if raw_output.is_absolute() {
        raw_output
    } else {
        workspace.join(raw_output)
    };

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    println!("Generating SBOM at {}", output_path.display());

    let manifest_path = workspace.join("Cargo.toml");
    let metadata = load_metadata(&manifest_path)?;
    let branding = load_workspace_branding(workspace)?;
    let bom = build_bom(&branding, &metadata)?;
    let document = serde_json::to_vec_pretty(&bom)
        .map_err(|error| TaskError::Metadata(format!("failed to encode SBOM JSON: {error}")))?;
    fs::write(output_path, document)?;
    Ok(())
}

/// Returns usage text for the command.
#[cfg(test)]
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask sbom [--output PATH]\n\nOptions:\n  --output PATH    Override the SBOM output path (relative to the workspace root unless absolute)\n  -h, --help       Show this help message",
    )
}

fn load_metadata(manifest_path: &Path) -> TaskResult<Metadata> {
    let mut command = MetadataCommand::new();
    command
        .manifest_path(manifest_path)
        .features(CargoOpt::AllFeatures)
        .other_options(vec![String::from("--locked")]);

    command
        .exec()
        .map_err(|error| TaskError::Metadata(format!("failed to load cargo metadata: {error}")))
}

fn build_bom(branding: &WorkspaceBranding, metadata: &Metadata) -> TaskResult<Bom> {
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    let mut components_by_id = HashMap::new();

    for package in &metadata.packages {
        let component = Component::from_package(package, workspace_members.contains(&package.id));
        components_by_id.insert(package.id.clone(), component);
    }

    let root_id = metadata
        .resolve
        .as_ref()
        .and_then(|resolve| resolve.root.clone())
        .or_else(|| metadata.workspace_members.first().cloned())
        .ok_or_else(|| {
            TaskError::Metadata(String::from("workspace metadata missing root package"))
        })?;

    let root_component = components_by_id
        .get(&root_id)
        .cloned()
        .ok_or_else(|| TaskError::Metadata(String::from("failed to identify root component")))?;

    let mut components: Vec<_> = components_by_id.values().cloned().collect();
    components.sort_by(|lhs, rhs| lhs.bom_ref.cmp(&rhs.bom_ref));

    let mut dependencies = Vec::new();
    if let Some(resolve) = &metadata.resolve {
        for node in &resolve.nodes {
            let Some(component) = components_by_id.get(&node.id) else {
                continue;
            };

            let mut depends_on = node
                .deps
                .iter()
                .filter_map(|dep| components_by_id.get(&dep.pkg))
                .map(|dependency| dependency.bom_ref.clone())
                .collect::<Vec<_>>();
            depends_on.sort();
            depends_on.dedup();

            dependencies.push(Dependency {
                reference: component.bom_ref.clone(),
                depends_on,
            });
        }
    }

    dependencies.sort_by(|lhs, rhs| lhs.reference.cmp(&rhs.reference));

    Ok(Bom {
        bom_format: String::from("CycloneDX"),
        spec_version: String::from("1.5"),
        version: 1,
        metadata: MetadataSection {
            component: root_component,
            tools: vec![Tool {
                vendor: Some(branding.client_bin.clone()),
                name: String::from("xtask sbom"),
                version: Some(String::from(env!("CARGO_PKG_VERSION"))),
            }],
        },
        components,
        dependencies,
    })
}

#[derive(Clone, Serialize)]
struct Bom {
    #[serde(rename = "bomFormat")]
    bom_format: String,
    #[serde(rename = "specVersion")]
    spec_version: String,
    version: u32,
    metadata: MetadataSection,
    components: Vec<Component>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    dependencies: Vec<Dependency>,
}

#[derive(Clone, Serialize)]
struct MetadataSection {
    component: Component,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Tool>,
}

#[derive(Clone, Serialize)]
struct Tool {
    #[serde(skip_serializing_if = "Option::is_none")]
    vendor: Option<String>,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

#[derive(Clone, Serialize)]
struct Component {
    #[serde(rename = "bom-ref")]
    bom_ref: String,
    #[serde(rename = "type")]
    component_type: String,
    name: String,
    version: String,
    purl: String,
}

impl Component {
    fn from_package(package: &Package, is_workspace_member: bool) -> Self {
        let reference = bom_reference(package);
        Self {
            bom_ref: reference.clone(),
            component_type: if is_workspace_member {
                String::from("application")
            } else {
                String::from("library")
            },
            name: package.name.clone(),
            version: package.version.to_string(),
            purl: reference,
        }
    }
}

#[derive(Clone, Serialize)]
struct Dependency {
    #[serde(rename = "ref")]
    reference: String,
    #[serde(rename = "dependsOn", skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
}

fn bom_reference(package: &Package) -> String {
    format!("pkg:cargo/{}@{}", package.name, package.version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, SbomOptions { output: None });
    }

    #[test]
    fn parse_args_reports_help_request() {
        let error = parse_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == usage()));
    }

    #[test]
    fn parse_args_accepts_output_override() {
        let options = parse_args([OsString::from("--output"), OsString::from("custom.json")])
            .expect("parse succeeds");
        assert_eq!(
            options,
            SbomOptions {
                output: Some(PathBuf::from("custom.json")),
            }
        );
    }

    #[test]
    fn parse_args_rejects_duplicate_output_flags() {
        let error = parse_args([
            OsString::from("--output"),
            OsString::from("one.json"),
            OsString::from("--output"),
        ])
        .unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--output")));
    }

    #[test]
    fn parse_args_rejects_unknown_argument() {
        let error = parse_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("--unknown")));
    }

    fn workspace_root() -> &'static Path {
        static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_path_buf()
        })
    }

    #[test]
    fn execute_generates_sbom_document() {
        let temp = tempdir().expect("create temp dir");
        let output = temp.path().join("sbom.json");
        execute(
            workspace_root(),
            SbomOptions {
                output: Some(output.clone()),
            },
        )
        .expect("sbom execution succeeds");

        let data = fs::read(&output).expect("read sbom output");
        let value: serde_json::Value = serde_json::from_slice(&data).expect("parse sbom json");
        assert_eq!(value.get("bomFormat").unwrap(), "CycloneDX");
        assert!(value.get("components").is_some());

        let branding = load_workspace_branding(workspace_root()).expect("load branding");
        let vendor = value
            .get("metadata")
            .and_then(|meta| meta.get("tools"))
            .and_then(|tools| tools.get(0))
            .and_then(|tool| tool.get("vendor"))
            .and_then(|vendor| vendor.as_str())
            .expect("vendor string");
        assert_eq!(vendor, branding.client_bin);
    }

    #[test]
    fn build_bom_reports_missing_root_component() {
        let mut metadata = load_metadata(&workspace_root().join("Cargo.toml"))
            .expect("load metadata for mutation");
        metadata.resolve = None;
        metadata.workspace_members.clear();

        let branding = load_workspace_branding(workspace_root()).expect("load branding");

        let error = match build_bom(&branding, &metadata) {
            Ok(_) => panic!("expected build_bom to fail"),
            Err(error) => error,
        };
        assert!(matches!(error, TaskError::Metadata(message) if message.contains("missing root")));
    }
}
