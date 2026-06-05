//! CVE status validation between release notes and `SECURITY.md`.
//!
//! Parses CVE rows from the release body and `SECURITY.md`, then checks
//! that every CVE status in the release body matches the canonical status
//! in `SECURITY.md`.

use crate::error::{TaskError, TaskResult};
use crate::util::read_file_with_context;
use std::path::Path;

/// Options for the `validate` subcommand.
#[derive(Debug, Clone, Default)]
pub struct ValidateOptions {
    /// Path to a rendered release body file (default: `.github/RELEASE_TEMPLATE.md`).
    pub body: Option<std::path::PathBuf>,
}

/// A CVE row extracted from a markdown table.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CveRow {
    /// CVE identifier (e.g., `CVE-2026-29518`).
    id: String,
    /// Status string (e.g., `Fixed`, `Mitigated`, `Not vulnerable`).
    status: String,
}

/// Executes the `validate` subcommand.
pub fn execute(workspace: &Path, options: ValidateOptions) -> TaskResult<()> {
    let security_path = workspace.join("SECURITY.md");
    let security_content = read_file_with_context(&security_path)?;
    let security_cves = extract_cve_rows(&security_content);

    let body_content = match options.body {
        Some(path) => {
            let resolved = if path.is_absolute() {
                path
            } else {
                workspace.join(path)
            };
            read_file_with_context(&resolved)?
        }
        None => {
            let template_path = workspace.join(".github/RELEASE_TEMPLATE.md");
            read_file_with_context(&template_path)?
        }
    };
    let body_cves = extract_cve_rows(&body_content);

    if body_cves.is_empty() {
        println!("No CVE rows found in release body - nothing to validate.");
        return Ok(());
    }

    let mut mismatches = Vec::new();
    for body_cve in &body_cves {
        match security_cves.iter().find(|s| s.id == body_cve.id) {
            Some(security_cve) if security_cve.status != body_cve.status => {
                mismatches.push(format!(
                    "{}: release body says '{}' but SECURITY.md says '{}'",
                    body_cve.id, body_cve.status, security_cve.status,
                ));
            }
            None => {
                mismatches.push(format!(
                    "{}: present in release body but not found in SECURITY.md",
                    body_cve.id,
                ));
            }
            Some(_) => {}
        }
    }

    if mismatches.is_empty() {
        println!(
            "All {} CVE rows in release body match SECURITY.md.",
            body_cves.len()
        );
        Ok(())
    } else {
        let message = format!(
            "CVE status mismatches found:\n{}",
            mismatches
                .iter()
                .map(|m| format!("  - {m}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        Err(TaskError::Validation(message))
    }
}

/// Extracts CVE rows from markdown table lines.
///
/// Matches lines of the form `| CVE-20XX-NNNNN | ... | **Status** | ... |`
/// or `| CVE-20XX-NNNNN | ... | Status | ... |`. The status column is the
/// third or fourth pipe-delimited field depending on the table layout.
fn extract_cve_rows(content: &str) -> Vec<CveRow> {
    let mut rows = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') || !trimmed.contains("CVE-20") {
            continue;
        }

        let columns: Vec<&str> = trimmed
            .split('|')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();

        if columns.len() < 3 {
            continue;
        }

        let cve_id = columns[0].trim().to_owned();
        if !cve_id.starts_with("CVE-20") {
            continue;
        }

        // Find the status column - look for known status keywords.
        // The release template has 4 columns: CVE | Description | Status | Cite
        // SECURITY.md has 4 columns: CVE | Upstream Issue | oc-rsync Status | Reason
        let status = find_status_in_columns(&columns);
        if let Some(status) = status {
            rows.push(CveRow { id: cve_id, status });
        }
    }

    rows
}

/// Searches table columns for a recognized CVE status value.
fn find_status_in_columns(columns: &[&str]) -> Option<String> {
    let known_statuses = [
        "Fixed",
        "Mostly fixed",
        "Mitigated",
        "Not vulnerable",
        "Not applicable",
    ];

    for col in columns.iter().skip(1) {
        let cleaned = col.trim().replace("**", "").trim().to_owned();

        for status in &known_statuses {
            if cleaned.starts_with(status) {
                return Some((*status).to_owned());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn extract_cve_rows_from_release_template() {
        let content = r#"
| CVE | Description | Status | Cite |
|-----|-------------|--------|------|
| CVE-2026-29518 | Path traversal via symlink race | Fixed | SEC-1 |
| CVE-2026-43619 | Privilege escalation via TOCTOU | Fixed | SEC-1 |
"#;
        let rows = extract_cve_rows(content);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "CVE-2026-29518");
        assert_eq!(rows[0].status, "Fixed");
        assert_eq!(rows[1].id, "CVE-2026-43619");
        assert_eq!(rows[1].status, "Fixed");
    }

    #[test]
    fn extract_cve_rows_from_security_md() {
        let content = r#"
| CVE | Upstream Issue | oc-rsync Status | Reason |
|-----|---------------|-----------------|--------|
| CVE-2024-12084 | Heap overflow | Not vulnerable | Rust Vec |
| CVE-2024-12088 | --safe-links bypass | Mitigated | Rust path handling |
| CVE-2026-29518 | TOCTOU symlink race | **Fixed** | All path-based syscalls migrated |
"#;
        let rows = extract_cve_rows(content);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id, "CVE-2024-12084");
        assert_eq!(rows[0].status, "Not vulnerable");
        assert_eq!(rows[1].id, "CVE-2024-12088");
        assert_eq!(rows[1].status, "Mitigated");
        assert_eq!(rows[2].id, "CVE-2026-29518");
        assert_eq!(rows[2].status, "Fixed");
    }

    #[test]
    fn extract_cve_rows_skips_header_and_separator() {
        let content = r#"
| CVE | Description | Status | Cite |
|-----|-------------|--------|------|
| CVE-2026-29518 | test | Fixed | ref |
"#;
        let rows = extract_cve_rows(content);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn extract_cve_rows_empty_on_no_cves() {
        let content = "No CVE table here.\nJust plain text.\n";
        let rows = extract_cve_rows(content);
        assert!(rows.is_empty());
    }

    #[test]
    fn validate_matching_statuses_succeeds() {
        let dir = tempdir().unwrap();

        let security = r#"
| CVE | Upstream Issue | oc-rsync Status | Reason |
|-----|---------------|-----------------|--------|
| CVE-2026-29518 | Symlink race | **Fixed** | migrated |
| CVE-2026-43619 | TOCTOU | **Fixed** | migrated |
"#;
        fs::write(dir.path().join("SECURITY.md"), security).unwrap();

        let body = r#"
| CVE | Description | Status | Cite |
|-----|-------------|--------|------|
| CVE-2026-29518 | Path traversal | Fixed | SEC-1 |
| CVE-2026-43619 | Privilege escalation | Fixed | SEC-1 |
"#;
        let body_path = dir.path().join("body.md");
        fs::write(&body_path, body).unwrap();

        let options = ValidateOptions {
            body: Some(body_path),
        };
        execute(dir.path(), options).unwrap();
    }

    #[test]
    fn validate_mismatched_status_fails() {
        let dir = tempdir().unwrap();

        let security = r#"
| CVE | Upstream Issue | oc-rsync Status | Reason |
|-----|---------------|-----------------|--------|
| CVE-2026-29518 | Symlink race | **Mitigated** | partially done |
"#;
        fs::write(dir.path().join("SECURITY.md"), security).unwrap();

        let body = r#"
| CVE | Description | Status | Cite |
|-----|-------------|--------|------|
| CVE-2026-29518 | Path traversal | Fixed | SEC-1 |
"#;
        let body_path = dir.path().join("body.md");
        fs::write(&body_path, body).unwrap();

        let options = ValidateOptions {
            body: Some(body_path),
        };
        let err = execute(dir.path(), options).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("CVE-2026-29518"));
        assert!(message.contains("Fixed"));
        assert!(message.contains("Mitigated"));
    }

    #[test]
    fn validate_missing_cve_in_security_md_fails() {
        let dir = tempdir().unwrap();

        let security = "# Security\nNo CVE table.\n";
        fs::write(dir.path().join("SECURITY.md"), security).unwrap();

        let body = r#"
| CVE | Description | Status | Cite |
|-----|-------------|--------|------|
| CVE-2099-99999 | Future vuln | Fixed | ref |
"#;
        let body_path = dir.path().join("body.md");
        fs::write(&body_path, body).unwrap();

        let options = ValidateOptions {
            body: Some(body_path),
        };
        let err = execute(dir.path(), options).unwrap_err();
        assert!(err.to_string().contains("CVE-2099-99999"));
        assert!(err.to_string().contains("not found in SECURITY.md"));
    }

    #[test]
    fn validate_no_cves_in_body_succeeds() {
        let dir = tempdir().unwrap();

        fs::write(dir.path().join("SECURITY.md"), "# Security\n").unwrap();

        let body_path = dir.path().join("body.md");
        fs::write(&body_path, "## Release notes\nNo CVE table.\n").unwrap();

        let options = ValidateOptions {
            body: Some(body_path),
        };
        execute(dir.path(), options).unwrap();
    }

    #[test]
    fn find_status_extracts_bold_markers() {
        let columns = vec!["CVE-2026-29518", "Description", "**Fixed**", "cite"];
        assert_eq!(find_status_in_columns(&columns), Some("Fixed".to_owned()));
    }

    #[test]
    fn find_status_returns_none_for_unknown() {
        let columns = vec!["CVE-2026-29518", "Description", "Unknown", "cite"];
        assert_eq!(find_status_in_columns(&columns), None);
    }
}
