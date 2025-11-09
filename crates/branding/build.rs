use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use toml::Table;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));

    println!("cargo:rerun-if-env-changed=CARGO_WORKSPACE_DIR");
    println!("cargo:rerun-if-env-changed=CARGO_MANIFEST_DIR");
    println!("cargo:rerun-if-env-changed=OC_RSYNC_BUILD_OVERRIDE");

    if let Some(git_dir) = git_directory(&manifest_dir) {
        emit_rerun_if_exists(&git_dir.join("HEAD"));
        emit_rerun_if_exists(&git_dir.join("refs/heads"));
        emit_rerun_if_exists(&git_dir.join("refs/remotes"));
        emit_rerun_if_exists(&git_dir.join("packed-refs"));
    }

    let workspace_root = workspace_root(&manifest_dir).unwrap_or_else(|| manifest_dir.clone());
    emit_rerun_if_exists(&workspace_root.join("Cargo.toml"));

    let metadata = load_workspace_metadata(&workspace_root);
    metadata.validate(&workspace_root.join("Cargo.toml"));

    let build_revision = determine_build_revision(&manifest_dir);

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let dest = out_dir.join("workspace_generated.rs");

    fs::write(&dest, metadata.render(&build_revision))
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", dest.display()));
}

fn determine_build_revision(manifest_dir: &Path) -> String {
    if let Some(override_revision) = env::var("OC_RSYNC_BUILD_OVERRIDE").ok() {
        let trimmed = override_revision.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }

    git_revision(manifest_dir).unwrap_or_else(|| "unknown".to_owned())
}

fn git_revision(manifest_dir: &Path) -> Option<String> {
    run_git(manifest_dir, &["rev-parse", "--short", "HEAD"])
}

fn git_directory(manifest_dir: &Path) -> Option<PathBuf> {
    run_git(manifest_dir, &["rev-parse", "--git-dir"]).map(|output| {
        let path = PathBuf::from(output);
        if path.is_relative() {
            manifest_dir.join(path)
        } else {
            path
        }
    })
}

