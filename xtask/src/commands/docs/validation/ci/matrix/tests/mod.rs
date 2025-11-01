use super::*;
use crate::workspace::load_workspace_branding;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MANIFEST_SNIPPET: &str = r#"[workspace]
members = []
[workspace.metadata]
[workspace.metadata.oc_rsync]
brand = "oc"
upstream_version = "3.4.1"
rust_version = "3.4.1-rust"
protocol = 32
client_bin = "oc-rsync"
daemon_bin = "oc-rsyncd"
legacy_client_bin = "rsync"
legacy_daemon_bin = "rsyncd"
daemon_config_dir = "/etc/oc-rsyncd"
daemon_config = "/etc/oc-rsyncd/oc-rsyncd.conf"
daemon_secrets = "/etc/oc-rsyncd/oc-rsyncd.secrets"
legacy_daemon_config_dir = "/etc"
legacy_daemon_config = "/etc/rsyncd.conf"
legacy_daemon_secrets = "/etc/rsyncd.secrets"
source = "https://github.com/oferchen/rsync"
tarball_target = "x86_64-unknown-linux-gnu"
[workspace.metadata.oc_rsync.cross_compile]
linux = ["x86_64", "aarch64"]
macos = ["x86_64", "aarch64"]
windows = ["x86_64"]
"#;

fn write_manifest(workspace: &Path) {
    if !workspace.exists() {
        fs::create_dir_all(workspace).expect("create workspace root");
    }

    fs::write(workspace.join("Cargo.toml"), MANIFEST_SNIPPET).expect("write manifest");
}

fn write_ci_file(workspace: &Path, contents: &str) {
    let workflows = workspace.join(".github").join("workflows");
    fs::create_dir_all(&workflows).expect("create workflows");
    fs::write(workflows.join("ci.yml"), contents).expect("write ci workflow");
}

fn unique_workspace(prefix: &str) -> std::path::PathBuf {
    let unique_suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}_{unique_suffix}"))
}

mod accepts_workspace;
mod duplicate_entries;
mod extract;
mod mismatched_fields;
mod missing_entries;
mod parallelism;
mod platform_names;
mod unexpected_entries;
