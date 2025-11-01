use crate::commands::package::DIST_PROFILE;
use crate::error::{TaskError, TaskResult};
use crate::util::ensure_command_available;
use crate::workspace::WorkspaceBranding;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const INSTALL_HINT: &str = "install the GitHub CLI from https://cli.github.com/";
const REPOSITORY_ENV: &str = "OC_RSYNC_RELEASE_REPOSITORY";
const TAG_ENV: &str = "OC_RSYNC_RELEASE_TAG";
const COMMAND_ENV: &str = "OC_RSYNC_RELEASE_UPLOAD_CMD";
const GITHUB_PREFIX: &str = "https://github.com/";

pub(super) fn upload_release_artifacts(
    workspace: &Path,
    branding: &WorkspaceBranding,
) -> TaskResult<()> {
    let repository = resolve_repository(branding)?;
    let tag = resolve_tag(branding)?;
    let artifacts = gather_release_artifacts(workspace)?;

    println!("Uploading release artifacts for tag {tag} to {repository}:");
    for artifact in &artifacts {
        println!("  - {}", artifact.display());
    }

    run_upload_command(workspace, &repository, &tag, &artifacts)
}

fn resolve_repository(branding: &WorkspaceBranding) -> TaskResult<String> {
    if let Ok(value) = env::var(REPOSITORY_ENV) {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(TaskError::Validation(format!(
                "{REPOSITORY_ENV} cannot be empty"
            )));
        }
        return Ok(trimmed.to_owned());
    }

    let source = &branding.source;
    let Some(remainder) = source.strip_prefix(GITHUB_PREFIX) else {
        return Err(TaskError::Metadata(format!(
            "workspace metadata source '{source}' does not reference a GitHub repository"
        )));
    };

    let trimmed = remainder
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches('/')
        .to_owned();

    if trimmed.split('/').count() != 2 {
        return Err(TaskError::Metadata(format!(
            "workspace metadata source '{source}' must resolve to 'owner/repo'"
        )));
    }

    Ok(trimmed)
}

fn resolve_tag(branding: &WorkspaceBranding) -> TaskResult<String> {
    if let Ok(value) = env::var(TAG_ENV) {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(TaskError::Validation(format!("{TAG_ENV} cannot be empty")));
        }
        return Ok(trimmed.to_owned());
    }

    Ok(format!("v{}", branding.rust_version))
}

fn gather_release_artifacts(workspace: &Path) -> TaskResult<Vec<PathBuf>> {
    let mut artifacts = Vec::new();

    collect_tarballs(workspace, &mut artifacts)?;
    collect_debian_packages(workspace, &mut artifacts)?;
    collect_rpm_packages(workspace, &mut artifacts)?;

    artifacts.sort();
    artifacts.dedup();

    if artifacts.is_empty() {
        return Err(TaskError::Validation(String::from(
            "no release artifacts were found under target/; run `cargo xtask package` before uploading or pass --skip-upload",
        )));
    }

    Ok(artifacts)
}

fn collect_tarballs(workspace: &Path, artifacts: &mut Vec<PathBuf>) -> TaskResult<()> {
    let dist_dir = workspace.join("target").join("dist");
    if !dist_dir.is_dir() {
        return Ok(());
    }

    for entry in read_directory(&dist_dir)? {
        let metadata = read_metadata(&entry)?;
        if metadata.is_file() && has_suffix(&entry, ".tar.gz") {
            artifacts.push(entry);
        }
    }

    Ok(())
}

fn collect_debian_packages(workspace: &Path, artifacts: &mut Vec<PathBuf>) -> TaskResult<()> {
    let deb_dir = workspace.join("target").join("debian");
    if !deb_dir.is_dir() {
        return Ok(());
    }

    for entry in read_directory(&deb_dir)? {
        let metadata = read_metadata(&entry)?;
        if metadata.is_file() && has_suffix(&entry, ".deb") {
            artifacts.push(entry);
        }
    }

    Ok(())
}