fn workspace_root(manifest_dir: &Path) -> Option<PathBuf> {
    run_git(manifest_dir, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

fn run_git(manifest_dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(manifest_dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn emit_rerun_if_exists(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn load_workspace_metadata(workspace_root: &Path) -> WorkspaceMetadata {
    let manifest_path = workspace_root.join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", manifest_path.display()));
    let table: Table = manifest
        .parse()
        .unwrap_or_else(|err| panic!("failed to parse {}: {err}", manifest_path.display()));

    let workspace_table = table
        .get("workspace")
        .and_then(toml::Value::as_table)
        .unwrap_or_else(|| {
            panic!(
                "workspace.metadata.oc_rsync missing in {}",
                manifest_path.display()
            )
        });

    let metadata_table = workspace_table
        .get("metadata")
        .and_then(toml::Value::as_table)
        .unwrap_or_else(|| {
            panic!(
                "workspace.metadata.oc_rsync missing in {}",
                manifest_path.display()
            )
        });

    let rsync_table = metadata_table
        .get("oc_rsync")
        .and_then(toml::Value::as_table)
        .unwrap_or_else(|| {
            panic!(
                "workspace.metadata.oc_rsync missing in {}",
                manifest_path.display()
            )
        });

    WorkspaceMetadata {
        brand: str_field(rsync_table, "brand"),
        upstream_version: str_field(rsync_table, "upstream_version"),
        rust_version: str_field(rsync_table, "rust_version"),
        protocol: int_field(rsync_table, "protocol"),
        client_bin: str_field(rsync_table, "client_bin"),
        daemon_bin: str_field(rsync_table, "daemon_bin"),
        legacy_client_bin: str_field(rsync_table, "legacy_client_bin"),
        legacy_daemon_bin: str_field(rsync_table, "legacy_daemon_bin"),
        daemon_config_dir: str_field(rsync_table, "daemon_config_dir"),
        daemon_config: str_field(rsync_table, "daemon_config"),
        daemon_secrets: str_field(rsync_table, "daemon_secrets"),
        legacy_daemon_config_dir: str_field(rsync_table, "legacy_daemon_config_dir"),
        legacy_daemon_config: str_field(rsync_table, "legacy_daemon_config"),
        legacy_daemon_secrets: str_field(rsync_table, "legacy_daemon_secrets"),
        source: str_field(rsync_table, "source"),
    }
}

fn str_field<'a>(table: &'a Table, key: &str) -> String {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| panic!("workspace.metadata.oc_rsync.{key} missing"))
        .to_owned()
}

fn int_field(table: &Table, key: &str) -> u32 {
    table
        .get(key)
        .and_then(toml::Value::as_integer)
        .unwrap_or_else(|| panic!("workspace.metadata.oc_rsync.{key} missing")) as u32
}

struct WorkspaceMetadata {
    brand: String,
    upstream_version: String,
    rust_version: String,
    protocol: u32,
    client_bin: String,
    daemon_bin: String,
    legacy_client_bin: String,
    legacy_daemon_bin: String,
    daemon_config_dir: String,
    daemon_config: String,
    daemon_secrets: String,
    legacy_daemon_config_dir: String,
    legacy_daemon_config: String,
    legacy_daemon_secrets: String,
    source: String,
}

impl WorkspaceMetadata {
    fn validate(&self, manifest_path: &Path) {
        self.validate_branding(manifest_path);
        self.validate_versions(manifest_path);
        self.validate_protocol(manifest_path);
        self.validate_legacy_paths(manifest_path);
        self.validate_source(manifest_path);
    }

    fn validate_branding(&self, manifest_path: &Path) {
        match self.brand.as_str() {
            "oc" => {
                expect_eq(&self.client_bin, "oc-rsync", manifest_path, "client_bin");
                expect_eq(&self.daemon_bin, "oc-rsync", manifest_path, "daemon_bin");
                expect_eq(
                    &self.daemon_config_dir,
                    "/etc/oc-rsyncd",
                    manifest_path,
                    "daemon_config_dir",
                );
                expect_eq(
                    &self.daemon_config,
                    "/etc/oc-rsyncd/oc-rsyncd.conf",
                    manifest_path,
                    "daemon_config",
                );
                expect_eq(
                    &self.daemon_secrets,
                    "/etc/oc-rsyncd/oc-rsyncd.secrets",
                    manifest_path,
                    "daemon_secrets",
                );
            }
            "upstream" => {
                expect_eq(&self.client_bin, "rsync", manifest_path, "client_bin");
                expect_eq(&self.daemon_bin, "rsyncd", manifest_path, "daemon_bin");
                expect_eq(
                    &self.daemon_config_dir,
                    "/etc",
                    manifest_path,
                    "daemon_config_dir",
                );
                expect_eq(
                    &self.daemon_config,
                    "/etc/rsyncd.conf",
                    manifest_path,
                    "daemon_config",
                );
                expect_eq(
                    &self.daemon_secrets,
                    "/etc/rsyncd.secrets",
                    manifest_path,
                    "daemon_secrets",
                );
            }
            other => panic!(
                "unsupported workspace brand '{other}' in {}",
                manifest_path.display()
            ),
        }
    }

    fn validate_versions(&self, manifest_path: &Path) {
        let expected = format!("{}-rust", self.upstream_version);
        expect_eq(&self.rust_version, &expected, manifest_path, "rust_version");
    }

    fn validate_protocol(&self, manifest_path: &Path) {
        if !(28..=32).contains(&self.protocol) {
            panic!(
                "workspace.metadata.oc_rsync.protocol must be between 28 and 32 in {}",
                manifest_path.display()
            );
        }
    }

    fn validate_legacy_paths(&self, manifest_path: &Path) {
        expect_eq(
            &self.legacy_client_bin,
            "rsync",
            manifest_path,
            "legacy_client_bin",
        );
        expect_eq(
            &self.legacy_daemon_bin,
            "rsyncd",
            manifest_path,
            "legacy_daemon_bin",
        );
        expect_eq(
            &self.legacy_daemon_config_dir,
            "/etc",
            manifest_path,
            "legacy_daemon_config_dir",
        );
        expect_eq(
            &self.legacy_daemon_config,
            "/etc/rsyncd.conf",
            manifest_path,
            "legacy_daemon_config",
        );
        expect_eq(
            &self.legacy_daemon_secrets,
            "/etc/rsyncd.secrets",
            manifest_path,
            "legacy_daemon_secrets",
        );
    }

    fn validate_source(&self, manifest_path: &Path) {
        if self.source.is_empty() {
            panic!(
                "workspace.metadata.oc_rsync.source must not be empty in {}",
                manifest_path.display()
            );
        }
    }

    fn render(&self, build_revision: &str) -> String {
        let mut output = String::new();
        output.push_str("pub const BRAND: &str = \"");
        output.push_str(&self.brand);
        output.push_str("\";\n");
        output.push_str("pub const UPSTREAM_VERSION: &str = \"");
        output.push_str(&self.upstream_version);
        output.push_str("\";\n");
        output.push_str("pub const RUST_VERSION: &str = \"");
        output.push_str(&self.rust_version);
        output.push_str("\";\n");
        output.push_str("pub const PROTOCOL_VERSION: u32 = ");
        output.push_str(&self.protocol.to_string());
        output.push_str(";\n");
        output.push_str("pub const CLIENT_PROGRAM_NAME: &str = \"");
        output.push_str(&self.client_bin);
        output.push_str("\";\n");
        output.push_str("pub const DAEMON_PROGRAM_NAME: &str = \"");
        output.push_str(&self.daemon_bin);
        output.push_str("\";\n");
        output.push_str("pub const LEGACY_CLIENT_PROGRAM_NAME: &str = \"");
        output.push_str(&self.legacy_client_bin);
        output.push_str("\";\n");
        output.push_str("pub const LEGACY_DAEMON_PROGRAM_NAME: &str = \"");
        output.push_str(&self.legacy_daemon_bin);
        output.push_str("\";\n");
        output.push_str("pub const DAEMON_CONFIG_DIR: &str = \"");
        output.push_str(&self.daemon_config_dir);
        output.push_str("\";\n");
        output.push_str("pub const DAEMON_CONFIG_PATH: &str = \"");
        output.push_str(&self.daemon_config);
        output.push_str("\";\n");
        output.push_str("pub const DAEMON_SECRETS_PATH: &str = \"");
        output.push_str(&self.daemon_secrets);
        output.push_str("\";\n");
        output.push_str("pub const LEGACY_DAEMON_CONFIG_DIR: &str = \"");
        output.push_str(&self.legacy_daemon_config_dir);
        output.push_str("\";\n");
        output.push_str("pub const LEGACY_DAEMON_CONFIG_PATH: &str = \"");
        output.push_str(&self.legacy_daemon_config);
        output.push_str("\";\n");
        output.push_str("pub const LEGACY_DAEMON_SECRETS_PATH: &str = \"");
        output.push_str(&self.legacy_daemon_secrets);
        output.push_str("\";\n");
        output.push_str("pub const SOURCE_URL: &str = \"");
        output.push_str(&self.source);
        output.push_str("\";\n");
        output.push_str("/// Human-readable toolchain description embedded at compile time.\n");
        output.push_str("pub const BUILD_TOOLCHAIN: &str = \"Built in Rust 2024\";\n");
        output.push_str("/// Sanitised build revision derived from the workspace VCS state.\n");
        output.push_str("pub const BUILD_REVISION: &str = \"");
        output.push_str(&sanitize_revision(build_revision));
        output.push_str("\";\n");
        output
    }
}

fn expect_eq(actual: &str, expected: &str, manifest_path: &Path, field: &str) {
    if actual != expected {
        panic!(
            "workspace.metadata.oc_rsync.{field} must be '{expected}' in {} but was '{actual}'",
            manifest_path.display()
        );
    }
}

fn sanitize_revision(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return "unknown".to_owned();
    }

    let head = trimmed.split(['\r', '\n']).next().unwrap_or("");
    let cleaned = head.trim();
    if cleaned.is_empty() || cleaned.chars().any(char::is_control) {
        "unknown".to_owned()
    } else {
        cleaned.to_owned()
    }
}
