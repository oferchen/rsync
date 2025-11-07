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
    println!(
        "cargo:rustc-env=RSYNC_WORKSPACE_ROOT={}",
        workspace_root.display()
    );

    emit_rerun_if_exists(&workspace_root.join("Cargo.toml"));

    let metadata = load_workspace_metadata(&workspace_root);
    metadata.emit_cargo_env();

    let build_id = env::var("OC_RSYNC_BUILD_OVERRIDE")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| git_revision(&manifest_dir))
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=OC_RSYNC_BUILD_REV={build_id}");
}

fn git_revision(manifest_dir: &Path) -> Option<String> {
    run_git(manifest_dir, &["rev-parse", "--short", "HEAD"])
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

    let oc_rsync_table = metadata_table
        .get("oc_rsync")
        .and_then(toml::Value::as_table)
        .unwrap_or_else(|| {
            panic!(
                "workspace.metadata.oc_rsync missing in {}",
                manifest_path.display()
            )
        });

    let metadata = WorkspaceMetadata {
        brand: str_field(oc_rsync_table, "brand"),
        upstream_version: str_field(oc_rsync_table, "upstream_version"),
        rust_version: str_field(oc_rsync_table, "rust_version"),
        protocol: int_field(oc_rsync_table, "protocol"),
        client_bin: str_field(oc_rsync_table, "client_bin"),
        daemon_bin: str_field(oc_rsync_table, "daemon_bin"),
        daemon_wrapper_bin: str_field(oc_rsync_table, "daemon_wrapper_bin"),
        legacy_client_bin: str_field(oc_rsync_table, "legacy_client_bin"),
        legacy_daemon_bin: str_field(oc_rsync_table, "legacy_daemon_bin"),
        daemon_config_dir: str_field(oc_rsync_table, "daemon_config_dir"),
        daemon_config: str_field(oc_rsync_table, "daemon_config"),
        daemon_secrets: str_field(oc_rsync_table, "daemon_secrets"),
        legacy_daemon_config_dir: str_field(oc_rsync_table, "legacy_daemon_config_dir"),
        legacy_daemon_config: str_field(oc_rsync_table, "legacy_daemon_config"),
        legacy_daemon_secrets: str_field(oc_rsync_table, "legacy_daemon_secrets"),
        source: str_field(oc_rsync_table, "source"),
        web_site: str_field(oc_rsync_table, "web_site"),
    };

    metadata.validate(&manifest_path);
    metadata
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

struct WorkspaceMetadata {
    brand: String,
    upstream_version: String,
    rust_version: String,
    protocol: u32,
    client_bin: String,
    daemon_bin: String,
    daemon_wrapper_bin: String,
    legacy_client_bin: String,
    legacy_daemon_bin: String,
    daemon_config_dir: String,
    daemon_config: String,
    daemon_secrets: String,
    legacy_daemon_config_dir: String,
    legacy_daemon_config: String,
    legacy_daemon_secrets: String,
    source: String,
    web_site: String,
}

impl WorkspaceMetadata {
    fn validate(&self, manifest_path: &Path) {
        self.validate_branding(manifest_path);
        self.validate_versions(manifest_path);
        self.validate_protocol(manifest_path);
        self.validate_legacy_paths(manifest_path);
        self.validate_links(manifest_path);
    }

    fn validate_branding(&self, manifest_path: &Path) {
        match self.brand.as_str() {
            "oc" => {
                expect_eq(&self.client_bin, "oc-rsync", manifest_path, "client_bin");
                expect_eq(&self.daemon_bin, "oc-rsync", manifest_path, "daemon_bin");
                expect_eq(
                    &self.daemon_wrapper_bin,
                    "oc-rsyncd",
                    manifest_path,
                    "daemon_wrapper_bin",
                );
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
                    &self.daemon_wrapper_bin,
                    "rsyncd",
                    manifest_path,
                    "daemon_wrapper_bin",
                );
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
            other => {
                panic!(
                    "unsupported workspace brand '{}' in {}",
                    other,
                    manifest_path.display()
                );
            }
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

    fn validate_links(&self, manifest_path: &Path) {
        validate_non_empty(&self.source, manifest_path, "source");
        validate_non_empty(&self.web_site, manifest_path, "web_site");
    }

    fn emit_cargo_env(&self) {
        println!("cargo:rustc-env=OC_RSYNC_WORKSPACE_BRAND={}", self.brand);
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_UPSTREAM_VERSION={}",
            self.upstream_version
        );
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_RUST_VERSION={}",
            self.rust_version
        );
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_PROTOCOL={}",
            self.protocol
        );
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_CLIENT_BIN={}",
            self.client_bin
        );
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_DAEMON_BIN={}",
            self.daemon_bin
        );
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_DAEMON_WRAPPER_BIN={}",
            self.daemon_wrapper_bin
        );
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_LEGACY_CLIENT_BIN={}",
            self.legacy_client_bin
        );
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_LEGACY_DAEMON_BIN={}",
            self.legacy_daemon_bin
        );
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_DAEMON_CONFIG_DIR={}",
            self.daemon_config_dir
        );
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_DAEMON_CONFIG={}",
            self.daemon_config
        );
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_DAEMON_SECRETS={}",
            self.daemon_secrets
        );
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_LEGACY_DAEMON_CONFIG_DIR={}",
            self.legacy_daemon_config_dir
        );
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_LEGACY_DAEMON_CONFIG={}",
            self.legacy_daemon_config
        );
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_LEGACY_DAEMON_SECRETS={}",
            self.legacy_daemon_secrets
        );
        println!("cargo:rustc-env=OC_RSYNC_WORKSPACE_SOURCE={}", self.source);
        println!(
            "cargo:rustc-env=OC_RSYNC_WORKSPACE_WEB_SITE={}",
            self.web_site
        );
    }
}

fn expect_eq(actual: &str, expected: &str, manifest_path: &Path, field: &str) {
    if actual != expected {
        panic!(
            "workspace.metadata.oc_rsync.{field} must be '{expected}' (found '{actual}') in {}",
            manifest_path.display()
        );
    }
}

fn str_field(table: &Table, key: &str) -> String {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| panic!("missing string field {key} in workspace.metadata.oc_rsync"))
        .to_owned()
}

fn validate_non_empty(value: &str, manifest_path: &Path, field: &str) {
    if value.trim().is_empty() {
        panic!(
            "workspace.metadata.oc_rsync.{field} must not be empty in {}",
            manifest_path.display()
        );
    }
}

fn int_field(table: &Table, key: &str) -> u32 {
    let value = table
        .get(key)
        .and_then(toml::Value::as_integer)
        .unwrap_or_else(|| panic!("missing integer field {key} in workspace.metadata.oc_rsync"));
    u32::try_from(value)
        .unwrap_or_else(|_| panic!("value for {key} must fit within a u32: {value}"))
}
