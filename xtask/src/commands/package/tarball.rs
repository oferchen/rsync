use super::{AMD64_TARBALL_ARCH, AMD64_TARBALL_TARGET};
use crate::error::{TaskError, TaskResult};
use crate::workspace::WorkspaceBranding;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tar::{Builder, EntryType, Header, HeaderMode};

pub(super) fn build_amd64_tarball(
    workspace: &Path,
    branding: &WorkspaceBranding,
    profile: &Option<OsString>,
) -> TaskResult<()> {
    let profile_name = tarball_profile_name(profile);
    let target_dir = workspace
        .join("target")
        .join(AMD64_TARBALL_TARGET)
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
        branding.client_bin, branding.rust_version, AMD64_TARBALL_ARCH
    );
    let tarball_path = dist_dir.join(format!("{root_name}.tar.gz"));
    println!(
        "Building amd64 tar.gz distribution at {}",
        tarball_path.display()
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
