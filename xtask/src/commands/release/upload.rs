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
    let artifacts = gather_release_artifacts(workspace, branding)?;

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

fn gather_release_artifacts(
    workspace: &Path,
    branding: &WorkspaceBranding,
) -> TaskResult<Vec<PathBuf>> {
    let mut artifacts = Vec::new();

    collect_tarballs(workspace, &mut artifacts)?;
    collect_debian_packages(workspace, &mut artifacts)?;
    collect_rpm_packages(workspace, &mut artifacts)?;
    collect_cross_compile_binaries(workspace, branding, &mut artifacts)?;

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
    let target_dir = workspace.join("target");
    if !target_dir.is_dir() {
        return Ok(());
    }

    for profile in [DIST_PROFILE, "release", "debug"] {
        let rpms_dir = target_dir.join(profile).join("rpmbuild").join("RPMS");
        collect_rpms_from_directory(&rpms_dir, artifacts)?;
    }

    for entry in read_directory(&target_dir)? {
        let metadata = read_metadata(&entry)?;
        if !metadata.is_dir() {
            continue;
        }

        let rpms_dir = entry.join(DIST_PROFILE).join("rpmbuild").join("RPMS");
        collect_rpms_from_directory(&rpms_dir, artifacts)?;
    }

    Ok(())
}

