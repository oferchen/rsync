use crate::error::{TaskError, TaskResult};
use crate::workspace::WorkspaceBranding;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tar::{Builder, EntryType, Header, HeaderMode};

/// Linux tarball specification derived from workspace metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TarballSpec {
    /// Architecture label embedded in the output artifact name.
    pub arch: &'static str,
    /// Cargo compilation target triple used to locate binaries.
    pub target_triple: &'static str,
}

/// Returns tarball specifications for each Linux architecture declared in the
/// workspace metadata.
pub(super) fn linux_tarball_specs(branding: &WorkspaceBranding) -> TaskResult<Vec<TarballSpec>> {
    let linux_arches = branding.cross_compile.get("linux").ok_or_else(|| {
        TaskError::Validation(String::from(
            "missing linux entry in [workspace.metadata.oc_rsync.cross_compile]",
        ))
    })?;

    let mut specs = Vec::with_capacity(linux_arches.len());
    for arch in linux_arches {
        let spec = match arch.as_str() {
            "x86_64" => TarballSpec {
                arch: "amd64",
                target_triple: "x86_64-unknown-linux-gnu",
            },
            "aarch64" => TarballSpec {
                arch: "aarch64",
                target_triple: "aarch64-unknown-linux-gnu",
            },
            other => {
                return Err(TaskError::Validation(format!(
                    "unsupported linux tarball architecture '{other}' declared in workspace metadata"
                )));
            }
        };
        specs.push(spec);
    }

    Ok(specs)
}

pub(super) fn build_tarball(
    workspace: &Path,
    branding: &WorkspaceBranding,
    profile: &Option<OsString>,
    spec: &TarballSpec,
) -> TaskResult<()> {
    let profile_name = tarball_profile_name(profile);
    let target_dir = workspace
        .join("target")
        .join(spec.target_triple)
        .join(&profile_name);

    let binaries = [
        (branding.client_bin.as_str(), 0o755),
        (branding.daemon_bin.as_str(), 0o755),
        (branding.legacy_client_bin.as_str(), 0o755),
        (branding.legacy_daemon_bin.as_str(), 0o755),
    ];

    for (name, _) in &binaries {
        let path = target_dir.join(name);
        ensure_tarball_source(&path)?;
    }

    let dist_dir = workspace.join("target").join("dist");
    fs::create_dir_all(&dist_dir)?;

    let root_name = format!(
        "{}-{}-{}",
        branding.client_bin, branding.rust_version, spec.arch
    );
    let tarball_path = dist_dir.join(format!("{root_name}.tar.gz"));
    println!(
        "Building {arch} tar.gz distribution at {path}",
        arch = spec.arch,
        path = tarball_path.display()
    );

    let tarball_file = File::create(&tarball_path).map_err(|error| {
        TaskError::Io(io::Error::new(
            error.kind(),
            format!(
                "failed to create tarball at {}: {error}",
                tarball_path.display()
            ),
        ))
    })?;
    let encoder = GzEncoder::new(tarball_file, Compression::default());
    let mut builder = Builder::new(encoder);
    builder.mode(HeaderMode::Deterministic);

    let directories = [
        root_name.clone(),
        format!("{root_name}/bin"),
        format!("{root_name}/libexec"),
        format!("{root_name}/libexec/oc-rsync"),
        format!("{root_name}/lib"),
        format!("{root_name}/lib/systemd"),
        format!("{root_name}/lib/systemd/system"),
        format!("{root_name}/etc"),
        format!("{root_name}/etc/oc-rsyncd"),
        format!("{root_name}/etc/default"),
    ];

    for directory in &directories {
        append_directory_entry(&mut builder, directory, 0o755)?;
    }

    let packaging_entries: Vec<(PathBuf, &str, u32)> = vec![
        (
            workspace.join("packaging/systemd/oc-rsyncd.service"),
            "lib/systemd/system/oc-rsyncd.service",
            0o644,
        ),
        (
            workspace.join("packaging/etc/oc-rsyncd/oc-rsyncd.conf"),
            "etc/oc-rsyncd/oc-rsyncd.conf",
            0o644,
        ),
        (
            workspace.join("packaging/etc/oc-rsyncd/oc-rsyncd.secrets"),
            "etc/oc-rsyncd/oc-rsyncd.secrets",
            0o600,
        ),
        (
            workspace.join("packaging/default/oc-rsyncd"),
            "etc/default/oc-rsyncd",
            0o644,
        ),
        (workspace.join("LICENSE"), "LICENSE", 0o644),
        (workspace.join("README.md"), "README.md", 0o644),
    ];

    for (source, _, _) in &packaging_entries {
        ensure_tarball_source(source)?;
    }

    let mut append_binary = |name: &str, relative: &str, mode: u32| -> TaskResult<()> {
        let source = target_dir.join(name);
        let destination = format!("{root_name}/{relative}");
        append_file_entry(&mut builder, &destination, &source, mode)
    };

    append_binary(
        branding.client_bin.as_str(),
        &format!("bin/{}", branding.client_bin),
        0o755,
    )?;
    append_binary(
        branding.daemon_bin.as_str(),
        &format!("bin/{}", branding.daemon_bin),
        0o755,
    )?;
    append_binary(
        branding.legacy_client_bin.as_str(),
        &format!("libexec/oc-rsync/{}", branding.legacy_client_bin),
        0o755,
    )?;
    append_binary(
        branding.legacy_daemon_bin.as_str(),
        &format!("libexec/oc-rsync/{}", branding.legacy_daemon_bin),
        0o755,
    )?;

    for (source, relative, mode) in &packaging_entries {
        let destination = format!("{root_name}/{relative}");
        append_file_entry(&mut builder, &destination, source, *mode)?;
    }

    let encoder = builder.into_inner()?;
    encoder.finish()?;

    Ok(())
}

