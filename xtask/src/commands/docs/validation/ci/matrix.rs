use super::super::read_file;
use super::yaml::{extract_job_section, find_yaml_scalar};
use crate::error::TaskResult;
use crate::workspace::WorkspaceBranding;
use std::collections::BTreeSet;
use std::path::Path;

pub(super) fn validate_ci_cross_compile_matrix(
    workspace: &Path,
    branding: &WorkspaceBranding,
    failures: &mut Vec<String>,
) -> TaskResult<()> {
    let ci_path = workspace.join(".github").join("workflows").join("ci.yml");
    let ci_contents = read_file(&ci_path)?;
    let display_path = ci_path
        .strip_prefix(workspace)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| ci_path.display().to_string());

    let mut expected_entries = BTreeSet::new();

    for (os, arches) in &branding.cross_compile {
        for arch in arches {
            if let Some(name) = expected_matrix_name(os, arch) {
                ensure_matrix_entry(
                    failures,
                    &display_path,
                    &ci_contents,
                    &name,
                    MatrixExpectations {
                        enabled: Some(true),
                        target: expected_target(os, arch),
                        build_command: Some(expected_build_command(os)),
                        build_daemon: Some(expected_build_daemon(os, arch)),
                        uses_zig: Some(expected_uses_zig(os)),
                        generate_sbom: Some(expected_generate_sbom(os)),
                        needs_cross_gcc: expected_needs_cross_gcc(os, arch),
                    },
                );
                expected_entries.insert(name);
            } else {
                failures.push(format!(
                    "{display_path}: unrecognised cross-compile platform '{os}' in workspace metadata"
                ));
            }
        }
    }

    ensure_matrix_entry(
        failures,
        &display_path,
        &ci_contents,
        "windows-x86",
        MatrixExpectations {
            enabled: Some(false),
            target: expected_target("windows", "x86"),
            build_command: Some("zigbuild"),
            build_daemon: Some(false),
            uses_zig: Some(true),
            generate_sbom: Some(false),
            needs_cross_gcc: Some(false),
        },
    );
    if failures.is_empty() {
        expected_entries.insert(String::from("windows-x86"));
    }

    ensure_matrix_entry(
        failures,
        &display_path,
        &ci_contents,
        "windows-aarch64",
        MatrixExpectations {
            enabled: Some(false),
            target: expected_target("windows", "aarch64"),
            build_command: Some("zigbuild"),
            build_daemon: Some(false),
            uses_zig: Some(true),
            generate_sbom: Some(false),
            needs_cross_gcc: Some(false),
        },
    );

    if failures.is_empty() {
        expected_entries.insert(String::from("windows-aarch64"));
    }

    if failures.is_empty() {
        let names = collect_matrix_platform_names(&ci_contents);
        check_for_unexpected_matrix_entries(&display_path, &names, &expected_entries, failures);
    }

    ensure_cross_compile_parallelism(&display_path, &ci_contents, failures);

    Ok(())
}

fn ensure_cross_compile_parallelism(
    display_path: &str,
    contents: &str,
    failures: &mut Vec<String>,
) {
    let Some(section) = extract_job_section(contents, "cross-compile") else {
        failures.push(format!(
            "{display_path}: missing cross-compile job definition"
        ));
        return;
    };

    match find_yaml_scalar(&section, "max-parallel") {
        Some(value) => match value.parse::<usize>() {
            Ok(parallelism) if parallelism > 1 => {}
            Ok(_) => failures.push(format!(
                "{display_path}: cross-compile job must set max-parallel greater than 1 for parallel builds"
            )),
            Err(error) => failures.push(format!(
                "{display_path}: failed to parse max-parallel value '{value}': {error}"
            )),
        },
        None => failures.push(format!(
            "{display_path}: cross-compile job missing max-parallel configuration"
        )),
    }

    match find_yaml_scalar(&section, "fail-fast") {
        Some(value) if value.eq_ignore_ascii_case("false") => {}
        Some(value) => failures.push(format!(
            "{display_path}: cross-compile job must disable fail-fast to keep the matrix running in parallel (found '{value}')"
        )),
        None => failures.push(format!(
            "{display_path}: cross-compile job missing fail-fast configuration"
        )),
    }
}