fn collect_rpms_from_directory(rpms_dir: &Path, artifacts: &mut Vec<PathBuf>) -> TaskResult<()> {
    if !rpms_dir.is_dir() {
        return Ok(());
    }

    for arch_dir in read_directory(rpms_dir)? {
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

    Ok(())
}

fn collect_cross_compile_binaries(
    workspace: &Path,
    branding: &WorkspaceBranding,
    artifacts: &mut Vec<PathBuf>,
) -> TaskResult<()> {
    for (os, arches) in &branding.cross_compile {
        for arch in arches {
            let target = cross_compile_target(os, arch).ok_or_else(|| {
                TaskError::Validation(format!(
                    "unsupported cross-compilation target '{os}-{arch}' declared in workspace metadata",
                ))
            })?;

            let release_dir = workspace.join("target").join(target).join("release");

            collect_cross_compile_binary(&release_dir, &branding.client_bin, os, arch, artifacts)?;

            if cross_compile_builds_daemon(os) {
                collect_cross_compile_binary(
                    &release_dir,
                    &branding.daemon_bin,
                    os,
                    arch,
                    artifacts,
                )?;
            }
        }
    }

    Ok(())
}

fn collect_cross_compile_binary(
    release_dir: &Path,
    binary: &str,
    os: &str,
    arch: &str,
    artifacts: &mut Vec<PathBuf>,
) -> TaskResult<()> {
    let primary_path = release_dir.join(binary);
    if primary_path.is_file() {
        artifacts.push(primary_path);
        return Ok(());
    }

    if os == "windows" {
        let exe_path = release_dir.join(format!("{binary}.exe"));
        if exe_path.is_file() {
            artifacts.push(exe_path);
            return Ok(());
        }

        return Err(TaskError::Validation(format!(
            "cross-compiled binary missing for {os}/{arch}: expected {} or {}",
            primary_path.display(),
            exe_path.display(),
        )));
    }

    Err(TaskError::Validation(format!(
        "cross-compiled binary missing for {os}/{arch}: expected {}",
        primary_path.display(),
    )))
}

fn cross_compile_target(os: &str, arch: &str) -> Option<&'static str> {
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

fn cross_compile_builds_daemon(os: &str) -> bool {
    !matches!(os, "windows")
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
            format!("failed to invoke {display}: {error}"),
        ))
    })?;

    if status.success() {
        Ok(())
    } else {
        Err(TaskError::CommandFailed {
            program: format!("{display} release upload"),
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
    use crate::test_support;
    use std::ffi::OsString;
    use std::io::Write;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;

    struct ScopedEnv {
        previous: Vec<(&'static str, Option<OsString>)>,
        _guard: MutexGuard<'static, ()>,
    }

    impl ScopedEnv {
        fn new() -> Self {
            let guard = env_lock().lock().expect("env lock poisoned");
            Self {
                previous: Vec::new(),
                _guard: guard,
            }
        }

        #[allow(unsafe_code)]
        fn set(&mut self, key: &'static str, value: OsString) {
            if self.previous.iter().all(|(existing, _)| existing != &key) {
                self.previous.push((key, env::var_os(key)));
            }
            unsafe {
                env::set_var(key, &value);
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    impl Drop for ScopedEnv {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            for (key, value) in self.previous.drain(..).rev() {
                if let Some(value) = value {
                    unsafe {
                        env::set_var(key, value);
                    }
                } else {
                    unsafe {
                        env::remove_var(key);
                    }
                }
            }
        }
    }

    fn sample_branding() -> WorkspaceBranding {
        test_support::workspace_branding_snapshot()
    }

    #[test]
    fn resolve_repository_defaults_to_branding_source() {
        let branding = sample_branding();
        let repository = resolve_repository(&branding).expect("repository parsed");
        let expected = branding
            .source
            .strip_prefix(GITHUB_PREFIX)
            .unwrap_or(&branding.source);
        assert_eq!(repository, expected);
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
        assert_eq!(tag, format!("v{}", branding.rust_version));
    }

    #[test]
    fn gather_release_artifacts_collects_known_outputs() {
        let temp = TempDir::new().expect("temp workspace");
        let workspace = temp.path();
        let mut branding = sample_branding();
        branding
            .cross_compile
            .insert(String::from("linux"), vec![String::from("x86_64")]);
        branding
            .cross_compile
            .insert(String::from("macos"), vec![String::from("x86_64")]);
        branding
            .cross_compile
            .insert(String::from("windows"), vec![String::from("x86_64")]);
        branding.cross_compile_matrix.clear();
        branding
            .cross_compile_matrix
            .insert(String::from("linux-x86_64"), true);
        branding
            .cross_compile_matrix
            .insert(String::from("darwin-x86_64"), true);
        branding
            .cross_compile_matrix
            .insert(String::from("windows-x86_64"), true);

        let tarball = workspace.join("target/dist").join(format!(
            "{}-{}-x86_64-unknown-linux-gnu.tar.gz",
            branding.client_bin, branding.rust_version
        ));
        let deb = workspace.join("target/debian").join(format!(
            "{}_{}_amd64.deb",
            branding.client_bin, branding.rust_version
        ));
        let rpm = workspace
            .join("target")
            .join("x86_64-unknown-linux-gnu")
            .join(DIST_PROFILE)
            .join("rpmbuild/RPMS/x86_64")
            .join(format!(
                "{}-{}-1.x86_64.rpm",
                branding.client_bin, branding.upstream_version
            ));
        let linux_binary = workspace
            .join("target")
            .join("x86_64-unknown-linux-gnu")
            .join("release")
            .join(&branding.client_bin);
        let mac_binary = workspace
            .join("target")
            .join("x86_64-apple-darwin")
            .join("release")
            .join(&branding.client_bin);
        let windows_binary = workspace
            .join("target")
            .join("x86_64-pc-windows-gnu")
            .join("release")
            .join(format!("{}.exe", branding.client_bin));

        for path in [
            &tarball,
            &deb,
            &rpm,
            &linux_binary,
            &mac_binary,
            &windows_binary,
        ] {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent directories");
            }
            fs::File::create(path).expect("create artifact");
        }

        let artifacts = gather_release_artifacts(workspace, &branding).expect("gather artifacts");
        let mut expected = vec![deb, rpm, tarball, linux_binary, mac_binary, windows_binary];
        expected.sort();
        assert_eq!(artifacts, expected);
    }

    #[test]
    fn upload_release_artifacts_invokes_command_with_assets() {
        let temp = TempDir::new().expect("temp workspace");
        let workspace = temp.path();

        let mut branding = sample_branding();
        branding
            .cross_compile
            .insert(String::from("linux"), vec![String::from("x86_64")]);
        branding
            .cross_compile
            .insert(String::from("macos"), vec![String::from("x86_64")]);
        branding
            .cross_compile
            .insert(String::from("windows"), vec![String::from("x86_64")]);
        branding.cross_compile_matrix.clear();
        branding
            .cross_compile_matrix
            .insert(String::from("linux-x86_64"), true);
        branding
            .cross_compile_matrix
            .insert(String::from("darwin-x86_64"), true);
        branding
            .cross_compile_matrix
            .insert(String::from("windows-x86_64"), true);
        let tarball = workspace.join("target/dist").join(format!(
            "{}-{}-x86_64-unknown-linux-gnu.tar.gz",
            branding.client_bin, branding.rust_version
        ));
        let deb = workspace.join("target/debian").join(format!(
            "{}_{}_amd64.deb",
            branding.client_bin, branding.rust_version
        ));
        let rpm = workspace
            .join("target")
            .join(DIST_PROFILE)
            .join("rpmbuild/RPMS/x86_64")
            .join(format!(
                "{}-{}-1.x86_64.rpm",
                branding.client_bin, branding.upstream_version
            ));
        let linux_binary = workspace
            .join("target")
            .join("x86_64-unknown-linux-gnu")
            .join("release")
            .join(&branding.client_bin);
        let mac_binary = workspace
            .join("target")
            .join("x86_64-apple-darwin")
            .join("release")
            .join(&branding.client_bin);
        let windows_binary = workspace
            .join("target")
            .join("x86_64-pc-windows-gnu")
            .join("release")
            .join(format!("{}.exe", branding.client_bin));

        for path in [
            &tarball,
            &deb,
            &rpm,
            &linux_binary,
            &mac_binary,
            &windows_binary,
        ] {
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
            linux_binary.to_string_lossy().into_owned(),
            mac_binary.to_string_lossy().into_owned(),
            windows_binary.to_string_lossy().into_owned(),
        ];
        asset_lines.sort();
        expected_lines.extend(asset_lines);
        expected_lines.push(String::new());
        assert_eq!(contents, expected_lines.join("\n"));
    }
}