fn tarball_profile_name(profile: &Option<OsString>) -> String {
    profile
        .as_ref()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("debug"))
}

fn ensure_tarball_source(path: &Path) -> TaskResult<()> {
    if path.is_file() {
        return Ok(());
    }

    Err(TaskError::Validation(format!(
        "tarball source file missing: {}",
        path.display()
    )))
}

fn append_directory_entry<W: Write>(
    builder: &mut Builder<W>,
    path: &str,
    mode: u32,
) -> TaskResult<()> {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Directory);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(0);
    header.set_path(path)?;
    header.set_cksum();
    builder.append(&header, io::empty())?;
    Ok(())
}

fn append_file_entry<W: Write>(
    builder: &mut Builder<W>,
    destination: &str,
    source: &Path,
    mode: u32,
) -> TaskResult<()> {
    let metadata = fs::metadata(source).map_err(|error| {
        TaskError::Io(io::Error::new(
            error.kind(),
            format!("failed to read metadata for {}: {error}", source.display()),
        ))
    })?;

    if !metadata.is_file() {
        return Err(TaskError::Validation(format!(
            "expected regular file for tarball entry: {}",
            source.display()
        )));
    }

    let mut file = File::open(source).map_err(|error| {
        TaskError::Io(io::Error::new(
            error.kind(),
            format!("failed to open {}: {error}", source.display()),
        ))
    })?;

    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(metadata.len());
    header.set_path(destination)?;
    header.set_cksum();
    builder.append(&header, &mut file)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn branding_with_linux_arches(arches: &[&str]) -> WorkspaceBranding {
        let mut cross_compile = BTreeMap::new();
        cross_compile.insert(
            String::from("linux"),
            arches.iter().map(|arch| arch.to_string()).collect(),
        );

        WorkspaceBranding {
            brand: String::from("oc"),
            upstream_version: String::from("3.4.1"),
            rust_version: String::from("3.4.1-rust"),
            protocol: 32,
            client_bin: String::from("oc-rsync"),
            daemon_bin: String::from("oc-rsyncd"),
            legacy_client_bin: String::from("rsync"),
            legacy_daemon_bin: String::from("rsyncd"),
            daemon_config_dir: PathBuf::from("/etc/oc-rsyncd"),
            daemon_config: PathBuf::from("/etc/oc-rsyncd/oc-rsyncd.conf"),
            daemon_secrets: PathBuf::from("/etc/oc-rsyncd/oc-rsyncd.secrets"),
            legacy_daemon_config_dir: PathBuf::from("/etc"),
            legacy_daemon_config: PathBuf::from("/etc/rsyncd.conf"),
            legacy_daemon_secrets: PathBuf::from("/etc/rsyncd.secrets"),
            source: String::from("https://example.invalid"),
            cross_compile,
        }
    }

    #[test]
    fn linux_tarball_specs_accepts_supported_architectures() {
        let branding = branding_with_linux_arches(&["x86_64", "aarch64"]);
        let specs = linux_tarball_specs(&branding).expect("spec extraction succeeds");
        assert_eq!(
            specs,
            vec![
                TarballSpec {
                    arch: "amd64",
                    target_triple: "x86_64-unknown-linux-gnu",
                },
                TarballSpec {
                    arch: "aarch64",
                    target_triple: "aarch64-unknown-linux-gnu",
                },
            ]
        );
    }

    #[test]
    fn linux_tarball_specs_rejects_unknown_architecture() {
        let branding = branding_with_linux_arches(&["sparc64"]);
        let error = linux_tarball_specs(&branding).unwrap_err();
        assert!(
            matches!(error, TaskError::Validation(message) if message.contains("unsupported linux tarball architecture"))
        );
    }

    #[test]
    fn linux_tarball_specs_require_linux_entry() {
        let mut branding = branding_with_linux_arches(&[]);
        branding.cross_compile.clear();
        let error = linux_tarball_specs(&branding).unwrap_err();
        assert!(
            matches!(error, TaskError::Validation(message) if message.contains("missing linux entry"))
        );
    }
}
