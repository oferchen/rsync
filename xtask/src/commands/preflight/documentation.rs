use crate::error::{TaskError, TaskResult};
use crate::util::ensure;
use crate::workspace::WorkspaceBranding;
use std::fs;
use std::path::Path;

pub(super) fn validate_documentation(
    workspace: &Path,
    branding: &WorkspaceBranding,
) -> TaskResult<()> {
    struct DocumentationCheck<'a> {
        relative_path: &'a str,
        required_snippets: Vec<&'a str>,
    }

    let checks = [
        DocumentationCheck {
            relative_path: "README.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
                branding.daemon_config.as_str(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/production_scope_p1.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
                branding.daemon_config_dir.as_str(),
                branding.daemon_config.as_str(),
                branding.daemon_secrets.as_str(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/differences.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/gaps.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/COMPARE.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
                branding.daemon_config.as_str(),
            ],
        },
        DocumentationCheck {
            relative_path: "docs/feature_matrix.md",
            required_snippets: vec![
                branding.client_bin.as_str(),
                branding.daemon_bin.as_str(),
                branding.rust_version.as_str(),
                branding.daemon_config.as_str(),
                branding.daemon_secrets.as_str(),
            ],
        },
    ];

    for check in checks {
        let path = workspace.join(check.relative_path);
        let contents = fs::read_to_string(&path).map_err(|error| {
            TaskError::Io(std::io::Error::new(
                error.kind(),
                format!("failed to read {}: {error}", path.display()),
            ))
        })?;

        let missing: Vec<&str> = check
            .required_snippets
            .iter()
            .copied()
            .filter(|snippet| !snippet.is_empty() && !contents.contains(snippet))
            .collect();

        ensure(
            missing.is_empty(),
            format!(
                "{} missing required documentation snippets: {}",
                check.relative_path,
                missing
                    .iter()
                    .map(|snippet| format!("'{}'", snippet))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )?;
    }

    Ok(())
}