fn collect_rpm_packages(workspace: &Path, artifacts: &mut Vec<PathBuf>) -> TaskResult<()> {
    for profile in [DIST_PROFILE, "release", "debug"] {
        let rpms_dir = workspace
            .join("target")
            .join(profile)
            .join("rpmbuild")
            .join("RPMS");

        if !rpms_dir.is_dir() {
            continue;
        }

        for arch_dir in read_directory(&rpms_dir)? {
            let metadata = read_metadata(&arch_dir)?;
            if !metadata.is_dir() {
                continue;
            }

            for entry in read_directory(&arch_dir)? {
                let metadata = read_metadata(&entry)?;
                if metadata.is_file() && has_suffix(&entry, ".rpm") {
                    artifacts.push(entry);
                }
            }
        }
    }

    Ok(())
}

fn read_directory(path: &Path) -> TaskResult<Vec<PathBuf>> {
    let entries = fs::read_dir(path).map_err(|error| {
        TaskError::Io(io::Error::new(
            error.kind(),
            format!("failed to read directory {}: {error}", path.display()),
        ))
    })?;

    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            TaskError::Io(io::Error::new(
                error.kind(),
                format!("failed to iterate directory {}: {error}", path.display()),
            ))
        })?;
        paths.push(entry.path());
    }

    paths.sort();
    Ok(paths)
}

fn read_metadata(path: &Path) -> TaskResult<fs::Metadata> {
    fs::metadata(path).map_err(|error| {
        TaskError::Io(io::Error::new(
            error.kind(),
            format!("failed to read metadata for {}: {error}", path.display()),
        ))
    })
}

fn has_suffix(path: &Path, suffix: &str) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|name| name.ends_with(suffix))
        .unwrap_or(false)
}

fn run_upload_command(
    workspace: &Path,
    repository: &str,
    tag: &str,
    artifacts: &[PathBuf],
) -> TaskResult<()> {
    let program = resolve_command()?;
    let display = program.display();

    if let CommandLocation::Named(name) = &program {
        ensure_command_available(name, INSTALL_HINT)?;
    }

    let mut command = match &program {
        CommandLocation::Named(name) => Command::new(name),
        CommandLocation::Explicit(path) => Command::new(path),
    };

    command.current_dir(workspace);
    command.arg("release");
    command.arg("upload");
    command.arg(tag);
    command.arg("--repo");
    command.arg(repository);
    command.arg("--clobber");

    for artifact in artifacts {
        command.arg(artifact);
    }

    let status = command.status().map_err(|error| {
        TaskError::Io(io::Error::new(
            error.kind(),
            format!("failed to invoke {}: {error}", display),
        ))
    })?;

    if status.success() {
        Ok(())
    } else {
        Err(TaskError::CommandFailed {
            program: format!("{} release upload", display),
            status,
        })
    }
}

enum CommandLocation {
    Named(String),
    Explicit(PathBuf),
}

impl CommandLocation {
    fn display(&self) -> String {
        match self {
            CommandLocation::Named(name) => name.clone(),
            CommandLocation::Explicit(path) => path.display().to_string(),
        }
    }
}

