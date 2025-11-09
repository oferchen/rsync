use super::DIST_PROFILE;
use crate::error::{TaskError, TaskResult};
use crate::workspace::WorkspaceBranding;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::collections::{BTreeSet, HashSet};
use std::env;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tar::{Builder, EntryType, Header, HeaderMode};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TarballPlatform {
    Linux,
    Macos,
    Windows,
}

impl TarballPlatform {
    fn archive_tag(self) -> &'static str {
        match self {
            TarballPlatform::Linux => "linux",
            TarballPlatform::Macos => "darwin",
            TarballPlatform::Windows => "windows",
        }
    }

    fn binary_extension(self) -> &'static str {
        match self {
            TarballPlatform::Windows => ".exe",
            TarballPlatform::Linux | TarballPlatform::Macos => "",
        }
    }

    fn includes_daemon(self) -> bool {
        !matches!(self, TarballPlatform::Windows)
    }

    fn directories(self, root: &str) -> Vec<String> {
        let mut directories = vec![
            root.to_string(),
            format!("{root}/bin"),
            format!("{root}/libexec"),
            format!("{root}/libexec/oc-rsync"),
        ];

        if matches!(self, TarballPlatform::Linux) {
            directories.extend_from_slice(&[
                format!("{root}/lib"),
                format!("{root}/lib/systemd"),
                format!("{root}/lib/systemd/system"),
                format!("{root}/etc"),
                format!("{root}/etc/oc-rsyncd"),
                format!("{root}/etc/default"),
            ]);
        }

        directories
    }

    fn packaging_entries(self, workspace: &Path) -> Vec<(PathBuf, String, u32)> {
        let mut entries = vec![
            (workspace.join("LICENSE"), String::from("LICENSE"), 0o644),
            (
                workspace.join("README.md"),
                String::from("README.md"),
                0o644,
            ),
        ];

        if matches!(self, TarballPlatform::Linux) {
            entries.extend_from_slice(&[
                (
                    workspace.join("packaging/systemd/oc-rsyncd.service"),
                    String::from("lib/systemd/system/oc-rsyncd.service"),
                    0o644,
                ),
                (
                    workspace.join("packaging/etc/oc-rsyncd/oc-rsyncd.conf"),
                    String::from("etc/oc-rsyncd/oc-rsyncd.conf"),
                    0o644,
                ),
                (
                    workspace.join("packaging/etc/oc-rsyncd/oc-rsyncd.secrets"),
                    String::from("etc/oc-rsyncd/oc-rsyncd.secrets"),
                    0o600,
                ),
                (
                    workspace.join("packaging/default/oc-rsyncd"),
                    String::from("etc/default/oc-rsyncd"),
                    0o644,
                ),
            ]);
        }

        entries
    }

    fn host() -> Option<Self> {
        if cfg!(target_os = "linux") {
            Some(TarballPlatform::Linux)
        } else if cfg!(target_os = "macos") {
            Some(TarballPlatform::Macos)
        } else if cfg!(target_os = "windows") {
            Some(TarballPlatform::Windows)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TarballSpec {
    pub platform: TarballPlatform,
    pub arch: &'static str,
    pub metadata_arch: &'static str,
    pub target_triple: &'static str,
}

impl TarballSpec {
    pub fn display_name(&self) -> String {
        format!("{}-{}", self.platform.archive_tag(), self.metadata_arch)
    }

    pub fn binary_extension(&self) -> &'static str {
        self.platform.binary_extension()
    }

    pub fn includes_daemon(&self) -> bool {
        self.platform.includes_daemon()
    }

    pub fn requires_cross_compiler(&self) -> bool {
        if !matches!(self.platform, TarballPlatform::Linux) {
            return false;
        }

        let host_arch = env::consts::ARCH;
        self.metadata_arch != host_arch
    }
}

pub(super) fn tarball_specs(
    branding: &WorkspaceBranding,
    target_filter: Option<&OsStr>,
) -> TaskResult<Vec<TarballSpec>> {
    let mut specs = Vec::new();
    specs.extend(platform_specs(branding, TarballPlatform::Linux)?);
    specs.extend(platform_specs(branding, TarballPlatform::Macos)?);
    specs.extend(platform_specs(branding, TarballPlatform::Windows)?);

    if let Some(filter) = target_filter {
        let filter_value = filter.to_string_lossy();
        if let Some(spec) = specs
            .into_iter()
            .find(|candidate| candidate.target_triple == filter_value)
        {
            return Ok(vec![spec]);
        }

        return Err(TaskError::Validation(format!(
            "unknown tarball target triple '{filter_value}'"
        )));
    }

    let Some(host_platform) = TarballPlatform::host() else {
        return Err(TaskError::Validation(String::from(
            "tarball packaging is unsupported on this host platform",
        )));
    };

    let filtered: Vec<_> = specs
        .into_iter()
        .filter(|spec| spec.platform == host_platform)
        .collect();

    if filtered.is_empty() {
        return Err(TaskError::Validation(format!(
            "no tarball specifications available for host platform {}",
            host_platform.archive_tag(),
        )));
    }

    Ok(filtered)
}

fn platform_specs(
    branding: &WorkspaceBranding,
    platform: TarballPlatform,
) -> TaskResult<Vec<TarballSpec>> {
    let key = match platform {
        TarballPlatform::Linux => "linux",
        TarballPlatform::Macos => "macos",
        TarballPlatform::Windows => "windows",
    };

    let Some(arches) = branding.cross_compile.get(key) else {
        return Ok(Vec::new());
    };

    let mut specs = Vec::with_capacity(arches.len());

    for arch in arches {
        let spec = match (platform, arch.as_str()) {
            (TarballPlatform::Linux, "x86_64") => TarballSpec {
                platform,
                arch: "amd64",
                metadata_arch: "x86_64",
                target_triple: "x86_64-unknown-linux-gnu",
            },
            (TarballPlatform::Linux, "aarch64") => TarballSpec {
                platform,
                arch: "aarch64",
                metadata_arch: "aarch64",
                target_triple: "aarch64-unknown-linux-gnu",
            },
            (TarballPlatform::Macos, "x86_64") => TarballSpec {
                platform,
                arch: "x86_64",
                metadata_arch: "x86_64",
                target_triple: "x86_64-apple-darwin",
            },
            (TarballPlatform::Macos, "aarch64") => TarballSpec {
                platform,
                arch: "aarch64",
                metadata_arch: "aarch64",
                target_triple: "aarch64-apple-darwin",
            },
            (TarballPlatform::Windows, "x86_64") => TarballSpec {
                platform,
                arch: "x86_64",
                metadata_arch: "x86_64",
                target_triple: "x86_64-pc-windows-msvc",
            },
            (TarballPlatform::Windows, "aarch64") => TarballSpec {
                platform,
                arch: "aarch64",
                metadata_arch: "aarch64",
                target_triple: "aarch64-pc-windows-msvc",
            },
            _ => {
                return Err(TaskError::Validation(format!(
                    "unsupported {key} tarball architecture '{arch}' declared in workspace metadata",
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
    let skip_build = env::var_os("OC_RSYNC_PACKAGE_SKIP_BUILD").is_some();

    let extension = spec.binary_extension();
    let root_name = format!(
        "{}-{}-{}-{}",
        branding.client_bin,
        branding.rust_version,
        spec.platform.archive_tag(),
        spec.arch
    );

    let dist_dir = workspace.join("target").join("dist");
    fs::create_dir_all(&dist_dir)?;

    let tarball_path = dist_dir.join(format!("{root_name}.tar.gz"));
    println!(
        "Building {display} tar.gz distribution at {path}",
        display = spec.display_name(),
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

    for directory in spec.platform.directories(&root_name) {
        append_directory_entry(&mut builder, &directory, 0o755)?;
    }

    for (source, relative, mode) in spec.platform.packaging_entries(workspace) {
        ensure_tarball_source(&source)?;
        let destination = format!("{root_name}/{relative}");
        append_file_entry(&mut builder, &destination, &source, mode)?;
    }

    let binaries = [
        (
            branding.client_bin.as_str(),
            format!("bin/{}{}", branding.client_bin, extension),
            true,
        ),
        (
            branding.daemon_bin.as_str(),
            format!("bin/{}{}", branding.daemon_bin, extension),
            spec.includes_daemon(),
        ),
        (
            branding.legacy_client_bin.as_str(),
            format!(
                "libexec/oc-rsync/{}{}",
                branding.legacy_client_bin, extension
            ),
            true,
        ),
        (
            branding.legacy_daemon_bin.as_str(),
            format!(
                "libexec/oc-rsync/{}{}",
                branding.legacy_daemon_bin, extension
            ),
            spec.includes_daemon(),
        ),
    ];

    let mut seen_destinations = BTreeSet::new();
    for (name, relative, include) in &binaries {
        if !include {
            continue;
        }

        let file_name = format!("{name}{extension}");
        let path = resolve_binary_path(workspace, spec, profile, skip_build, &file_name)?;
        ensure_tarball_source(&path)?;
        let destination = format!("{root_name}/{relative}");
        if !seen_destinations.insert(destination.clone()) {
            continue;
        }
        append_file_entry(&mut builder, &destination, &path, 0o755)?;
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

fn resolve_binary_path(
    workspace: &Path,
    spec: &TarballSpec,
    profile: &Option<OsString>,
    skip_build: bool,
    file_name: &str,
) -> TaskResult<PathBuf> {
    let directories = tarball_binary_directories(workspace, spec, profile, skip_build);
    let mut primary_candidate = None;

    for directory in directories {
        let candidate = directory.join(file_name);
        if primary_candidate.is_none() {
            primary_candidate = Some(candidate.clone());
        }

        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    let missing_path = primary_candidate.unwrap_or_else(|| {
        workspace
            .join("target")
            .join(spec.target_triple)
            .join(tarball_profile_name(profile))
            .join(file_name)
    });

    Err(TaskError::Validation(format!(
        "tarball source file missing: {}",
        missing_path.display()
    )))
}

fn tarball_binary_directories(
    workspace: &Path,
    spec: &TarballSpec,
    profile: &Option<OsString>,
    skip_build: bool,
) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    let mut seen = HashSet::new();
    let base = workspace.join("target").join(spec.target_triple);

    let mut push_directory = |profile_name: String| {
        if seen.insert(profile_name.clone()) {
            directories.push(base.join(&profile_name));
        }
    };

    push_directory(tarball_profile_name(profile));

    if skip_build {
        for fallback in [DIST_PROFILE, "release", "debug"] {
            push_directory(fallback.to_string());
        }
    }

    directories
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
    use crate::test_support;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::tempdir;

    fn branding_with_cross_compile(
        linux_arches: &[&str],
        mac_arches: &[&str],
        windows_arches: &[&str],
    ) -> WorkspaceBranding {
        let mut cross_compile = BTreeMap::new();
        cross_compile.insert(
            String::from("linux"),
            linux_arches.iter().map(|arch| arch.to_string()).collect(),
        );
        cross_compile.insert(
            String::from("macos"),
            mac_arches.iter().map(|arch| arch.to_string()).collect(),
        );
        cross_compile.insert(
            String::from("windows"),
            windows_arches.iter().map(|arch| arch.to_string()).collect(),
        );

        let mut branding = test_support::workspace_branding_snapshot();
        branding.cross_compile = cross_compile;
        branding.cross_compile_matrix = BTreeMap::new();
        branding
    }

    #[test]
    fn tarball_specs_supports_linux_architectures() {
        let branding = branding_with_cross_compile(&["x86_64", "aarch64"], &[], &[]);
        let target = OsString::from("x86_64-unknown-linux-gnu");
        let specs = tarball_specs(&branding, Some(target.as_os_str())).expect("spec extraction");
        assert_eq!(
            specs,
            vec![TarballSpec {
                platform: TarballPlatform::Linux,
                arch: "amd64",
                metadata_arch: "x86_64",
                target_triple: "x86_64-unknown-linux-gnu",
            }]
        );
    }

    #[test]
    fn tarball_specs_rejects_unknown_architecture() {
        let branding = branding_with_cross_compile(&["sparc64"], &[], &[]);
        let error = tarball_specs(&branding, None).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message) if message.contains("unsupported linux tarball architecture")
        ));
    }

    #[test]
    fn tarball_specs_rejects_unknown_target_filter() {
        let branding = branding_with_cross_compile(&["x86_64"], &[], &[]);
        let target = OsString::from("wasm32-unknown-unknown");
        let error = tarball_specs(&branding, Some(target.as_os_str())).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message) if message.contains("unknown tarball target triple")
        ));
    }

    #[test]
    fn resolve_binary_path_falls_back_to_release_when_build_skipped() {
        let workspace = tempdir().expect("create workspace");
        let release_dir = workspace
            .path()
            .join("target")
            .join("x86_64-unknown-linux-gnu")
            .join("release");
        fs::create_dir_all(&release_dir).expect("create release directory");
        let expected_binary = release_dir.join("oc-rsync");
        fs::write(&expected_binary, b"binary").expect("write fake binary");

        let spec = TarballSpec {
            platform: TarballPlatform::Linux,
            arch: "amd64",
            metadata_arch: "x86_64",
            target_triple: "x86_64-unknown-linux-gnu",
        };

        let resolved = resolve_binary_path(
            workspace.path(),
            &spec,
            &Some(OsString::from(DIST_PROFILE)),
            true,
            "oc-rsync",
        )
        .expect("fallback resolves binary path");

        assert_eq!(resolved, expected_binary);
    }
}