fn collect_matrix_platform_names(contents: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_platform_list = false;
    let mut platform_indent = 0usize;

    for line in contents.lines() {
        if !in_platform_list {
            if let Some(index) = line.find("platform:") {
                platform_indent = line[..index].chars().take_while(|c| *c == ' ').count();
                in_platform_list = true;
            }
            continue;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let indent = line.chars().take_while(|c| *c == ' ').count();
        if indent <= platform_indent {
            in_platform_list = false;
            if let Some(index) = line.find("platform:") {
                platform_indent = line[..index].chars().take_while(|c| *c == ' ').count();
                in_platform_list = true;
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("- name:") {
            names.push(rest.trim().to_string());
        }
    }

    names
}

fn check_for_unexpected_matrix_entries(
    display_path: &str,
    names: &[String],
    expected_entries: &BTreeSet<String>,
    failures: &mut Vec<String>,
) {
    let mut seen = BTreeSet::new();

    for name in names {
        if !seen.insert(name.clone()) {
            failures.push(format!(
                "{display_path}: duplicate cross-compilation entry '{name}'"
            ));
        }

        if !expected_entries.contains(name) {
            failures.push(format!(
                "{display_path}: unexpected cross-compilation entry '{name}'"
            ));
        }
    }

    for expected in expected_entries {
        if !seen.contains(expected) {
            failures.push(format!(
                "{display_path}: missing cross-compilation entry '{expected}'"
            ));
        }
    }
}

fn ensure_matrix_entry(
    failures: &mut Vec<String>,
    display_path: &str,
    contents: &str,
    name: &str,
    expectations: MatrixExpectations<'_>,
) {
    match extract_matrix_entry(contents, name) {
        Some(entry) => {
            if let Some(expected) = expectations.enabled {
                if entry.enabled != Some(expected) {
                    failures.push(format!(
                        "{display_path}: cross-compilation entry '{name}' expected enabled={expected}"
                    ));
                }
            }

            if let Some(expected) = expectations.target {
                if entry.target.as_deref() != Some(expected) {
                    failures.push(format!(
                        "{display_path}: cross-compilation entry '{name}' expected target='{expected}'"
                    ));
                }
            }

            if let Some(expected) = expectations.build_command {
                if entry.build_command.as_deref() != Some(expected) {
                    failures.push(format!(
                        "{display_path}: cross-compilation entry '{name}' expected build_command='{expected}'"
                    ));
                }
            }

            if let Some(expected) = expectations.build_daemon {
                if entry.build_daemon != Some(expected) {
                    failures.push(format!(
                        "{display_path}: cross-compilation entry '{name}' expected build_daemon={expected}"
                    ));
                }
            }

            if let Some(expected) = expectations.uses_zig {
                if entry.uses_zig != Some(expected) {
                    failures.push(format!(
                        "{display_path}: cross-compilation entry '{name}' expected uses_zig={expected}"
                    ));
                }
            }

            if let Some(expected) = expectations.generate_sbom {
                if entry.generate_sbom != Some(expected) {
                    failures.push(format!(
                        "{display_path}: cross-compilation entry '{name}' expected generate_sbom={expected}"
                    ));
                }
            }

            if let Some(expected) = expectations.needs_cross_gcc {
                if entry.needs_cross_gcc != Some(expected) {
                    failures.push(format!(
                        "{display_path}: cross-compilation entry '{name}' expected needs_cross_gcc={expected}"
                    ));
                }
            }
        }
        None => failures.push(format!(
            "{display_path}: missing cross-compilation entry '{name}'"
        )),
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
struct MatrixEntry {
    enabled: Option<bool>,
    target: Option<String>,
    build_command: Option<String>,
    build_daemon: Option<bool>,
    uses_zig: Option<bool>,
    generate_sbom: Option<bool>,
    needs_cross_gcc: Option<bool>,
}

#[derive(Clone, Debug, Default)]
struct MatrixExpectations<'a> {
    enabled: Option<bool>,
    target: Option<&'a str>,
    build_command: Option<&'a str>,
    build_daemon: Option<bool>,
    uses_zig: Option<bool>,
    generate_sbom: Option<bool>,
    needs_cross_gcc: Option<bool>,
}

fn extract_matrix_entry(contents: &str, name: &str) -> Option<MatrixEntry> {
    let mut entry = None;
    let mut capturing = false;

    for line in contents.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("- name:") {
            if capturing {
                break;
            }

            if rest.trim() == name {
                entry = Some(MatrixEntry::default());
                capturing = true;
            }
            continue;
        }

        if !capturing {
            continue;
        }

        if let Some(current) = entry.as_mut() {
            let trimmed = trimmed.trim();
            if let Some(enabled) = trimmed.strip_prefix("enabled:") {
                current.enabled = Some(matches!(enabled.trim(), "true"));
            } else if let Some(target) = trimmed.strip_prefix("target:") {
                current.target = Some(target.trim().to_owned());
            } else if let Some(build_command) = trimmed.strip_prefix("build_command:") {
                current.build_command = Some(build_command.trim().to_owned());
            } else if let Some(build_daemon) = trimmed.strip_prefix("build_daemon:") {
                current.build_daemon = parse_bool(build_daemon);
            } else if let Some(uses_zig) = trimmed.strip_prefix("uses_zig:") {
                current.uses_zig = parse_bool(uses_zig);
            } else if let Some(generate_sbom) = trimmed.strip_prefix("generate_sbom:") {
                current.generate_sbom = parse_bool(generate_sbom);
            } else if let Some(needs_cross_gcc) = trimmed.strip_prefix("needs_cross_gcc:") {
                current.needs_cross_gcc = parse_bool(needs_cross_gcc);
            }
        }
    }

    entry
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn expected_matrix_name(os: &str, arch: &str) -> Option<String> {
    match os {
        "linux" => Some(format!("linux-{arch}")),
        "macos" => Some(format!("darwin-{arch}")),
        "windows" => Some(format!("windows-{arch}")),
        _ => None,
    }
}

fn expected_target(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-gnu"),
        ("windows", "x86") => Some("i686-pc-windows-gnu"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

fn expected_build_command(os: &str) -> &'static str {
    match os {
        "linux" => "build",
        "macos" | "windows" => "zigbuild",
        _ => "build",
    }
}

fn expected_build_daemon(os: &str, _arch: &str) -> bool {
    !matches!(os, "windows")
}

fn expected_uses_zig(os: &str) -> bool {
    matches!(os, "macos" | "windows")
}

fn expected_generate_sbom(os: &str) -> bool {
    !matches!(os, "windows")
}

fn expected_needs_cross_gcc(os: &str, arch: &str) -> Option<bool> {
    match (os, arch) {
        ("linux", "aarch64") => Some(true),
        ("linux", _) => Some(false),
        _ => Some(false),
    }
}

#[cfg(test)]
mod tests;