fn resolve_command() -> TaskResult<CommandLocation> {
    if let Some(value) = env::var_os(COMMAND_ENV) {
        if value.is_empty() {
            return Err(TaskError::Validation(format!(
                "{COMMAND_ENV} cannot be empty"
            )));
        }
        return Ok(CommandLocation::Explicit(PathBuf::from(value)));
    }

    Ok(CommandLocation::Named(String::from("gh")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::io::Write;
    use tempfile::TempDir;

    struct ScopedEnv {
        previous: Vec<(&'static str, Option<OsString>)>,
    }

    impl ScopedEnv {
        fn new() -> Self {
            Self {
                previous: Vec::new(),
            }
        }

        #[allow(unsafe_code)]
        fn set(&mut self, key: &'static str, value: OsString) {
            if self.previous.iter().all(|(existing, _)| existing != &key) {
                self.previous.push((key, env::var_os(key)));
            }
            unsafe { env::set_var(key, &value) };
        }
    }

    #[allow(unsafe_code)]
    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            for (key, value) in self.previous.drain(..).rev() {
                if let Some(value) = value {
                    unsafe { env::set_var(key, value) };
                } else {
                    unsafe { env::remove_var(key) };
                }
            }
        }
    }

    fn sample_branding() -> WorkspaceBranding {
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
            source: String::from("https://github.com/example/rsync"),
            cross_compile: Default::default(),
        }
    }

    #[test]
    fn resolve_repository_defaults_to_branding_source() {
        let branding = sample_branding();
        let repository = resolve_repository(&branding).expect("repository parsed");
        assert_eq!(repository, "example/rsync");
    }

    #[test]
    fn resolve_repository_respects_override() {
        let branding = sample_branding();
        let mut env = ScopedEnv::new();
        env.set(REPOSITORY_ENV, OsString::from("custom/repo"));
        let repository = resolve_repository(&branding).expect("repository parsed");
        assert_eq!(repository, "custom/repo");
    }

    #[test]
    fn resolve_tag_defaults_to_rust_version() {
        let branding = sample_branding();
        let tag = resolve_tag(&branding).expect("tag parsed");
        assert_eq!(tag, "v3.4.1-rust");
    }

    #[test]
    fn gather_release_artifacts_collects_known_outputs() {
        let temp = TempDir::new().expect("temp workspace");
        let workspace = temp.path();

        let tarball = workspace
            .join("target/dist")
            .join("oc-rsync-3.4.1-rust-x86_64-unknown-linux-gnu.tar.gz");
        let deb = workspace
            .join("target/debian")
            .join("oc-rsync_3.4.1-rust_amd64.deb");
        let rpm = workspace
            .join("target")
            .join(DIST_PROFILE)
            .join("rpmbuild/RPMS/x86_64")
            .join("oc-rsync-3.4.1-1.x86_64.rpm");

        for path in [&tarball, &deb, &rpm] {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent directories");
            }
            fs::File::create(path).expect("create artifact");
        }

        let artifacts = gather_release_artifacts(workspace).expect("gather artifacts");
        let mut expected = vec![deb, rpm, tarball];
        expected.sort();
        assert_eq!(artifacts, expected);
    }

    #[test]
    fn upload_release_artifacts_invokes_command_with_assets() {
        let temp = TempDir::new().expect("temp workspace");
        let workspace = temp.path();

        let branding = sample_branding();
        let tarball = workspace
            .join("target/dist")
            .join("oc-rsync-3.4.1-rust-x86_64-unknown-linux-gnu.tar.gz");
        let deb = workspace
            .join("target/debian")
            .join("oc-rsync_3.4.1-rust_amd64.deb");
        let rpm = workspace
            .join("target")
            .join(DIST_PROFILE)
            .join("rpmbuild/RPMS/x86_64")
            .join("oc-rsync-3.4.1-1.x86_64.rpm");

        for path in [&tarball, &deb, &rpm] {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent directories");
            }
            fs::File::create(path).expect("create artifact");
        }

        let log_path = workspace.join("upload-args.log");
        let script_path = workspace.join("fake-gh");
        let mut script = fs::File::create(&script_path).expect("create fake gh");
        writeln!(
            script,
            "#!/bin/sh\nprintf '%s\n' \"$@\" > \"$OC_RSYNC_TEST_UPLOAD_LOG\""
        )
        .expect("write script");
        drop(script);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script_path)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script_path, permissions).expect("make script executable");
        }

        let mut env = ScopedEnv::new();
        env.set(COMMAND_ENV, script_path.clone().into_os_string());
        env.set(TAG_ENV, OsString::from("v3.4.1-rust"));
        env.set(REPOSITORY_ENV, OsString::from("example/rsync"));
        env.set(
            "OC_RSYNC_TEST_UPLOAD_LOG",
            log_path.clone().into_os_string(),
        );

        upload_release_artifacts(workspace, &branding).expect("upload succeeds");

        let contents = fs::read_to_string(&log_path).expect("read upload log");
        let mut expected_lines = vec![
            String::from("release"),
            String::from("upload"),
            String::from("v3.4.1-rust"),
            String::from("--repo"),
            String::from("example/rsync"),
            String::from("--clobber"),
        ];
        let mut asset_lines = vec![
            deb.to_string_lossy().into_owned(),
            rpm.to_string_lossy().into_owned(),
            tarball.to_string_lossy().into_owned(),
        ];
        asset_lines.sort();
        expected_lines.extend(asset_lines);
        expected_lines.push(String::new());
        assert_eq!(contents, expected_lines.join("\n"));
    }
}
