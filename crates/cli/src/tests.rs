use super::*;
use crate::password::set_password_stdin_input;
use rsync_checksums::strong::Md5;
use rsync_core::{
    branding::manifest,
    client::{ClientEventKind, FilterRuleKind},
};
use rsync_daemon as daemon_cli;
use rsync_filters::{FilterRule as EngineFilterRule, FilterSet};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime};

#[cfg(unix)]
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};

const RSYNC: &str = branding::client_program_name();
const OC_RSYNC: &str = branding::oc_client_program_name();
const RSYNCD: &str = branding::daemon_program_name();
const OC_RSYNC_D: &str = branding::oc_daemon_program_name();

const LEGACY_DAEMON_GREETING: &str = "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n";

#[cfg(all(
    unix,
    not(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    ))
))]
fn mkfifo_for_tests(path: &Path, mode: u32) -> io::Result<()> {
    use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};
    use std::convert::TryInto;

    let bits: u16 = (mode & 0o177_777)
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mode out of range"))?;
    let mode = Mode::from_bits_truncate(bits.into());
    mknodat(CWD, path, FileType::Fifo, mode, makedev(0, 0)).map_err(io::Error::from)
}

#[cfg(all(
    unix,
    any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    )
))]
fn mkfifo_for_tests(path: &Path, mode: u32) -> io::Result<()> {
    use std::convert::TryInto;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let bits: libc::mode_t = (mode & 0o177_777)
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mode out of range"))?;
    let path_c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains interior NUL"))?;
    let result = unsafe { libc::mkfifo(path_c.as_ptr(), bits) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn assert_contains_client_trailer(rendered: &str) {
    let expected = format!("[client={}]", manifest().rust_version());
    assert!(
        rendered.contains(&expected),
        "expected message to contain {expected:?}, got {rendered:?}"
    );
}
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn run_with_args<I, S>(args: I) -> (i32, Vec<u8>, Vec<u8>)
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run(args, &mut stdout, &mut stderr);
    (code, stdout, stderr)
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

#[allow(unsafe_code)]
impl EnvGuard {
    fn set(key: &'static str, value: &OsStr) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

#[allow(unsafe_code)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.previous.take() {
            unsafe {
                std::env::set_var(self.key, value);
            }
        } else {
            unsafe {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn clear_rsync_rsh() -> EnvGuard {
    EnvGuard::set("RSYNC_RSH", OsStr::new(""))
}

#[cfg(unix)]
fn write_executable_script(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write script");
    let mut permissions = std::fs::metadata(path)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("set script permissions");
}

#[test]
fn version_flag_renders_report() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--version")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::default().human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn oc_version_flag_renders_oc_banner() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--version")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::default()
        .with_client_brand(Brand::Oc)
        .human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn short_version_flag_renders_report() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("-V")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::default().human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn skip_compress_env_variable_enables_list() {
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set("RSYNC_SKIP_COMPRESS", OsStr::new("gz"));

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("archive.gz");
    let destination = tmp.path().join("dest.gz");
    std::fs::write(&source, b"payload").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-z"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(stdout.is_empty());
    assert_eq!(std::fs::read(destination).expect("read dest"), b"payload");
}

#[test]
fn skip_compress_invalid_env_reports_error() {
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set("RSYNC_SKIP_COMPRESS", OsStr::new("["));

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("file.txt");
    let destination = tmp.path().join("dest.txt");
    std::fs::write(&source, b"payload").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("RSYNC_SKIP_COMPRESS"));
    assert!(rendered.contains("invalid"));
}

#[test]
fn oc_short_version_flag_renders_oc_banner() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("-V")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::default()
        .with_client_brand(Brand::Oc)
        .human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn version_flag_ignores_additional_operands() {
    let (code, stdout, stderr) = run_with_args([
        OsStr::new(RSYNC),
        OsStr::new("--version"),
        OsStr::new("source"),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::default().human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn short_version_flag_ignores_additional_operands() {
    let (code, stdout, stderr) = run_with_args([
        OsStr::new(RSYNC),
        OsStr::new("-V"),
        OsStr::new("source"),
        OsStr::new("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::default().human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn daemon_flag_delegates_to_daemon_help() {
    let mut expected_stdout = Vec::new();
    let mut expected_stderr = Vec::new();
    let expected_code = daemon_cli::run(
        [OsStr::new(RSYNCD), OsStr::new("--help")],
        &mut expected_stdout,
        &mut expected_stderr,
    );

    assert_eq!(expected_code, 0);
    assert!(expected_stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsStr::new(RSYNC),
        OsStr::new("--daemon"),
        OsStr::new("--help"),
    ]);

    assert_eq!(code, expected_code);
    assert_eq!(stdout, expected_stdout);
    assert_eq!(stderr, expected_stderr);
}

#[test]
fn daemon_flag_delegates_to_daemon_version() {
    let mut expected_stdout = Vec::new();
    let mut expected_stderr = Vec::new();
    let expected_code = daemon_cli::run(
        [OsStr::new(RSYNCD), OsStr::new("--version")],
        &mut expected_stdout,
        &mut expected_stderr,
    );

    assert_eq!(expected_code, 0);
    assert!(expected_stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsStr::new(RSYNC),
        OsStr::new("--daemon"),
        OsStr::new("--version"),
    ]);

    assert_eq!(code, expected_code);
    assert_eq!(stdout, expected_stdout);
    assert_eq!(stderr, expected_stderr);
}

#[test]
fn oc_daemon_flag_delegates_to_oc_daemon_version() {
    let mut expected_stdout = Vec::new();
    let mut expected_stderr = Vec::new();
    let expected_code = daemon_cli::run(
        [OsStr::new(OC_RSYNC_D), OsStr::new("--version")],
        &mut expected_stdout,
        &mut expected_stderr,
    );

    assert_eq!(expected_code, 0);
    assert!(expected_stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsStr::new(OC_RSYNC),
        OsStr::new("--daemon"),
        OsStr::new("--version"),
    ]);

    assert_eq!(code, expected_code);
    assert_eq!(stdout, expected_stdout);
    assert_eq!(stderr, expected_stderr);
}

#[test]
fn daemon_mode_arguments_ignore_operands_after_double_dash() {
    let args = vec![
        OsString::from(RSYNC),
        OsString::from("--"),
        OsString::from("--daemon"),
        OsString::from("dest"),
    ];

    assert!(daemon_mode_arguments(&args).is_none());
}

#[test]
fn help_flag_renders_static_help_snapshot() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = render_help(ProgramName::Rsync);
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn oc_help_flag_uses_wrapped_program_name() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = render_help(ProgramName::OcRsync);
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn oc_help_mentions_oc_rsyncd_delegation() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("valid UTF-8");
    let upstream = format!("delegates to {}", branding::daemon_program_name());
    let branded = format!("delegates to {}", branding::oc_daemon_program_name());
    assert!(rendered.contains(&branded));
    assert!(!rendered.contains(&upstream));
}

#[test]
fn oc_help_mentions_branded_daemon_phrase() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("valid UTF-8");
    let upstream = format!("Run as an {} daemon", branding::client_program_name());
    let branded = format!("Run as an {} daemon", branding::oc_client_program_name());
    assert!(rendered.contains(&branded));
    assert!(!rendered.contains(&upstream));
}

#[test]
fn short_h_flag_enables_human_readable_mode() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-h"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Enabled));
}

#[test]
fn transfer_request_reports_missing_operands() {
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC)]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());

    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("missing source operands"));
    assert_contains_client_trailer(&rendered);
}

#[test]
fn run_reports_invalid_chmod_specification() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    std::fs::write(&source, b"data").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--chmod=a+q"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
    assert!(rendered.contains("failed to parse --chmod specification"));
}

#[test]
fn transfer_request_copies_file() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&source, b"cli copy").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"cli copy"
    );
}

#[test]
fn backup_flag_creates_default_suffix_backups() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir_all(&source_dir).expect("create source dir");
    std::fs::create_dir_all(&dest_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    std::fs::write(&source_file, b"new data").expect("write source");

    let dest_root = dest_dir.join("source");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(dest_root.join("file.txt"), b"old data").expect("seed dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--backup"),
        source_dir.clone().into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let target_root = dest_dir.join("source");
    assert_eq!(
        std::fs::read(target_root.join("file.txt")).expect("read dest"),
        b"new data"
    );
    assert_eq!(
        std::fs::read(target_root.join("file.txt~")).expect("read backup"),
        b"old data"
    );
}

#[test]
fn backup_dir_flag_places_backups_in_relative_directory() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir_all(source_dir.join("nested")).expect("create nested source");
    std::fs::create_dir_all(dest_dir.join("source/nested")).expect("create nested dest");

    let source_file = source_dir.join("nested/file.txt");
    std::fs::write(&source_file, b"updated").expect("write source");
    std::fs::write(dest_dir.join("source/nested/file.txt"), b"previous").expect("seed dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--backup-dir"),
        OsString::from("backups"),
        source_dir.clone().into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let backup_path = dest_dir.join("backups/source/nested/file.txt~");
    assert_eq!(
        std::fs::read(&backup_path).expect("read backup"),
        b"previous"
    );
    assert_eq!(
        std::fs::read(dest_dir.join("source/nested/file.txt")).expect("read dest"),
        b"updated"
    );
}

#[test]
fn backup_suffix_flag_overrides_default_suffix() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir_all(&source_dir).expect("create source dir");
    std::fs::create_dir_all(&dest_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    std::fs::write(&source_file, b"fresh").expect("write source");
    let dest_root = dest_dir.join("source");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(dest_root.join("file.txt"), b"stale").expect("seed dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--suffix"),
        OsString::from(".bak"),
        source_dir.clone().into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    assert_eq!(
        std::fs::read(dest_root.join("file.txt")).expect("read dest"),
        b"fresh"
    );
    let backup_path = dest_root.join("file.txt.bak");
    assert_eq!(std::fs::read(&backup_path).expect("read backup"), b"stale");
}

#[test]
fn verbose_transfer_emits_event_lines() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("file.txt");
    let destination = tmp.path().join("out.txt");
    std::fs::write(&source, b"verbose").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("verbose output is UTF-8");
    assert!(rendered.contains("file.txt"));
    assert!(!rendered.contains("Total transferred"));
    assert!(rendered.contains("sent 7 bytes  received 7 bytes"));
    assert!(rendered.contains("total size is 7  speedup is 0.50"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"verbose"
    );
}

#[test]
fn stats_human_readable_formats_totals() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("file.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let dest_default = tmp.path().join("default");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        source.clone().into_os_string(),
        dest_default.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");
    assert!(rendered.contains("Total file size: 1,536 bytes"));
    assert!(rendered.contains("Total bytes sent: 1,536"));

    let dest_human = tmp.path().join("human");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("--human-readable"),
        source.into_os_string(),
        dest_human.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");
    assert!(rendered.contains("Total file size: 1.54K bytes"));
    assert!(rendered.contains("Total bytes sent: 1.54K"));
}

#[test]
fn stats_human_readable_combined_formats_totals() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("file.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let dest_combined = tmp.path().join("combined");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("--human-readable=2"),
        source.into_os_string(),
        dest_combined.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");
    assert!(rendered.contains("Total file size: 1.54K (1,536) bytes"));
    assert!(rendered.contains("Total bytes sent: 1.54K (1,536)"));
}

#[cfg(unix)]
#[test]
fn verbose_transfer_reports_skipped_specials() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_fifo = tmp.path().join("skip.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

    let destination = tmp.path().join("dest.pipe");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source_fifo.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(std::fs::symlink_metadata(&destination).is_err());

    let rendered = String::from_utf8(stdout).expect("verbose output is UTF-8");
    assert!(rendered.contains("skipping non-regular file \"skip.pipe\""));
}

#[test]
fn verbose_human_readable_formats_sizes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("sizes.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let dest_default = tmp.path().join("default.bin");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        source.clone().into_os_string(),
        dest_default.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("verbose output utf8");
    assert!(rendered.contains("1,536 bytes"));

    let dest_human = tmp.path().join("human.bin");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        OsString::from("--human-readable"),
        source.into_os_string(),
        dest_human.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("verbose output utf8");
    assert!(rendered.contains("1.54K bytes"));
}

#[test]
fn verbose_human_readable_combined_formats_sizes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("sizes.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let destination = tmp.path().join("combined.bin");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        OsString::from("--human-readable=2"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("verbose output utf8");
    assert!(rendered.contains("1.54K (1,536) bytes"));
}

#[test]
fn progress_transfer_renders_progress_lines() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("progress.txt");
    let destination = tmp.path().join("progress.out");
    std::fs::write(&source, b"progress").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
    assert!(rendered.contains("progress.txt"));
    assert!(rendered.contains("(xfr#1, to-chk=0/1)"));
    assert!(!rendered.contains("Total transferred"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"progress"
    );
}

#[test]
fn progress_human_readable_formats_sizes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("human-progress.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let destination_default = tmp.path().join("default-progress.bin");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        source.clone().into_os_string(),
        destination_default.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("progress output utf8");
    let normalized = rendered.replace('\r', "\n");
    assert!(normalized.contains("1,536"));

    let destination_human = tmp.path().join("human-progress.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("--human-readable"),
        source.into_os_string(),
        destination_human.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("progress output utf8");
    let normalized = rendered.replace('\r', "\n");
    assert!(normalized.contains("1.54K"));
}

#[test]
fn progress_human_readable_combined_formats_sizes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("human-progress.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let destination = tmp.path().join("combined-progress.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("--human-readable=2"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("progress output utf8");
    let normalized = rendered.replace('\r', "\n");
    assert!(normalized.contains("1.54K (1,536)"));
}

#[test]
fn progress_transfer_routes_messages_to_stderr_when_requested() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("stderr-progress.txt");
    let destination = tmp.path().join("stderr-progress.out");
    std::fs::write(&source, b"stderr-progress").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("--msgs2stderr"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    let rendered_out = String::from_utf8(stdout).expect("stdout utf8");
    assert!(rendered_out.trim().is_empty());

    let rendered_err = String::from_utf8(stderr).expect("stderr utf8");
    assert!(rendered_err.contains("stderr-progress.txt"));
    assert!(rendered_err.contains("(xfr#1, to-chk=0/1)"));

    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"stderr-progress"
    );
}

#[test]
fn progress_percent_placeholder_used_for_unknown_totals() {
    assert_eq!(format_progress_percent(42, None), "??%");
    assert_eq!(format_progress_percent(0, Some(0)), "100%");
    assert_eq!(format_progress_percent(50, Some(200)), "25%");
}

#[test]
fn progress_reports_intermediate_updates() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("large.bin");
    let destination = tmp.path().join("large.out");
    let payload = vec![0xA5u8; 256 * 1024];
    std::fs::write(&source, &payload).expect("write large source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
    assert!(rendered.contains("large.bin"));
    assert!(rendered.contains("(xfr#1, to-chk=0/1)"));
    assert!(rendered.contains("\r"));
    assert!(rendered.contains(" 50%"));
    assert!(rendered.contains("100%"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        payload
    );
}

#[cfg(unix)]
#[test]
fn progress_reports_unknown_totals_with_placeholder() {
    use std::os::unix::fs::FileTypeExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("fifo.in");
    mkfifo_for_tests(&source, 0o600).expect("mkfifo");

    let destination = tmp.path().join("fifo.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("--specials"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
    assert!(rendered.contains("fifo.in"));
    assert!(rendered.contains("??%"));
    assert!(rendered.contains("to-chk=0/1"));

    let metadata = std::fs::symlink_metadata(&destination).expect("stat destination");
    assert!(metadata.file_type().is_fifo());
}

#[cfg(unix)]
#[test]
fn info_progress2_enables_progress_output() {
    use std::os::unix::fs::FileTypeExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("info-fifo.in");
    mkfifo_for_tests(&source, 0o600).expect("mkfifo");

    let destination = tmp.path().join("info-fifo.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=progress2"),
        OsString::from("--specials"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
    assert!(!rendered.contains("info-fifo.in"));
    assert!(rendered.contains("to-chk=0/1"));
    assert!(rendered.contains("0.00kB/s"));

    let metadata = std::fs::symlink_metadata(&destination).expect("stat destination");
    assert!(metadata.file_type().is_fifo());
}

#[test]
fn progress_with_verbose_inserts_separator_before_totals() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("progress.txt");
    let destination = tmp.path().join("progress.out");
    std::fs::write(&source, b"progress").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("-v"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
    assert!(rendered.contains("(xfr#1, to-chk=0/1)"));
    assert!(rendered.contains("\n\nsent"));
    assert!(rendered.contains("sent"));
    assert!(rendered.contains("total size is"));
}

#[test]
fn stats_transfer_renders_summary_block() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("stats.txt");
    let destination = tmp.path().join("stats.out");
    let payload = b"statistics";
    std::fs::write(&source, payload).expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("stats output is UTF-8");
    let expected_size = payload.len();
    assert!(rendered.contains("Number of files: 1 (reg: 1)"));
    assert!(rendered.contains("Number of created files: 1 (reg: 1)"));
    assert!(rendered.contains("Number of regular files transferred: 1"));
    assert!(!rendered.contains("Number of regular files matched"));
    assert!(!rendered.contains("Number of hard links"));
    assert!(rendered.contains(&format!("Total file size: {expected_size} bytes")));
    assert!(rendered.contains(&format!("Literal data: {expected_size} bytes")));
    assert!(rendered.contains("Matched data: 0 bytes"));
    assert!(rendered.contains("File list size: 0"));
    assert!(rendered.contains("File list generation time:"));
    assert!(rendered.contains("File list transfer time:"));
    assert!(rendered.contains(&format!("Total bytes sent: {expected_size}")));
    assert!(rendered.contains(&format!("Total bytes received: {expected_size}")));
    assert!(rendered.contains("\n\nsent"));
    assert!(rendered.contains("total size is"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        payload
    );
}

#[test]
fn info_stats_enables_summary_block() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("info-stats.txt");
    let destination = tmp.path().join("info-stats.out");
    let payload = b"statistics";
    std::fs::write(&source, payload).expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=stats"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("stats output is UTF-8");
    let expected_size = payload.len();
    assert!(rendered.contains("Number of files: 1 (reg: 1)"));
    assert!(rendered.contains(&format!("Total file size: {expected_size} bytes")));
    assert!(rendered.contains("Literal data:"));
    assert!(rendered.contains("\n\nsent"));
    assert!(rendered.contains("total size is"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        payload
    );
}

#[test]
fn info_none_disables_progress_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("info-none.txt");
    let destination = tmp.path().join("info-none.out");
    std::fs::write(&source, b"payload").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("--info=none"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    assert!(!rendered.contains("to-chk"));
    assert!(rendered.trim().is_empty());
}

#[test]
fn info_help_lists_supported_flags() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--info=help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(stdout, INFO_HELP_TEXT.as_bytes());
}

#[test]
fn debug_help_lists_supported_flags() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--debug=help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(stdout, DEBUG_HELP_TEXT.as_bytes());
}

#[test]
fn info_rejects_unknown_flag() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--info=unknown")]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("stderr utf8");
    assert!(rendered.contains("invalid --info flag"));
}

#[test]
fn info_accepts_comma_separated_tokens() {
    let flags = vec![OsString::from("progress,name2,stats")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert!(matches!(settings.progress, ProgressSetting::PerFile));
    assert_eq!(settings.name, Some(NameOutputLevel::UpdatedAndUnchanged));
    assert_eq!(settings.stats, Some(true));
}

#[test]
fn info_rejects_empty_segments() {
    let flags = vec![OsString::from("progress,,stats")];
    let error = parse_info_flags(&flags).err().expect("parse should fail");
    assert!(error.to_string().contains("--info flag must not be empty"));
}

#[test]
fn debug_accepts_comma_separated_tokens() {
    let flags = vec![OsString::from("checksum,io")];
    let settings = parse_debug_flags(&flags).expect("flags parse");
    assert!(!settings.help_requested);
    assert_eq!(
        settings.flags,
        vec![OsString::from("checksum"), OsString::from("io")]
    );
}

#[test]
fn debug_rejects_empty_segments() {
    let flags = vec![OsString::from("checksum,,io")];
    let error = parse_debug_flags(&flags).err().expect("parse should fail");
    assert!(error.to_string().contains("--debug flag must not be empty"));
}

#[test]
fn info_name_emits_filenames_without_verbose() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("name.txt");
    let destination = tmp.path().join("name.out");
    std::fs::write(&source, b"name-info").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=name"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    assert!(rendered.contains("name.txt"));
    assert!(rendered.contains("sent"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"name-info"
    );
}

#[test]
fn info_name0_suppresses_verbose_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("quiet.txt");
    let destination = tmp.path().join("quiet.out");
    std::fs::write(&source, b"quiet").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        OsString::from("--info=name0"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    assert!(!rendered.contains("quiet.txt"));
    assert!(rendered.contains("sent"));
}

#[test]
fn info_name2_reports_unchanged_entries() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("unchanged.txt");
    let destination = tmp.path().join("unchanged.out");
    std::fs::write(&source, b"unchanged").expect("write source");

    let initial = run_with_args([
        OsString::from(RSYNC),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);
    assert_eq!(initial.0, 0);
    assert!(initial.1.is_empty());
    assert!(initial.2.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=name2"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    assert!(rendered.contains("unchanged.txt"));
}

#[test]
fn transfer_request_with_archive_copies_file() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&source, b"archive").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"archive"
    );
}

#[test]
fn transfer_request_with_remove_source_files_deletes_source() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&source, b"move me").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--remove-source-files"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert!(!source.exists(), "source should be removed");
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"move me"
    );
}

#[test]
fn transfer_request_with_remove_sent_files_alias_deletes_source() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&source, b"alias move").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--remove-sent-files"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert!(!source.exists(), "source should be removed");
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"alias move"
    );
}

#[test]
fn transfer_request_with_bwlimit_copies_file() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&source, b"limited").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--bwlimit=2048"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"limited"
    );
}

#[test]
fn transfer_request_with_no_bwlimit_copies_file() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&source, b"unlimited").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-bwlimit"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"unlimited"
    );
}

#[test]
fn transfer_request_with_out_format_renders_entries() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"format").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--out-format=%f %b"),
        source.clone().into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(String::from_utf8(stdout).expect("utf8"), "source.txt 6\n");

    let destination = dest_dir.join("source.txt");
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"format"
    );
}

#[test]
fn transfer_request_with_itemize_changes_renders_itemized_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let dest_dir = tmp.path().join("dest");
    std::fs::create_dir(&dest_dir).expect("create dest dir");
    std::fs::write(&source, b"itemized").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--itemize-changes"),
        source.clone().into_os_string(),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(
        String::from_utf8(stdout).expect("utf8"),
        ">f+++++++++ source.txt\n"
    );

    let destination = dest_dir.join("source.txt");
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"itemized"
    );
}

#[test]
fn transfer_request_with_ignore_existing_leaves_destination_unchanged() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&source, b"updated").expect("write source");
    std::fs::write(&destination, b"original").expect("write destination");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--ignore-existing"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"original"
    );
}

#[test]
fn transfer_request_with_ignore_missing_args_skips_missing_sources() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let missing = tmp.path().join("missing.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&destination, b"existing").expect("write destination");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--ignore-missing-args"),
        missing.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"existing"
    );
}

#[test]
fn transfer_request_with_relative_preserves_parent_directories() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("src");
    let destination_root = tmp.path().join("dest");
    std::fs::create_dir_all(source_root.join("foo/bar")).expect("create source tree");
    std::fs::create_dir_all(&destination_root).expect("create destination");
    let source_file = source_root.join("foo").join("bar").join("relative.txt");
    std::fs::write(&source_file, b"relative").expect("write source");

    let operand = source_root
        .join(".")
        .join("foo")
        .join("bar")
        .join("relative.txt");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--relative"),
        operand.into_os_string(),
        destination_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let copied = destination_root
        .join("foo")
        .join("bar")
        .join("relative.txt");
    assert_eq!(
        std::fs::read(copied).expect("read copied file"),
        b"relative"
    );
}

#[cfg(unix)]
#[test]
fn transfer_request_with_sparse_preserves_holes() {
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.bin");
    let mut source_file = std::fs::File::create(&source).expect("create source");
    source_file.write_all(&[0x10]).expect("write leading byte");
    source_file
        .seek(SeekFrom::Start(1024 * 1024))
        .expect("seek to hole");
    source_file.write_all(&[0x20]).expect("write trailing byte");
    source_file.set_len(3 * 1024 * 1024).expect("extend source");

    let dense_dest = tmp.path().join("dense.bin");
    let sparse_dest = tmp.path().join("sparse.bin");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--sparse"),
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let dense_meta = std::fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = std::fs::metadata(&sparse_dest).expect("sparse metadata");

    assert_eq!(dense_meta.len(), sparse_meta.len());
    assert!(sparse_meta.blocks() < dense_meta.blocks());
}

#[test]
fn transfer_request_with_files_from_copies_listed_sources() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_a = tmp.path().join("files-from-a.txt");
    let source_b = tmp.path().join("files-from-b.txt");
    std::fs::write(&source_a, b"files-from-a").expect("write source a");
    std::fs::write(&source_b, b"files-from-b").expect("write source b");

    let list_path = tmp.path().join("files-from.list");
    let list_contents = format!("{}\n{}\n", source_a.display(), source_b.display());
    std::fs::write(&list_path, list_contents).expect("write list");

    let dest_dir = tmp.path().join("files-from-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_a = dest_dir.join(source_a.file_name().expect("file name a"));
    let copied_b = dest_dir.join(source_b.file_name().expect("file name b"));
    assert_eq!(
        std::fs::read(&copied_a).expect("read copied a"),
        b"files-from-a"
    );
    assert_eq!(
        std::fs::read(&copied_b).expect("read copied b"),
        b"files-from-b"
    );
}

#[test]
fn transfer_request_with_files_from_skips_comment_lines() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_a = tmp.path().join("comment-a.txt");
    let source_b = tmp.path().join("comment-b.txt");
    std::fs::write(&source_a, b"comment-a").expect("write source a");
    std::fs::write(&source_b, b"comment-b").expect("write source b");

    let list_path = tmp.path().join("files-from.list");
    let contents = format!(
        "# leading comment\n; alt comment\n{}\n{}\n",
        source_a.display(),
        source_b.display()
    );
    std::fs::write(&list_path, contents).expect("write list");

    let dest_dir = tmp.path().join("files-from-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_a = dest_dir.join(source_a.file_name().expect("file name a"));
    let copied_b = dest_dir.join(source_b.file_name().expect("file name b"));
    assert_eq!(std::fs::read(&copied_a).expect("read a"), b"comment-a");
    assert_eq!(std::fs::read(&copied_b).expect("read b"), b"comment-b");
}

#[test]
fn transfer_request_with_from0_reads_null_separated_list() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_a = tmp.path().join("from0-a.txt");
    let source_b = tmp.path().join("from0-b.txt");
    std::fs::write(&source_a, b"from0-a").expect("write source a");
    std::fs::write(&source_b, b"from0-b").expect("write source b");

    let mut bytes = Vec::new();
    bytes.extend_from_slice(source_a.display().to_string().as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(source_b.display().to_string().as_bytes());
    bytes.push(0);
    let list_path = tmp.path().join("files-from0.list");
    std::fs::write(&list_path, bytes).expect("write list");

    let dest_dir = tmp.path().join("files-from0-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--from0"),
        OsString::from(format!("--files-from={}", list_path.display())),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_a = dest_dir.join(source_a.file_name().expect("file name a"));
    let copied_b = dest_dir.join(source_b.file_name().expect("file name b"));
    assert_eq!(std::fs::read(&copied_a).expect("read copied a"), b"from0-a");
    assert_eq!(std::fs::read(&copied_b).expect("read copied b"), b"from0-b");
}

#[test]
fn transfer_request_with_from0_preserves_comment_prefix_entries() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let comment_named = tmp.path().join("#commented.txt");
    std::fs::write(&comment_named, b"from0-comment").expect("write comment source");

    let mut bytes = Vec::new();
    bytes.extend_from_slice(comment_named.display().to_string().as_bytes());
    bytes.push(0);
    let list_path = tmp.path().join("files-from0-comments.list");
    std::fs::write(&list_path, bytes).expect("write list");

    let dest_dir = tmp.path().join("files-from0-comments-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--from0"),
        OsString::from(format!("--files-from={}", list_path.display())),
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied = dest_dir.join(comment_named.file_name().expect("file name"));
    assert_eq!(
        std::fs::read(&copied).expect("read copied"),
        b"from0-comment"
    );
}

#[test]
fn files_from_reports_read_failures() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let missing = tmp.path().join("missing.list");
    let dest_dir = tmp.path().join("files-from-error-dest");
    std::fs::create_dir(&dest_dir).expect("create dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", missing.display())),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("utf8");
    assert!(rendered.contains("failed to read file list"));
}

#[cfg(unix)]
#[test]
fn files_from_preserves_non_utf8_entries() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let list_path = tmp.path().join("binary.list");
    std::fs::write(&list_path, [b'f', b'o', 0x80, b'\n']).expect("write binary list");

    let entries =
        load_file_list_operands(&[list_path.into_os_string()], false).expect("load entries");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].as_os_str().as_bytes(), b"fo\x80");
}

#[test]
fn from0_reader_accepts_missing_trailing_separator() {
    let data = b"alpha\0beta\0gamma";
    let mut reader = BufReader::new(&data[..]);
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, true, &mut entries).expect("read list");

    assert_eq!(
        entries,
        vec![
            OsString::from("alpha"),
            OsString::from("beta"),
            OsString::from("gamma"),
        ]
    );
}

#[cfg(unix)]
#[test]
fn transfer_request_with_owner_group_preserves_flags() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&source, b"metadata").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--owner"),
        OsString::from("--group"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"metadata"
    );
}

#[cfg(unix)]
#[test]
fn transfer_request_with_perms_preserves_mode() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-perms.txt");
    let destination = tmp.path().join("dest-perms.txt");
    std::fs::write(&source, b"data").expect("write source");
    let atime = FileTime::from_unix_time(1_700_070_000, 0);
    let mtime = FileTime::from_unix_time(1_700_080_000, 0);
    set_file_times(&source, atime, mtime).expect("set times");
    std::fs::set_permissions(&source, PermissionsExt::from_mode(0o640)).expect("set perms");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--perms"),
        OsString::from("--times"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let metadata = std::fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(dest_mtime, mtime);
}

#[cfg(unix)]
#[test]
fn transfer_request_with_no_perms_overrides_archive() {
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-no-perms.txt");
    let destination = tmp.path().join("dest-no-perms.txt");
    std::fs::write(&source, b"data").expect("write source");
    std::fs::set_permissions(&source, PermissionsExt::from_mode(0o600)).expect("set perms");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--no-perms"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let metadata = std::fs::metadata(&destination).expect("dest metadata");
    assert_ne!(metadata.permissions().mode() & 0o777, 0o600);
}

#[test]
fn parse_args_recognises_perms_and_times_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--perms"),
        OsString::from("--times"),
        OsString::from("--omit-dir-times"),
        OsString::from("--omit-link-times"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.perms, Some(true));
    assert_eq!(parsed.times, Some(true));
    assert_eq!(parsed.omit_dir_times, Some(true));
    assert_eq!(parsed.omit_link_times, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--no-perms"),
        OsString::from("--no-times"),
        OsString::from("--no-omit-dir-times"),
        OsString::from("--no-omit-link-times"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.perms, Some(false));
    assert_eq!(parsed.times, Some(false));
    assert_eq!(parsed.omit_dir_times, Some(false));
    assert_eq!(parsed.omit_link_times, Some(false));
}

#[test]
fn parse_args_recognises_super_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--super"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.super_mode, Some(true));
}

#[test]
fn parse_args_recognises_no_super_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-super"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.super_mode, Some(false));
}

#[test]
fn parse_args_collects_chmod_values() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--chmod=Du+rwx"),
        OsString::from("--chmod"),
        OsString::from("Fgo-w"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(
        parsed.chmod,
        vec![OsString::from("Du+rwx"), OsString::from("Fgo-w")]
    );
}

#[test]
fn parse_args_recognises_update_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--update"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.update);
}

#[test]
fn parse_args_recognises_modify_window() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--modify-window=5"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.modify_window, Some(OsString::from("5")));
}

#[test]
fn parse_args_recognises_checksum_choice() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--checksum-choice=XXH128"),
    ])
    .expect("parse");

    let expected = StrongChecksumChoice::parse("xxh128").expect("choice");
    assert_eq!(parsed.checksum_choice, Some(expected));
    assert_eq!(parsed.checksum_choice_arg, Some(OsString::from("xxh128")));
}

#[test]
fn parse_args_rejects_invalid_checksum_choice() {
    let error = match parse_args([
        OsString::from(RSYNC),
        OsString::from("--checksum-choice=invalid"),
    ]) {
        Ok(_) => panic!("parse should fail"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    assert!(
        error
            .to_string()
            .contains("invalid --checksum-choice value 'invalid'")
    );
}

#[test]
fn parse_args_recognises_owner_overrides() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--owner"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.owner, Some(true));
    assert_eq!(parsed.group, None);

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--no-owner"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.owner, Some(false));
    assert!(parsed.archive);
}

#[test]
fn parse_args_recognises_chown_option() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--chown=user:group"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.chown, Some(OsString::from("user:group")));
}

#[test]
fn parse_args_sets_protect_args_flag() {
    let parsed =
        parse_args([OsString::from(RSYNC), OsString::from("--protect-args")]).expect("parse");

    assert_eq!(parsed.protect_args, Some(true));
}

#[test]
fn parse_args_sets_protect_args_alias() {
    let parsed =
        parse_args([OsString::from(RSYNC), OsString::from("--secluded-args")]).expect("parse");

    assert_eq!(parsed.protect_args, Some(true));
}

#[test]
fn parse_args_sets_no_protect_args_flag() {
    let parsed =
        parse_args([OsString::from(RSYNC), OsString::from("--no-protect-args")]).expect("parse");

    assert_eq!(parsed.protect_args, Some(false));
}

#[test]
fn parse_args_sets_no_protect_args_alias() {
    let parsed =
        parse_args([OsString::from(RSYNC), OsString::from("--no-secluded-args")]).expect("parse");

    assert_eq!(parsed.protect_args, Some(false));
}

#[test]
fn parse_args_sets_ipv4_address_mode() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--ipv4"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.address_mode, AddressMode::Ipv4);
}

#[test]
fn parse_args_sets_ipv6_address_mode() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--ipv6"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.address_mode, AddressMode::Ipv6);
}

#[test]
fn parse_args_recognises_group_overrides() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--group"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.group, Some(true));
    assert_eq!(parsed.owner, None);

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--no-group"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.group, Some(false));
    assert!(parsed.archive);
}

#[test]
fn parse_args_reads_env_protect_args_default() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", OsStr::new("1"));

    let parsed = parse_args([OsString::from(RSYNC)]).expect("parse");

    assert_eq!(parsed.protect_args, Some(true));
}

#[test]
fn parse_args_respects_env_protect_args_disabled() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", OsStr::new("0"));

    let parsed = parse_args([OsString::from(RSYNC)]).expect("parse");

    assert_eq!(parsed.protect_args, Some(false));
}

#[test]
fn parse_args_recognises_numeric_ids_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--numeric-ids"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.numeric_ids, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-numeric-ids"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.numeric_ids, Some(false));
}

#[test]
fn parse_args_recognises_sparse_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--sparse"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.sparse, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-sparse"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.sparse, Some(false));
}

#[test]
fn parse_args_recognises_copy_links_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--copy-links"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.copy_links, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-copy-links"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.copy_links, Some(false));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-L"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.copy_links, Some(true));
}

#[test]
fn parse_args_recognises_copy_unsafe_links_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--copy-unsafe-links"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.copy_unsafe_links, Some(true));
    assert!(parsed.safe_links);

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-copy-unsafe-links"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.copy_unsafe_links, Some(false));
}

#[test]
fn parse_args_recognises_hard_links_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--hard-links"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.hard_links, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-hard-links"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.hard_links, Some(false));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-H"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.hard_links, Some(true));
}

#[test]
fn parse_args_recognises_copy_dirlinks_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--copy-dirlinks"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.copy_dirlinks);

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-k"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.copy_dirlinks);
}

#[test]
fn parse_args_recognises_keep_dirlinks_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--keep-dirlinks"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.keep_dirlinks, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-keep-dirlinks"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.keep_dirlinks, Some(false));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-K"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.keep_dirlinks, Some(true));
}

#[test]
fn parse_args_recognises_safe_links_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--safe-links"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.safe_links);
}

#[test]
fn parse_args_recognises_cvs_exclude_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--cvs-exclude"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.cvs_exclude);

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-C"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.cvs_exclude);
}

#[test]
fn parse_args_recognises_partial_dir_and_enables_partial() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--partial-dir=.rsync-partial"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.partial);
    assert_eq!(
        parsed.partial_dir.as_deref(),
        Some(Path::new(".rsync-partial"))
    );
}

#[test]
fn parse_args_recognises_temp_dir_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--temp-dir=.rsync-tmp"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.temp_dir.as_deref(), Some(Path::new(".rsync-tmp")));
}

#[test]
fn parse_args_resets_delay_updates_with_no_delay_updates() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--delay-updates"),
        OsString::from("--no-delay-updates"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.delay_updates);
}

#[test]
fn parse_args_allows_no_partial_to_clear_partial_dir() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--partial-dir=.rsync-partial"),
        OsString::from("--no-partial"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.partial);
    assert!(parsed.partial_dir.is_none());
}

#[test]
fn parse_args_recognises_delay_updates_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--delay-updates"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.delay_updates);
}

#[test]
fn parse_args_recognises_no_delay_updates_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--delay-updates"),
        OsString::from("--no-delay-updates"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.delay_updates);
}

#[test]
fn parse_args_collects_link_dest_paths() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--link-dest=baseline"),
        OsString::from("--link-dest=/var/cache"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(
        parsed.link_dests,
        vec![PathBuf::from("baseline"), PathBuf::from("/var/cache")]
    );
}

#[test]
fn parse_args_recognises_devices_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--devices"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.devices, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-devices"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.devices, Some(false));
}

#[test]
fn parse_args_recognises_remove_sent_files_alias() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--remove-sent-files"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.remove_source_files);
}

#[test]
fn parse_args_recognises_specials_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--specials"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.specials, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-specials"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.specials, Some(false));
}

#[test]
fn parse_args_recognises_archive_devices_combo() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-D"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.devices, Some(true));
    assert_eq!(parsed.specials, Some(true));
}

#[test]
fn parse_args_recognises_relative_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--relative"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.relative, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-relative"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.relative, Some(false));
}

#[test]
fn parse_args_recognises_one_file_system_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--one-file-system"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.one_file_system, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-one-file-system"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.one_file_system, Some(false));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-x"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.one_file_system, Some(true));
}

#[test]
fn parse_args_recognises_implied_dirs_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--implied-dirs"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.implied_dirs, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-implied-dirs"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.implied_dirs, Some(false));
}

#[test]
fn parse_args_recognises_prune_empty_dirs_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--prune-empty-dirs"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.prune_empty_dirs, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-prune-empty-dirs"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.prune_empty_dirs, Some(false));
}

#[test]
fn parse_args_recognises_inplace_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--inplace"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.inplace, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-inplace"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.inplace, Some(false));
}

#[test]
fn parse_args_recognises_append_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--append"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.append, Some(true));
    assert!(!parsed.append_verify);

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-append"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.append, Some(false));
    assert!(!parsed.append_verify);
}

#[test]
fn parse_args_captures_skip_compress_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--skip-compress=gz/mp3"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.skip_compress, Some(OsString::from("gz/mp3")));
}

#[test]
fn parse_args_recognises_append_verify_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--append-verify"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.append, Some(true));
    assert!(parsed.append_verify);
}

#[test]
fn parse_args_recognises_preallocate_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--preallocate"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.preallocate);

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.preallocate);
}

#[test]
fn parse_args_recognises_whole_file_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-W"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.whole_file, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-whole-file"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.whole_file, Some(false));
}

#[test]
fn parse_args_recognises_stats_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.stats);
}

#[test]
fn parse_args_recognises_human_readable_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--human-readable"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Enabled));
}

#[test]
fn parse_args_recognises_no_human_readable_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-human-readable"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Disabled));
}

#[test]
fn parse_args_recognises_human_readable_level_two() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--human-readable=2"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Combined));
}

#[test]
fn parse_args_recognises_msgs2stderr_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--msgs2stderr"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.msgs_to_stderr);
}

#[test]
fn parse_args_collects_out_format_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--out-format=%f %b"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.out_format, Some(OsString::from("%f %b")));
}

#[test]
fn parse_args_recognises_itemize_changes_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--itemize-changes"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.itemize_changes);
}

#[test]
fn parse_args_recognises_port_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--port=10873"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.daemon_port, Some(10873));
}

#[test]
fn parse_args_recognises_list_only_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.list_only);
    assert!(parsed.dry_run);
}

#[test]
fn parse_args_recognises_mkpath_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--mkpath"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.mkpath);
}

#[test]
fn parse_args_recognises_no_bwlimit_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-bwlimit"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(matches!(parsed.bwlimit, Some(BandwidthArgument::Disabled)));
}

#[test]
fn parse_args_no_bwlimit_overrides_bwlimit_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--bwlimit=2M"),
        OsString::from("--no-bwlimit"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(matches!(parsed.bwlimit, Some(BandwidthArgument::Disabled)));
}

#[test]
fn parse_args_collects_filter_patterns() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--exclude"),
        OsString::from("*.tmp"),
        OsString::from("--include"),
        OsString::from("important/**"),
        OsString::from("--filter"),
        OsString::from("+ staging/**"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.excludes, vec![OsString::from("*.tmp")]);
    assert_eq!(parsed.includes, vec![OsString::from("important/**")]);
    assert_eq!(parsed.filters, vec![OsString::from("+ staging/**")]);
}

#[test]
fn parse_args_collects_filter_files() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--exclude-from"),
        OsString::from("excludes.txt"),
        OsString::from("--include-from"),
        OsString::from("includes.txt"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.exclude_from, vec![OsString::from("excludes.txt")]);
    assert_eq!(parsed.include_from, vec![OsString::from("includes.txt")]);
}

#[test]
fn parse_args_collects_reference_destinations() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compare-dest"),
        OsString::from("compare"),
        OsString::from("--copy-dest"),
        OsString::from("copy"),
        OsString::from("--link-dest"),
        OsString::from("link"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compare_destinations, vec![OsString::from("compare")]);
    assert_eq!(parsed.copy_destinations, vec![OsString::from("copy")]);
    assert_eq!(parsed.link_destinations, vec![OsString::from("link")]);
}

#[test]
fn parse_args_collects_files_from_paths() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--files-from"),
        OsString::from("list-a"),
        OsString::from("--files-from"),
        OsString::from("list-b"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(
        parsed.files_from,
        vec![OsString::from("list-a"), OsString::from("list-b")]
    );
}

#[test]
fn parse_args_sets_from0_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--from0"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.from0);
}

#[test]
fn parse_args_recognises_password_file() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--password-file"),
        OsString::from("secret.txt"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.password_file, Some(OsString::from("secret.txt")));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--password-file=secrets.d"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.password_file, Some(OsString::from("secrets.d")));
}

#[test]
fn chown_requires_non_empty_components() {
    let error = parse_chown_argument(OsStr::new("")).expect_err("empty --chown spec should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("--chown requires a non-empty USER and/or GROUP"),
        "diagnostic missing non-empty message: {rendered}"
    );

    let colon_error =
        parse_chown_argument(OsStr::new(":")).expect_err("missing user and group should fail");
    let colon_rendered = colon_error.to_string();
    assert!(
        colon_rendered.contains("--chown requires a user and/or group"),
        "diagnostic missing user/group message: {colon_rendered}"
    );
}

#[test]
fn parse_args_recognises_no_motd_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-motd"),
        OsString::from("rsync://example/"),
    ])
    .expect("parse");

    assert!(parsed.no_motd);
}

#[test]
fn parse_args_collects_protocol_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--protocol=30"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.protocol, Some(OsString::from("30")));
}

#[test]
fn parse_args_collects_timeout_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--timeout=90"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.timeout, Some(OsString::from("90")));
}

#[test]
fn parse_args_collects_contimeout_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--contimeout=45"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.contimeout, Some(OsString::from("45")));
}

#[test]
fn parse_args_collects_max_delete_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--max-delete=12"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.max_delete, Some(OsString::from("12")));
}

#[test]
fn parse_args_collects_checksum_seed_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--checksum-seed=42"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.checksum_seed, Some(42));
}

#[test]
fn parse_args_collects_min_max_size_values() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--min-size=1.5K"),
        OsString::from("--max-size=2M"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.min_size, Some(OsString::from("1.5K")));
    assert_eq!(parsed.max_size, Some(OsString::from("2M")));
}

#[test]
fn parse_max_delete_argument_accepts_zero() {
    let limit = parse_max_delete_argument(OsStr::new("0")).expect("parse max-delete");
    assert_eq!(limit, 0);
}

#[test]
fn parse_size_limit_argument_accepts_fractional_units() {
    let value =
        parse_size_limit_argument(OsStr::new("1.5K"), "--min-size").expect("parse size limit");
    assert_eq!(value, 1536);
}

#[test]
fn parse_size_limit_argument_rejects_negative() {
    let error =
        parse_size_limit_argument(OsStr::new("-2"), "--min-size").expect_err("negative rejected");
    let rendered = error.to_string();
    assert!(
        rendered.contains("size must be non-negative"),
        "missing detail: {rendered}"
    );
}

#[test]
fn parse_size_limit_argument_rejects_invalid_suffix() {
    let error = parse_size_limit_argument(OsStr::new("10QB"), "--max-size")
        .expect_err("invalid suffix rejected");
    let rendered = error.to_string();
    assert!(
        rendered.contains("expected a size with an optional"),
        "missing message: {rendered}"
    );
}

#[test]
fn pow_u128_for_size_accepts_zero_exponent() {
    let result = pow_u128_for_size(1024, 0).expect("pow for zero exponent");
    assert_eq!(result, 1);
}

#[test]
fn pow_u128_for_size_reports_overflow() {
    let result = pow_u128_for_size(u32::MAX, 5);
    assert!(matches!(result, Err(SizeParseError::TooLarge)));
}

#[test]
fn parse_max_delete_argument_rejects_negative() {
    let error =
        parse_max_delete_argument(OsStr::new("-4")).expect_err("negative limit should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("deletion limit must be non-negative"),
        "diagnostic missing detail: {rendered}"
    );
}

#[test]
fn parse_max_delete_argument_rejects_non_numeric() {
    let error =
        parse_max_delete_argument(OsStr::new("abc")).expect_err("non-numeric limit should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("deletion limit must be an unsigned integer"),
        "diagnostic missing unsigned message: {rendered}"
    );
}

#[test]
fn parse_checksum_seed_argument_accepts_zero() {
    let seed = parse_checksum_seed_argument(OsStr::new("0")).expect("parse checksum seed");
    assert_eq!(seed, 0);
}

#[test]
fn parse_checksum_seed_argument_accepts_max_u32() {
    let seed = parse_checksum_seed_argument(OsStr::new("4294967295")).expect("parse checksum seed");
    assert_eq!(seed, u32::MAX);
}

#[test]
fn parse_checksum_seed_argument_rejects_negative() {
    let error =
        parse_checksum_seed_argument(OsStr::new("-1")).expect_err("negative seed should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("must be non-negative"),
        "diagnostic missing negativity detail: {rendered}"
    );
}

#[test]
fn parse_checksum_seed_argument_rejects_non_numeric() {
    let error =
        parse_checksum_seed_argument(OsStr::new("seed")).expect_err("non-numeric seed should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("invalid --checksum-seed value"),
        "diagnostic missing invalid message: {rendered}"
    );
}

#[test]
fn parse_modify_window_argument_accepts_positive_values() {
    let value = parse_modify_window_argument(OsStr::new("  42 ")).expect("parse modify-window");
    assert_eq!(value, 42);
}

#[test]
fn parse_modify_window_argument_rejects_negative_values() {
    let error = parse_modify_window_argument(OsStr::new("-1"))
        .expect_err("negative modify-window should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("window must be non-negative"),
        "diagnostic missing negativity detail: {rendered}"
    );
}

#[test]
fn parse_modify_window_argument_rejects_invalid_values() {
    let error = parse_modify_window_argument(OsStr::new("abc"))
        .expect_err("non-numeric modify-window should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("window must be an unsigned integer"),
        "diagnostic missing numeric detail: {rendered}"
    );
}

#[test]
fn parse_modify_window_argument_rejects_empty_values() {
    let error = parse_modify_window_argument(OsStr::new("   "))
        .expect_err("empty modify-window should fail");
    let rendered = error.to_string();
    assert!(
        rendered.contains("value must not be empty"),
        "diagnostic missing emptiness detail: {rendered}"
    );
}

#[test]
fn timeout_argument_zero_disables_timeout() {
    let timeout = parse_timeout_argument(OsStr::new("0")).expect("parse timeout");
    assert_eq!(timeout, TransferTimeout::Disabled);
}

#[test]
fn timeout_argument_positive_sets_seconds() {
    let timeout = parse_timeout_argument(OsStr::new("15")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(15));
}

#[test]
fn timeout_argument_negative_reports_error() {
    let error = parse_timeout_argument(OsStr::new("-1")).unwrap_err();
    assert!(error.to_string().contains("timeout must be non-negative"));
}

#[test]
fn bind_address_argument_accepts_ipv4_literal() {
    let parsed = parse_bind_address_argument(OsStr::new("192.0.2.1")).expect("parse bind address");
    let expected = "192.0.2.1".parse::<IpAddr>().expect("ip literal");
    assert_eq!(parsed.socket().ip(), expected);
    assert_eq!(parsed.raw(), OsStr::new("192.0.2.1"));
}

#[test]
fn bind_address_argument_rejects_empty_value() {
    let error = parse_bind_address_argument(OsStr::new(" ")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("--address requires a non-empty value")
    );
}

#[test]
fn out_format_argument_accepts_supported_placeholders() {
    let format = parse_out_format(OsStr::new(
        "%f %b %c %l %o %M %B %L %N %p %u %g %U %G %t %i %h %a %m %P %C %%",
    ))
    .expect("parse out-format");
    assert!(!format.is_empty());
}

#[test]
fn out_format_argument_rejects_unknown_placeholders() {
    let error = parse_out_format(OsStr::new("%z")).unwrap_err();
    assert!(error.to_string().contains("unsupported --out-format"));
}

#[test]
fn out_format_remote_placeholders_preserve_literals_without_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("file.txt");
    std::fs::write(&source, b"payload").expect("write source");
    let destination = dst_dir.join("file.txt");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%h %a %m %P"))
        .expect("parse placeholders")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render placeholders");

    let rendered = String::from_utf8(output).expect("utf8");
    assert_eq!(rendered, "%h %a %m %P\n");
}

#[test]
fn out_format_renders_full_checksum_for_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src dir");
    std::fs::create_dir(&dst_dir).expect("create dst dir");

    let source = src_dir.join("file.bin");
    let contents = b"checksum payload";
    std::fs::write(&source, contents).expect("write source");
    let destination = dst_dir.join("file.bin");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| event.relative_path().to_string_lossy() == "file.bin")
        .expect("file event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%C"))
        .expect("parse %C")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %C");

    let rendered = String::from_utf8(output).expect("utf8");
    let mut hasher = Md5::new();
    hasher.update(contents);
    let digest = hasher.finalize();
    let expected: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(rendered, format!("{expected}\n"));
}

#[test]
fn out_format_renders_full_checksum_for_non_file_entries_as_spaces() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir_all(src_dir.join("nested")).expect("create source tree");
    std::fs::create_dir(&dst_dir).expect("create destination root");
    std::fs::write(src_dir.join("nested").join("file.txt"), b"contents").expect("write file");

    let source_operand = OsString::from(format!("{}/", src_dir.display()));
    let dest_operand = OsString::from(format!("{}/", dst_dir.display()));

    let config = ClientConfig::builder()
        .transfer_args([source_operand, dest_operand])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let dir_event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DirectoryCreated))
        .expect("directory event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%C"))
        .expect("parse %C")
        .render(dir_event, &OutFormatContext::default(), &mut output)
        .expect("render %C");

    let rendered = String::from_utf8(output).expect("utf8");
    assert_eq!(rendered.len(), 33);
    assert!(rendered[..32].chars().all(|ch| ch == ' '));
    assert_eq!(rendered.as_bytes()[32], b'\n');
}

#[test]
fn out_format_renders_checksum_bytes_for_data_copy_events() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("payload.bin");
    let contents = b"checksum-bytes";
    std::fs::write(&source, contents).expect("write source");
    let destination = dst_dir.join("payload.bin");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .times(true)
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%c"))
        .expect("parse %c")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %c");

    let rendered = String::from_utf8(output).expect("utf8");
    assert_eq!(rendered.trim_end(), contents.len().to_string());
}

#[test]
fn out_format_renders_checksum_bytes_as_zero_when_metadata_reused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("unchanged.txt");
    std::fs::write(&source, b"same contents").expect("write source");
    let destination = dst_dir.join("unchanged.txt");

    let build_config = || {
        ClientConfig::builder()
            .transfer_args([
                source.as_os_str().to_os_string(),
                destination.as_os_str().to_os_string(),
            ])
            .times(true)
            .force_event_collection(true)
            .build()
    };

    // First run populates the destination.
    let outcome = run_client_or_fallback::<io::Sink, io::Sink>(build_config(), None, None)
        .expect("initial copy");
    if let ClientOutcome::Fallback(_) = outcome {
        panic!("unexpected fallback outcome during initial copy");
    }

    // Second run should reuse metadata and avoid copying data bytes.
    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(build_config(), None, None).expect("re-run");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::MetadataReused))
        .expect("metadata reuse event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%c"))
        .expect("parse %c")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %c");

    let rendered = String::from_utf8(output).expect("utf8");
    assert_eq!(rendered.trim_end(), "0");
}

#[cfg(unix)]
#[test]
fn out_format_renders_permission_and_identity_placeholders() {
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;
    use users::{get_group_by_gid, get_user_by_uid, gid_t, uid_t};

    let temp = tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    fs::create_dir(&src_dir).expect("create src");
    fs::create_dir(&dst_dir).expect("create dst");
    let source = src_dir.join("script.sh");
    fs::write(&source, b"echo ok\n").expect("write source");

    let expected_uid = fs::metadata(&source).expect("source metadata").uid();
    let expected_gid = fs::metadata(&source).expect("source metadata").gid();
    let expected_user = get_user_by_uid(expected_uid as uid_t)
        .map(|user| user.name().to_string_lossy().into_owned())
        .unwrap_or_else(|| expected_uid.to_string());
    let expected_group = get_group_by_gid(expected_gid as gid_t)
        .map(|group| group.name().to_string_lossy().into_owned())
        .unwrap_or_else(|| expected_gid.to_string());

    let mut permissions = fs::metadata(&source)
        .expect("source metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&source, permissions).expect("set permissions");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            dst_dir.as_os_str().to_os_string(),
        ])
        .permissions(true)
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };
    let event = summary
        .events()
        .iter()
        .find(|event| {
            event
                .relative_path()
                .to_string_lossy()
                .contains("script.sh")
        })
        .expect("event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%B"))
        .expect("parse out-format")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %B");
    assert_eq!(output, b"rwxr-xr-x\n");

    output.clear();
    parse_out_format(OsStr::new("%p"))
        .expect("parse %p")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %p");
    let expected_pid = format!("{}\n", std::process::id());
    assert_eq!(output, expected_pid.as_bytes());

    output.clear();
    parse_out_format(OsStr::new("%U"))
        .expect("parse %U")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %U");
    assert_eq!(output, format!("{expected_uid}\n").as_bytes());

    output.clear();
    parse_out_format(OsStr::new("%G"))
        .expect("parse %G")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %G");
    assert_eq!(output, format!("{expected_gid}\n").as_bytes());

    output.clear();
    parse_out_format(OsStr::new("%u"))
        .expect("parse %u")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %u");
    assert_eq!(output, format!("{expected_user}\n").as_bytes());

    output.clear();
    parse_out_format(OsStr::new("%g"))
        .expect("parse %g")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %g");
    assert_eq!(output, format!("{expected_group}\n").as_bytes());
}

#[test]
fn out_format_renders_modify_time_placeholder() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("file.txt");
    std::fs::write(&source, b"data").expect("write source");
    let destination = temp.path().join("dest");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };
    let event = summary
        .events()
        .iter()
        .find(|event| event.relative_path().to_string_lossy().contains("file.txt"))
        .expect("event present");

    let format = parse_out_format(OsStr::new("%M")).expect("parse out-format");
    let mut output = Vec::new();
    format
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render out-format");

    assert!(String::from_utf8_lossy(&output).trim().contains('-'));
}

#[test]
fn out_format_renders_itemized_placeholder_for_new_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src dir");
    std::fs::create_dir(&dst_dir).expect("create dst dir");

    let source = src_dir.join("file.txt");
    std::fs::write(&source, b"content").expect("write source");
    let destination = dst_dir.join("file.txt");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| event.relative_path().to_string_lossy() == "file.txt")
        .expect("event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%i"))
        .expect("parse %i")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %i");

    assert_eq!(output, b">f+++++++++\n");
}

#[test]
fn out_format_itemized_placeholder_reports_deletion() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src dir");
    std::fs::create_dir(&dst_dir).expect("create dst dir");

    let destination_file = dst_dir.join("obsolete.txt");
    std::fs::write(&destination_file, b"old").expect("write obsolete");

    let source_operand = OsString::from(format!("{}/", src_dir.display()));
    let dest_operand = OsString::from(format!("{}/", dst_dir.display()));

    let config = ClientConfig::builder()
        .transfer_args([source_operand, dest_operand])
        .delete(true)
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let mut events = summary.events().iter();
    let event = events
        .find(|event| event.relative_path().to_string_lossy() == "obsolete.txt")
        .unwrap_or_else(|| {
            let recorded: Vec<_> = summary
                .events()
                .iter()
                .map(|event| event.relative_path().to_string_lossy().into_owned())
                .collect();
            panic!("deletion event missing, recorded events: {recorded:?}");
        });

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%i"))
        .expect("parse %i")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %i");

    assert_eq!(output, b"*deleting\n");
}

#[test]
fn parse_args_sets_compress_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-z"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.compress);
}

#[test]
fn parse_args_no_compress_overrides_compress_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-z"),
        OsString::from("--no-compress"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.compress);
    assert!(parsed.no_compress);
}

#[test]
fn parse_args_records_compress_level_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=5"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_level, Some(OsString::from("5")));
}

#[test]
fn parse_args_compress_level_zero_records_disable() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=0"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_level, Some(OsString::from("0")));
}

#[test]
fn parse_args_recognises_compress_level_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=5"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.compress);
    assert_eq!(parsed.compress_level, Some(OsString::from("5")));
}

#[test]
fn parse_args_compress_level_zero_disables_compress() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=0"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.compress);
    assert_eq!(parsed.compress_level, Some(OsString::from("0")));
}

#[test]
fn parse_compress_level_argument_rejects_invalid_value() {
    let error = parse_compress_level_argument(OsStr::new("fast")).unwrap_err();
    let rendered = error.to_string();
    assert!(rendered.contains("invalid compression level"));
    assert!(rendered.contains("integer"));

    let range_error = parse_compress_level_argument(OsStr::new("12")).unwrap_err();
    let rendered_range = range_error.to_string();
    assert!(rendered_range.contains("outside the supported range"));
}

#[test]
fn parse_filter_directive_accepts_include_and_exclude() {
    let include = parse_filter_directive(OsStr::new("+ assets/**")).expect("include rule parses");
    assert_eq!(
        include,
        FilterDirective::Rule(FilterRuleSpec::include("assets/**".to_string()))
    );

    let exclude = parse_filter_directive(OsStr::new("- *.bak")).expect("exclude rule parses");
    assert_eq!(
        exclude,
        FilterDirective::Rule(FilterRuleSpec::exclude("*.bak".to_string()))
    );

    let include_keyword =
        parse_filter_directive(OsStr::new("include logs/**")).expect("keyword include parses");
    assert_eq!(
        include_keyword,
        FilterDirective::Rule(FilterRuleSpec::include("logs/**".to_string()))
    );

    let exclude_keyword =
        parse_filter_directive(OsStr::new("exclude *.tmp")).expect("keyword exclude parses");
    assert_eq!(
        exclude_keyword,
        FilterDirective::Rule(FilterRuleSpec::exclude("*.tmp".to_string()))
    );

    let protect_keyword =
        parse_filter_directive(OsStr::new("protect backups/**")).expect("keyword protect parses");
    assert_eq!(
        protect_keyword,
        FilterDirective::Rule(FilterRuleSpec::protect("backups/**".to_string()))
    );
}

#[test]
fn parse_filter_directive_accepts_hide_and_show_keywords() {
    let show_keyword =
        parse_filter_directive(OsStr::new("show images/**")).expect("keyword show parses");
    assert_eq!(
        show_keyword,
        FilterDirective::Rule(FilterRuleSpec::show("images/**".to_string()))
    );

    let hide_keyword =
        parse_filter_directive(OsStr::new("hide *.swp")).expect("keyword hide parses");
    assert_eq!(
        hide_keyword,
        FilterDirective::Rule(FilterRuleSpec::hide("*.swp".to_string()))
    );
}

#[test]
fn parse_filter_directive_accepts_risk_keyword_and_shorthand() {
    let risk_keyword =
        parse_filter_directive(OsStr::new("risk backups/**")).expect("keyword risk parses");
    assert_eq!(
        risk_keyword,
        FilterDirective::Rule(FilterRuleSpec::risk("backups/**".to_string()))
    );

    let risk_shorthand =
        parse_filter_directive(OsStr::new("R logs/**")).expect("shorthand risk parses");
    assert_eq!(
        risk_shorthand,
        FilterDirective::Rule(FilterRuleSpec::risk("logs/**".to_string()))
    );
}

#[test]
fn parse_filter_directive_accepts_shorthand_hide_show_and_protect() {
    let protect =
        parse_filter_directive(OsStr::new("P backups/**")).expect("shorthand protect parses");
    assert_eq!(
        protect,
        FilterDirective::Rule(FilterRuleSpec::protect("backups/**".to_string()))
    );

    let hide = parse_filter_directive(OsStr::new("H *.tmp")).expect("shorthand hide parses");
    assert_eq!(
        hide,
        FilterDirective::Rule(FilterRuleSpec::hide("*.tmp".to_string()))
    );

    let show = parse_filter_directive(OsStr::new("S public/**")).expect("shorthand show parses");
    assert_eq!(
        show,
        FilterDirective::Rule(FilterRuleSpec::show("public/**".to_string()))
    );
}

#[test]
fn parse_filter_directive_accepts_exclude_if_present() {
    let directive = parse_filter_directive(OsStr::new("exclude-if-present marker"))
        .expect("exclude-if-present with whitespace parses");
    assert_eq!(
        directive,
        FilterDirective::Rule(FilterRuleSpec::exclude_if_present("marker".to_string()))
    );

    let equals_variant = parse_filter_directive(OsStr::new("exclude-if-present=.skip"))
        .expect("exclude-if-present with equals parses");
    assert_eq!(
        equals_variant,
        FilterDirective::Rule(FilterRuleSpec::exclude_if_present(".skip".to_string()))
    );
}

#[test]
fn parse_filter_directive_rejects_exclude_if_present_without_marker() {
    let error = parse_filter_directive(OsStr::new("exclude-if-present   "))
        .expect_err("missing marker should error");
    let rendered = error.to_string();
    assert!(rendered.contains("missing a marker file"));
}

#[test]
fn parse_filter_directive_accepts_clear_directive() {
    let clear = parse_filter_directive(OsStr::new("!")).expect("clear directive parses");
    assert_eq!(clear, FilterDirective::Clear);

    let clear_with_whitespace =
        parse_filter_directive(OsStr::new("  !   ")).expect("clear with whitespace parses");
    assert_eq!(clear_with_whitespace, FilterDirective::Clear);
}

#[test]
fn parse_filter_directive_accepts_clear_keyword() {
    let keyword = parse_filter_directive(OsStr::new("clear")).expect("keyword parses");
    assert_eq!(keyword, FilterDirective::Clear);

    let uppercase = parse_filter_directive(OsStr::new("  CLEAR  ")).expect("uppercase parses");
    assert_eq!(uppercase, FilterDirective::Clear);
}

#[test]
fn parse_filter_directive_rejects_clear_with_trailing_characters() {
    let error =
        parse_filter_directive(OsStr::new("! comment")).expect_err("trailing text should error");
    let rendered = error.to_string();
    assert!(rendered.contains("'!' rule has trailing characters: ! comment"));

    let error = parse_filter_directive(OsStr::new("!extra")).expect_err("suffix should error");
    let rendered = error.to_string();
    assert!(rendered.contains("'!' rule has trailing characters: !extra"));
}

#[test]
fn parse_filter_directive_rejects_missing_pattern() {
    let error =
        parse_filter_directive(OsStr::new("+   ")).expect_err("missing pattern should error");
    let rendered = error.to_string();
    assert!(rendered.contains("missing a pattern"));

    let shorthand_error =
        parse_filter_directive(OsStr::new("P   ")).expect_err("shorthand protect requires pattern");
    let rendered = shorthand_error.to_string();
    assert!(rendered.contains("missing a pattern"));
}

#[test]
fn parse_filter_directive_accepts_merge() {
    let directive =
        parse_filter_directive(OsStr::new("merge filters.txt")).expect("merge directive");
    let (options, _) = parse_merge_modifiers("", "merge filters.txt", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from("filters.txt"), None).with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_rejects_merge_without_path() {
    let error =
        parse_filter_directive(OsStr::new("merge ")).expect_err("missing merge path should error");
    let rendered = error.to_string();
    assert!(rendered.contains("missing a file path"));
}

#[test]
fn parse_filter_directive_accepts_merge_with_forced_include() {
    let directive =
        parse_filter_directive(OsStr::new("merge,+ rules")).expect("merge,+ should parse");
    let (options, _) = parse_merge_modifiers("+", "merge,+ rules", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from("rules"), Some(FilterRuleKind::Include))
        .with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_merge_with_forced_exclude() {
    let directive =
        parse_filter_directive(OsStr::new("merge,- rules")).expect("merge,- should parse");
    let (options, _) = parse_merge_modifiers("-", "merge,- rules", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from("rules"), Some(FilterRuleKind::Exclude))
        .with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_merge_with_cvs_alias() {
    let directive = parse_filter_directive(OsStr::new("merge,C")).expect("merge,C should parse");
    let (options, _) = parse_merge_modifiers("C", "merge,C", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from(".cvsignore"), Some(FilterRuleKind::Exclude))
        .with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_short_merge() {
    let directive =
        parse_filter_directive(OsStr::new(". per-dir")).expect("short merge directive parses");
    let (options, _) = parse_merge_modifiers("", ". per-dir", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from("per-dir"), None).with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_short_merge_with_cvs_alias() {
    let directive =
        parse_filter_directive(OsStr::new(".C")).expect("short merge directive with 'C' parses");
    let (options, _) = parse_merge_modifiers("C", ".C", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from(".cvsignore"), Some(FilterRuleKind::Exclude))
        .with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_merge_sender_modifier() {
    let directive = parse_filter_directive(OsStr::new("merge,s rules"))
        .expect("merge directive with 's' parses");
    let expected_options = DirMergeOptions::default()
        .allow_list_clearing(true)
        .sender_modifier();
    let expected =
        MergeDirective::new(OsString::from("rules"), None).with_options(expected_options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_merge_anchor_and_whitespace_modifiers() {
    let directive = parse_filter_directive(OsStr::new("merge,/w patterns"))
        .expect("merge directive with '/' and 'w' parses");
    let expected_options = DirMergeOptions::default()
        .allow_list_clearing(true)
        .anchor_root(true)
        .use_whitespace()
        .allow_comments(false);
    let expected =
        MergeDirective::new(OsString::from("patterns"), None).with_options(expected_options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_rejects_merge_with_unknown_modifier() {
    let error = parse_filter_directive(OsStr::new("merge,x rules"))
        .expect_err("merge with unsupported modifier should error");
    let rendered = error.to_string();
    assert!(rendered.contains("uses unsupported modifier"));
}

#[test]
fn parse_filter_directive_accepts_dir_merge_without_modifiers() {
    let directive = parse_filter_directive(OsStr::new("dir-merge .rsync-filter"))
        .expect("dir-merge without modifiers parses");
    assert_eq!(
        directive,
        FilterDirective::Rule(FilterRuleSpec::dir_merge(
            ".rsync-filter".to_string(),
            DirMergeOptions::default(),
        )),
    );
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_remove_modifier() {
    let directive = parse_filter_directive(OsStr::new("dir-merge,- .rsync-filter"))
        .expect("dir-merge with '-' modifier parses");
    assert_eq!(
        directive,
        FilterDirective::Rule(FilterRuleSpec::dir_merge(
            ".rsync-filter".to_string(),
            DirMergeOptions::default().with_enforced_kind(Some(DirMergeEnforcedKind::Exclude)),
        ))
    );
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_include_modifier() {
    let directive = parse_filter_directive(OsStr::new("dir-merge,+ .rsync-filter"))
        .expect("dir-merge with '+' modifier parses");

    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };

    assert_eq!(rule.pattern(), ".rsync-filter");
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Include));
    assert!(options.inherit_rules());
    assert!(!options.excludes_self());
}

#[test]
fn parse_filter_directive_accepts_short_dir_merge() {
    let directive =
        parse_filter_directive(OsStr::new(": rules")).expect("short dir-merge directive parses");

    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };

    assert_eq!(rule.pattern(), "rules");
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(options.inherit_rules());
    assert!(!options.excludes_self());
}

#[test]
fn parse_filter_directive_accepts_short_dir_merge_with_exclude_modifier() {
    let directive = parse_filter_directive(OsStr::new(":- per-dir"))
        .expect("short dir-merge with '-' modifier parses");

    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };

    assert_eq!(rule.pattern(), "per-dir");
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_no_inherit_modifier() {
    let directive = parse_filter_directive(OsStr::new("dir-merge,n per-dir"))
        .expect("dir-merge with 'n' modifier parses");

    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };

    assert_eq!(rule.pattern(), "per-dir");
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(!options.inherit_rules());
    assert!(options.allows_comments());
    assert!(!options.uses_whitespace());
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_exclude_self_modifier() {
    let directive = parse_filter_directive(OsStr::new("dir-merge,e per-dir"))
        .expect("dir-merge with 'e' modifier parses");

    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };

    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(options.excludes_self());
    assert!(options.inherit_rules());
    assert!(!options.uses_whitespace());
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_whitespace_modifier() {
    let directive = parse_filter_directive(OsStr::new("dir-merge,w per-dir"))
        .expect("dir-merge with 'w' modifier parses");

    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };

    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(options.uses_whitespace());
    assert!(!options.allows_comments());
}

#[test]
fn collect_filter_arguments_merges_shortcuts_with_filters() {
    use std::ffi::OsString;

    let filters = vec![OsString::from("+ foo"), OsString::from("- bar")];
    let filter_indices = vec![5_usize, 9_usize];
    let rsync_indices = vec![1_usize, 7_usize];

    let merged = collect_filter_arguments(&filters, &filter_indices, &rsync_indices);

    assert_eq!(
        merged,
        vec![
            OsString::from("dir-merge /.rsync-filter"),
            OsString::from("exclude .rsync-filter"),
            OsString::from("+ foo"),
            OsString::from("dir-merge .rsync-filter"),
            OsString::from("- bar"),
        ]
    );
}

#[test]
fn collect_filter_arguments_handles_shortcuts_without_filters() {
    use std::ffi::OsString;

    let filters: Vec<OsString> = Vec::new();
    let filter_indices: Vec<usize> = Vec::new();
    let merged = collect_filter_arguments(&filters, &filter_indices, &[2_usize, 4_usize]);

    assert_eq!(
        merged,
        vec![
            OsString::from("dir-merge /.rsync-filter"),
            OsString::from("exclude .rsync-filter"),
            OsString::from("dir-merge .rsync-filter"),
        ]
    );
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_cvs_modifier() {
    let directive = parse_filter_directive(OsStr::new("dir-merge,C"))
        .expect("dir-merge with 'C' modifier parses");

    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };

    assert_eq!(rule.pattern(), ".cvsignore");
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
    assert!(options.uses_whitespace());
    assert!(!options.allows_comments());
    assert!(!options.inherit_rules());
    assert!(options.list_clear_allowed());
}

#[test]
fn parse_filter_directive_rejects_dir_merge_with_conflicting_modifiers() {
    let error = parse_filter_directive(OsStr::new("dir-merge,+- per-dir"))
        .expect_err("conflicting modifiers should error");
    let rendered = error.to_string();
    assert!(rendered.contains("cannot combine '+' and '-'"));
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_sender_modifier() {
    let directive = parse_filter_directive(OsStr::new("dir-merge,s per-dir"))
        .expect("dir-merge with 's' modifier parses");
    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(options.applies_to_sender());
    assert!(!options.applies_to_receiver());
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_receiver_modifier() {
    let directive = parse_filter_directive(OsStr::new("dir-merge,r per-dir"))
        .expect("dir-merge with 'r' modifier parses");
    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(!options.applies_to_sender());
    assert!(options.applies_to_receiver());
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_anchor_modifier() {
    let directive = parse_filter_directive(OsStr::new("dir-merge,/ .rules"))
        .expect("dir-merge with '/' modifier parses");
    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(options.anchor_root_enabled());
}

#[test]
fn transfer_request_with_times_preserves_timestamp() {
    use filetime::{FileTime, set_file_times};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-times.txt");
    let destination = tmp.path().join("dest-times.txt");
    std::fs::write(&source, b"data").expect("write source");
    let mtime = FileTime::from_unix_time(1_700_090_000, 500_000_000);
    set_file_times(&source, mtime, mtime).expect("set times");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--times"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let metadata = std::fs::metadata(&destination).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(dest_mtime, mtime);
}

#[test]
fn transfer_request_with_filter_excludes_patterns() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        OsString::from("- *.tmp"),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.tmp").exists());
}

#[test]
fn transfer_request_with_filter_clear_resets_rules() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        OsString::from("- *.tmp"),
        OsString::from("--filter"),
        OsString::from("!"),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(copied_root.join("skip.tmp").exists());
}

#[test]
fn transfer_request_with_filter_merge_applies_rules() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let filter_file = tmp.path().join("filters.txt");
    std::fs::write(&filter_file, "- *.tmp\n").expect("write filter file");

    let filter_arg = OsString::from(format!("merge {}", filter_file.display()));

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        filter_arg,
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.tmp").exists());
}

#[test]
fn transfer_request_with_filter_merge_clear_resets_rules() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write tmp");
    std::fs::write(source_root.join("skip.log"), b"log").expect("write log");

    let filter_file = tmp.path().join("filters.txt");
    std::fs::write(&filter_file, "- *.tmp\n!\n- *.log\n").expect("write filter file");

    let filter_arg = OsString::from(format!("merge {}", filter_file.display()));

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        filter_arg,
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(copied_root.join("skip.tmp").exists());
    assert!(!copied_root.join("skip.log").exists());
}

#[test]
fn transfer_request_with_filter_protect_preserves_destination_entry() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");

    let dest_subdir = dest_root.join("source");
    std::fs::create_dir_all(&dest_subdir).expect("create destination contents");
    std::fs::write(dest_subdir.join("keep.txt"), b"keep").expect("write dest keep");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--delete"),
        OsString::from("--filter"),
        OsString::from("protect keep.txt"),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
}

#[test]
fn transfer_request_with_delete_excluded_prunes_filtered_entries() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");

    let dest_subdir = dest_root.join("source");
    std::fs::create_dir_all(&dest_subdir).expect("create destination contents");
    std::fs::write(dest_subdir.join("skip.log"), b"skip").expect("write excluded file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--delete-excluded"),
        OsString::from("--exclude=*.log"),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.log").exists());
}

#[test]
fn transfer_request_with_filter_merge_detects_recursion() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");

    let filter_file = tmp.path().join("filters.txt");
    std::fs::write(&filter_file, format!("merge {}\n", filter_file.display()))
        .expect("write recursive filter");

    let filter_arg = OsString::from(format!("merge {}", filter_file.display()));

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--filter"),
        filter_arg,
        source_root.into_os_string(),
        dest_root.into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8_lossy(&stderr);
    assert!(rendered.contains("recursive filter merge"));
}

#[test]
fn transfer_request_with_exclude_from_skips_patterns() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let exclude_file = tmp.path().join("filters.txt");
    std::fs::write(&exclude_file, "# comment\n\n*.tmp\n").expect("write filters");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--exclude-from"),
        exclude_file.as_os_str().to_os_string(),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.tmp").exists());
}

#[test]
fn transfer_request_with_cvs_exclude_skips_default_patterns() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _home_guard = EnvGuard::set("HOME", OsStr::new(""));
    let _cvs_guard = EnvGuard::set("CVSIGNORE", OsStr::new(""));

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("core"), b"core").expect("write core");
    let git_dir = source_root.join(".git");
    std::fs::create_dir_all(&git_dir).expect("create git dir");
    std::fs::write(git_dir.join("config"), b"git").expect("write git config");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--cvs-exclude"),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("core").exists());
    assert!(!copied_root.join(".git").exists());
}

#[test]
fn transfer_request_with_cvs_exclude_respects_cvsignore_files() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _home_guard = EnvGuard::set("HOME", OsStr::new(""));
    let _cvs_guard = EnvGuard::set("CVSIGNORE", OsStr::new(""));

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.log"), b"skip").expect("write skip");
    std::fs::write(source_root.join(".cvsignore"), b"skip.log\n").expect("write cvsignore");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--cvs-exclude"),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.log").exists());
}

#[test]
fn transfer_request_with_cvs_exclude_respects_cvsignore_env() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _home_guard = EnvGuard::set("HOME", OsStr::new(""));
    let _cvs_guard = EnvGuard::set("CVSIGNORE", OsStr::new("*.tmp"));

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--cvs-exclude"),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.tmp").exists());
}

#[test]
fn transfer_request_with_include_from_reinstate_patterns() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    let keep_dir = source_root.join("keep");
    std::fs::create_dir_all(&keep_dir).expect("create keep dir");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(keep_dir.join("file.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let include_file = tmp.path().join("includes.txt");
    std::fs::write(&include_file, "keep/\nkeep/**\n").expect("write include file");

    let mut expected_rules = Vec::new();
    expected_rules.push(FilterRuleSpec::exclude("*".to_string()));
    append_filter_rules_from_files(
        &mut expected_rules,
        &[include_file.as_os_str().to_os_string()],
        FilterRuleKind::Include,
    )
    .expect("load include patterns");

    let engine_rules = expected_rules.iter().filter_map(|rule| match rule.kind() {
        FilterRuleKind::Include => Some(EngineFilterRule::include(rule.pattern())),
        FilterRuleKind::Exclude => Some(EngineFilterRule::exclude(rule.pattern())),
        FilterRuleKind::Clear => None,
        FilterRuleKind::Protect => Some(EngineFilterRule::protect(rule.pattern())),
        FilterRuleKind::Risk => Some(EngineFilterRule::risk(rule.pattern())),
        FilterRuleKind::ExcludeIfPresent => None,
        FilterRuleKind::DirMerge => None,
    });
    let filter_set = FilterSet::from_rules(engine_rules).expect("filters");
    assert!(filter_set.allows(std::path::Path::new("keep"), true));
    assert!(filter_set.allows(std::path::Path::new("keep/file.txt"), false));
    assert!(!filter_set.allows(std::path::Path::new("skip.tmp"), false));

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--exclude"),
        OsString::from("*"),
        OsString::from("--include-from"),
        include_file.as_os_str().to_os_string(),
        source_operand,
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    assert!(dest_root.join("keep/file.txt").exists());
    assert!(!dest_root.join("skip.tmp").exists());
}

#[test]
fn transfer_request_reports_filter_file_errors() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--exclude-from"),
        OsString::from("missing.txt"),
        OsString::from("src"),
        OsString::from("dst"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
    assert!(rendered.contains("failed to read filter file 'missing.txt'"));
    assert_contains_client_trailer(&rendered);
}

#[test]
fn load_filter_file_patterns_skips_comments_and_trims_crlf() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("filters.txt");
    std::fs::write(&path, b"# comment\r\n\r\n include \r\npattern\r\n").expect("write filters");

    let patterns =
        load_filter_file_patterns(path.as_path()).expect("load filter patterns succeeds");

    assert_eq!(
        patterns,
        vec![" include ".to_string(), "pattern".to_string()]
    );
}

#[test]
fn load_filter_file_patterns_skip_semicolon_comments() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("filters-semicolon.txt");
    std::fs::write(&path, b"; leading comment\n  ; spaced comment\nkeep\n").expect("write filters");

    let patterns =
        load_filter_file_patterns(path.as_path()).expect("load filter patterns succeeds");

    assert_eq!(patterns, vec!["keep".to_string()]);
}

#[test]
fn load_filter_file_patterns_handles_invalid_utf8() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("filters.bin");
    std::fs::write(&path, [0xFFu8, b'\n']).expect("write invalid bytes");

    let patterns =
        load_filter_file_patterns(path.as_path()).expect("load filter patterns succeeds");

    assert_eq!(patterns, vec!["\u{fffd}".to_string()]);
}

#[test]
fn load_filter_file_patterns_reads_from_stdin() {
    super::set_filter_stdin_input(b"keep\n# comment\n\ninclude\n".to_vec());
    let patterns = super::load_filter_file_patterns(Path::new("-")).expect("load stdin patterns");

    assert_eq!(patterns, vec!["keep".to_string(), "include".to_string()]);
}

#[test]
fn apply_merge_directive_resolves_relative_paths() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let outer = temp.path().join("outer.rules");
    let subdir = temp.path().join("nested");
    std::fs::create_dir(&subdir).expect("create nested dir");
    let child = subdir.join("child.rules");
    let grand = subdir.join("grand.rules");

    std::fs::write(&outer, b"+ outer\nmerge nested/child.rules\n").expect("write outer");
    std::fs::write(&child, b"+ child\nmerge grand.rules\n").expect("write child");
    std::fs::write(&grand, b"+ grand\n").expect("write grand");

    let mut rules = Vec::new();
    let mut visited = HashSet::new();
    let directive = MergeDirective::new(OsString::from("outer.rules"), None)
        .with_options(DirMergeOptions::default().allow_list_clearing(true));
    super::apply_merge_directive(directive, temp.path(), &mut rules, &mut visited)
        .expect("merge succeeds");

    assert!(visited.is_empty());
    let patterns: Vec<_> = rules
        .iter()
        .map(|rule| rule.pattern().to_string())
        .collect();
    assert_eq!(patterns, vec!["outer", "child", "grand"]);
}

#[test]
fn apply_merge_directive_respects_forced_include() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("filters.rules");
    std::fs::write(&path, b"alpha\n!\nbeta\n").expect("write filters");

    let mut rules = vec![FilterRuleSpec::exclude("existing".to_string())];
    let mut visited = HashSet::new();
    let directive = MergeDirective::new(path.into_os_string(), Some(FilterRuleKind::Include))
        .with_options(
            DirMergeOptions::default()
                .with_enforced_kind(Some(DirMergeEnforcedKind::Include))
                .allow_list_clearing(true),
        );
    super::apply_merge_directive(directive, temp.path(), &mut rules, &mut visited)
        .expect("merge succeeds");

    assert!(visited.is_empty());
    let patterns: Vec<_> = rules
        .iter()
        .map(|rule| rule.pattern().to_string())
        .collect();
    assert_eq!(patterns, vec!["beta"]);
}

#[test]
fn merge_directive_options_inherit_parent_configuration() {
    let base = DirMergeOptions::default()
        .inherit(false)
        .exclude_filter_file(true)
        .allow_list_clearing(false)
        .anchor_root(true)
        .allow_comments(false)
        .with_side_overrides(Some(true), Some(false));

    let directive = MergeDirective::new(OsString::from("nested.rules"), None);
    let merged = super::merge_directive_options(&base, &directive);

    assert!(!merged.inherit_rules());
    assert!(merged.excludes_self());
    assert!(!merged.list_clear_allowed());
    assert!(merged.anchor_root_enabled());
    assert!(!merged.allows_comments());
    assert_eq!(merged.sender_side_override(), Some(true));
    assert_eq!(merged.receiver_side_override(), Some(false));
}

#[test]
fn merge_directive_options_respect_child_overrides() {
    let base = DirMergeOptions::default()
        .inherit(false)
        .with_side_overrides(Some(true), Some(false));

    let child_options = DirMergeOptions::default()
        .inherit(true)
        .allow_list_clearing(true)
        .with_enforced_kind(Some(DirMergeEnforcedKind::Include))
        .use_whitespace()
        .with_side_overrides(Some(false), Some(true));
    let directive =
        MergeDirective::new(OsString::from("nested.rules"), None).with_options(child_options);

    let merged = super::merge_directive_options(&base, &directive);

    assert_eq!(merged.enforced_kind(), Some(DirMergeEnforcedKind::Include));
    assert!(merged.uses_whitespace());
    assert_eq!(merged.sender_side_override(), Some(false));
    assert_eq!(merged.receiver_side_override(), Some(true));
}

#[test]
fn process_merge_directive_applies_parent_overrides_to_nested_merges() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let nested = temp.path().join("nested.rules");
    std::fs::write(&nested, b"+ file\n").expect("write nested");

    let options = DirMergeOptions::default()
        .sender_modifier()
        .inherit(false)
        .exclude_filter_file(true)
        .allow_list_clearing(false);

    let mut rules = Vec::new();
    let mut visited = HashSet::new();
    super::process_merge_directive(
        "merge nested.rules",
        &options,
        temp.path(),
        "parent.rules",
        &mut rules,
        &mut visited,
    )
    .expect("merge succeeds");

    assert!(visited.is_empty());
    let include_rule = rules
        .iter()
        .find(|rule| rule.pattern() == "file")
        .expect("include rule present");
    assert!(include_rule.applies_to_sender());
    assert!(!include_rule.applies_to_receiver());
    assert!(rules.iter().any(|rule| rule.pattern() == "nested.rules"));
}

#[test]
fn transfer_request_with_no_times_overrides_archive() {
    use filetime::{FileTime, set_file_times};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-no-times.txt");
    let destination = tmp.path().join("dest-no-times.txt");
    std::fs::write(&source, b"data").expect("write source");
    let mtime = FileTime::from_unix_time(1_700_100_000, 0);
    set_file_times(&source, mtime, mtime).expect("set times");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--no-times"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let metadata = std::fs::metadata(&destination).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_ne!(dest_mtime, mtime);
}

#[test]
fn transfer_request_with_omit_dir_times_skips_directory_timestamp() {
    use filetime::{FileTime, set_file_times};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    let source_dir = source_root.join("nested");
    let source_file = source_dir.join("file.txt");

    std::fs::create_dir_all(&source_dir).expect("create source dir");
    std::fs::write(&source_file, b"payload").expect("write file");

    let dir_mtime = FileTime::from_unix_time(1_700_200_000, 0);
    set_file_times(&source_dir, dir_mtime, dir_mtime).expect("set dir times");
    set_file_times(&source_file, dir_mtime, dir_mtime).expect("set file times");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--omit-dir-times"),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let dest_dir = dest_root.join("nested");
    let dest_file = dest_dir.join("file.txt");

    let dir_metadata = std::fs::metadata(&dest_dir).expect("dest dir metadata");
    let file_metadata = std::fs::metadata(&dest_file).expect("dest file metadata");
    let dest_dir_mtime = FileTime::from_last_modification_time(&dir_metadata);
    let dest_file_mtime = FileTime::from_last_modification_time(&file_metadata);

    assert_ne!(dest_dir_mtime, dir_mtime);
    assert_eq!(dest_file_mtime, dir_mtime);
}

#[cfg(unix)]
#[test]
fn transfer_request_with_omit_link_times_skips_symlink_timestamp() {
    use filetime::{FileTime, set_file_times, set_symlink_file_times};
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source dir");

    let source_target = source_root.join("target.txt");
    let source_link = source_root.join("link.txt");
    fs::write(&source_target, b"payload").expect("write source target");
    symlink("target.txt", &source_link).expect("create symlink");

    let timestamp = FileTime::from_unix_time(1_700_300_000, 0);
    set_file_times(&source_target, timestamp, timestamp).expect("set file times");
    set_symlink_file_times(&source_link, timestamp, timestamp).expect("set symlink times");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--omit-link-times"),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let dest_target = dest_root.join("target.txt");
    let dest_link = dest_root.join("link.txt");

    let dest_target_metadata = fs::metadata(&dest_target).expect("dest target metadata");
    let dest_link_metadata = fs::symlink_metadata(&dest_link).expect("dest link metadata");
    let dest_target_mtime = FileTime::from_last_modification_time(&dest_target_metadata);
    let dest_link_mtime = FileTime::from_last_modification_time(&dest_link_metadata);

    assert_eq!(dest_target_mtime, timestamp);
    assert_ne!(dest_link_mtime, timestamp);
}

#[test]
fn checksum_with_no_times_preserves_existing_destination() {
    use filetime::{FileTime, set_file_mtime};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-checksum.txt");
    let destination = tmp.path().join("dest-checksum.txt");
    std::fs::write(&source, b"payload").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-times"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let preserved = FileTime::from_unix_time(1_700_200_000, 0);
    set_file_mtime(&destination, preserved).expect("set destination mtime");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--checksum"),
        OsString::from("--no-times"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let metadata = std::fs::metadata(&destination).expect("dest metadata");
    let final_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(final_mtime, preserved);
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"payload"
    );
}

#[test]
fn transfer_request_with_exclude_from_stdin_skips_patterns() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    std::fs::create_dir_all(&source_root).expect("create source root");
    std::fs::create_dir_all(&dest_root).expect("create dest root");
    std::fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    std::fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    super::set_filter_stdin_input(b"*.tmp\n".to_vec());
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--exclude-from"),
        OsString::from("-"),
        source_root.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("keep.txt").exists());
    assert!(!copied_root.join("skip.tmp").exists());
}

#[test]
fn bwlimit_invalid_value_reports_error() {
    let (code, stdout, stderr) =
        run_with_args([OsString::from(RSYNC), OsString::from("--bwlimit=oops")]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("--bwlimit=oops is invalid"));
    assert_contains_client_trailer(&rendered);
}

#[test]
fn bwlimit_rejects_small_fractional_values() {
    let (code, stdout, stderr) =
        run_with_args([OsString::from(RSYNC), OsString::from("--bwlimit=0.4")]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("--bwlimit=0.4 is too small (min: 512 or 0 for unlimited)",));
}

#[test]
fn bwlimit_accepts_decimal_suffixes() {
    let limit = parse_bandwidth_limit(OsStr::new("1.5M"))
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.bytes_per_second().get(), 1_572_864);
}

#[test]
fn bwlimit_accepts_decimal_base_specifier() {
    let limit = parse_bandwidth_limit(OsStr::new("10KB"))
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.bytes_per_second().get(), 10_000);
}

#[test]
fn bwlimit_accepts_burst_component() {
    let limit = parse_bandwidth_limit(OsStr::new("4M:32K"))
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.bytes_per_second().get(), 4_194_304);
    assert_eq!(
        limit.burst_bytes().map(std::num::NonZeroU64::get),
        Some(32 * 1024)
    );
}

#[test]
fn bwlimit_zero_disables_limit() {
    let limit = parse_bandwidth_limit(OsStr::new("0")).expect("parse succeeds");
    assert!(limit.is_none());
}

#[test]
fn bwlimit_rejects_whitespace_wrapped_argument() {
    let error = parse_bandwidth_limit(OsStr::new(" 1M \t"))
        .expect_err("whitespace-wrapped bwlimit should fail");
    let rendered = format!("{error}");
    assert!(rendered.contains("--bwlimit= 1M \t is invalid"));
}

#[test]
fn bwlimit_accepts_leading_plus_sign() {
    let limit = parse_bandwidth_limit(OsStr::new("+2M"))
        .expect("parse succeeds")
        .expect("limit available");
    assert_eq!(limit.bytes_per_second().get(), 2_097_152);
}

#[test]
fn bwlimit_rejects_negative_values() {
    let (code, stdout, stderr) =
        run_with_args([OsString::from(RSYNC), OsString::from("--bwlimit=-1")]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("--bwlimit=-1 is invalid"));
}

#[test]
fn compress_level_invalid_value_reports_error() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=fast"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("--compress-level=fast is invalid"));
}

#[test]
fn compress_level_out_of_range_reports_error() {
    let (code, stdout, stderr) =
        run_with_args([OsString::from(RSYNC), OsString::from("--compress-level=12")]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("--compress-level=12 must be between 0 and 9"));
}

#[test]
fn skip_compress_invalid_reports_error() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--skip-compress=mp[]"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("invalid --skip-compress specification"));
    assert!(rendered.contains("empty character class"));
}

#[cfg(windows)]
#[test]
fn operand_detection_ignores_windows_drive_and_device_prefixes() {
    use std::ffi::OsStr;

    assert!(!operand_is_remote(OsStr::new("C:\\temp\\file.txt")));
    assert!(!operand_is_remote(OsStr::new("\\\\?\\C:\\temp\\file.txt")));
    assert!(!operand_is_remote(OsStr::new("\\\\.\\C:\\pipe\\name")));
}

#[test]
fn remote_operand_reports_launch_failure_when_fallback_missing() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let missing = OsString::from("rsync-missing-binary");
    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, missing.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());

    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("failed to launch fallback rsync binary"));
    assert_contains_client_trailer(&rendered);
}

#[test]
fn remote_daemon_listing_prints_modules() {
    let (addr, handle) = spawn_stub_daemon(vec![
        "@RSYNCD: MOTD Welcome to the test daemon\n",
        "@RSYNCD: OK\n",
        "first\tFirst module\n",
        "second\n",
        "@RSYNCD: EXIT\n",
    ]);

    let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("output is UTF-8");
    assert!(rendered.contains("Welcome to the test daemon"));
    assert!(rendered.contains("first\tFirst module"));
    assert!(rendered.contains("second"));

    handle.join().expect("server thread");
}

#[test]
fn remote_daemon_listing_suppresses_motd_with_flag() {
    let (addr, handle) = spawn_stub_daemon(vec![
        "@RSYNCD: MOTD Welcome to the test daemon\n",
        "@RSYNCD: OK\n",
        "module\n",
        "@RSYNCD: EXIT\n",
    ]);

    let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-motd"),
        OsString::from(url),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("output is UTF-8");
    assert!(!rendered.contains("Welcome to the test daemon"));
    assert!(rendered.contains("module"));

    handle.join().expect("server thread");
}

#[test]
fn remote_daemon_listing_with_rsync_path_does_not_spawn_fallback() {
    use tempfile::tempdir;

    let (addr, handle) = spawn_stub_daemon(vec!["@RSYNCD: OK\n", "module\n", "@RSYNCD: EXIT\n"]);

    let url = format!("rsync://{}:{}/", addr.ip(), addr.port());

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let marker_path = temp.path().join("marker.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "invoked" > "$MARKER_FILE"
exit 99
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _marker_guard = EnvGuard::set("MARKER_FILE", marker_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--rsync-path=/opt/custom/rsync"),
        OsString::from(url.clone()),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("output is UTF-8");
    assert!(rendered.contains("module"));
    assert!(!marker_path.exists());

    handle.join().expect("server thread");
}

#[test]
fn remote_daemon_listing_respects_protocol_cap() {
    let (addr, handle) = spawn_stub_daemon_with_protocol(
        vec!["@RSYNCD: OK\n", "module\n", "@RSYNCD: EXIT\n"],
        "29.0",
    );

    let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--protocol=29"),
        OsString::from(url),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("output is UTF-8");
    assert!(rendered.contains("module"));

    handle.join().expect("server thread");
}

#[test]
fn remote_daemon_listing_renders_warnings() {
    let (addr, handle) = spawn_stub_daemon(vec![
        "@WARNING: Maintenance\n",
        "@RSYNCD: OK\n",
        "module\n",
        "@RSYNCD: EXIT\n",
    ]);

    let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

    assert_eq!(code, 0);
    assert!(
        String::from_utf8(stdout)
            .expect("modules")
            .contains("module")
    );

    let rendered_err = String::from_utf8(stderr).expect("warnings are UTF-8");
    assert!(rendered_err.contains("@WARNING: Maintenance"));

    handle.join().expect("server thread");
}

#[test]
fn remote_daemon_error_is_reported() {
    let (addr, handle) = spawn_stub_daemon(vec!["@ERROR: unavailable\n", "@RSYNCD: EXIT\n"]);

    let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

    assert_eq!(code, 23);
    assert!(stdout.is_empty());

    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("unavailable"));

    handle.join().expect("server thread");
}

#[test]
fn module_list_username_prefix_is_accepted() {
    let (addr, handle) = spawn_stub_daemon(vec![
        "@RSYNCD: OK\n",
        "module\tWith comment\n",
        "@RSYNCD: EXIT\n",
    ]);

    let url = format!("rsync://user@{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("output is UTF-8");
    assert!(rendered.contains("module\tWith comment"));

    handle.join().expect("server thread");
}

#[test]
fn module_list_uses_connect_program_option() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let temp = tempfile::tempdir().expect("tempdir");
    let script_path = temp.path().join("connect-program.sh");
    let script = r#"#!/bin/sh
set -eu
printf "@RSYNCD: 31.0\n"
read _greeting
printf "@RSYNCD: OK\n"
read _request
printf "example\tvia connect program\n@RSYNCD: EXIT\n"
"#;
    write_executable_script(&script_path, script);

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--connect-program={}", script_path.display())),
        OsString::from("rsync://example/"),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("module listing is UTF-8");
    assert!(rendered.contains("example\tvia connect program"));
}

#[test]
fn module_list_username_prefix_legacy_syntax_is_accepted() {
    let (addr, handle) = spawn_stub_daemon(vec!["@RSYNCD: OK\n", "module\n", "@RSYNCD: EXIT\n"]);

    let url = format!("user@[{}]:{}::", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("output is UTF-8");
    assert!(rendered.contains("module"));

    handle.join().expect("server thread");
}

#[test]
fn module_list_uses_password_file_for_authentication() {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use rsync_checksums::strong::Md5;
    use tempfile::tempdir;

    let challenge = "pw-test";
    let secret = b"cli-secret";
    let expected_digest = {
        let mut hasher = Md5::new();
        hasher.update(secret);
        hasher.update(challenge.as_bytes());
        let digest = hasher.finalize();
        STANDARD_NO_PAD.encode(digest)
    };

    let expected_credentials = format!("user {expected_digest}");
    let (addr, handle) = spawn_auth_stub_daemon(
        challenge,
        expected_credentials,
        vec!["@RSYNCD: OK\n", "secure\n", "@RSYNCD: EXIT\n"],
    );

    let temp = tempdir().expect("tempdir");
    let password_path = temp.path().join("daemon.pw");
    std::fs::write(&password_path, b"cli-secret\n").expect("write password");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(&password_path)
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&password_path, permissions).expect("set permissions");
    }

    let url = format!("rsync://user@{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--password-file={}", password_path.display())),
        OsString::from(url),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("module listing is UTF-8");
    assert!(rendered.contains("secure"));

    handle.join().expect("server thread");
}

#[test]
fn module_list_reads_password_from_stdin() {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use rsync_checksums::strong::Md5;

    let challenge = "stdin-test";
    let secret = b"stdin-secret";
    let expected_digest = {
        let mut hasher = Md5::new();
        hasher.update(secret);
        hasher.update(challenge.as_bytes());
        let digest = hasher.finalize();
        STANDARD_NO_PAD.encode(digest)
    };

    let expected_credentials = format!("user {expected_digest}");
    let (addr, handle) = spawn_auth_stub_daemon(
        challenge,
        expected_credentials,
        vec!["@RSYNCD: OK\n", "secure\n", "@RSYNCD: EXIT\n"],
    );

    set_password_stdin_input(b"stdin-secret\n".to_vec());

    let url = format!("rsync://user@{}:{}/", addr.ip(), addr.port());
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--password-file=-"),
        OsString::from(url),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("module listing is UTF-8");
    assert!(rendered.contains("secure"));

    handle.join().expect("server thread");
}

#[cfg(unix)]
#[test]
fn module_list_rejects_world_readable_password_file() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let password_path = temp.path().join("insecure.pw");
    std::fs::write(&password_path, b"secret\n").expect("write password");
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(&password_path)
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&password_path, permissions).expect("set perms");
    }

    let url = String::from("rsync://user@127.0.0.1:873/");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--password-file={}", password_path.display())),
        OsString::from(url),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("password file"));
    assert!(rendered.contains("0600"));

    // No server was contacted: the permission error occurs before negotiation.
}

#[test]
fn password_file_requires_daemon_operands() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let password_path = temp.path().join("local.pw");
    std::fs::write(&password_path, b"secret\n").expect("write password");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(&password_path)
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&password_path, permissions).expect("set permissions");
    }

    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    std::fs::write(&source, b"data").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--password-file={}", password_path.display())),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("--password-file"));
    assert!(rendered.contains("rsync daemon"));
    assert!(!destination.exists());
}

#[test]
fn password_file_dash_conflicts_with_files_from_dash() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--files-from=-"),
        OsString::from("--password-file=-"),
        OsString::from("/tmp/dest"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("--password-file=- cannot be combined with --files-from=-"));
}

#[test]
fn protocol_option_requires_daemon_operands() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    std::fs::write(&source, b"data").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--protocol=30"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("--protocol"));
    assert!(rendered.contains("rsync daemon"));
    assert!(!destination.exists());
}

#[test]
fn invalid_protocol_value_reports_error() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--protocol=27"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("invalid protocol version '27'"));
    assert!(rendered.contains("outside the supported range"));
}

#[test]
fn non_numeric_protocol_value_reports_error() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--protocol=abc"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("invalid protocol version 'abc'"));
    assert!(rendered.contains("unsigned integer"));
}

#[test]
fn protocol_value_with_whitespace_and_plus_is_accepted() {
    let version = parse_protocol_version_arg(OsStr::new(" +31 \n"))
        .expect("whitespace-wrapped value should parse");
    assert_eq!(version.as_u8(), 31);
}

#[test]
fn protocol_value_negative_reports_specific_diagnostic() {
    let message = parse_protocol_version_arg(OsStr::new("-30"))
        .expect_err("negative protocol should be rejected");
    let rendered = message.to_string();
    assert!(rendered.contains("invalid protocol version '-30'"));
    assert!(rendered.contains("cannot be negative"));
}

#[test]
fn protocol_value_empty_reports_specific_diagnostic() {
    let message = parse_protocol_version_arg(OsStr::new("   "))
        .expect_err("empty protocol value should be rejected");
    let rendered = message.to_string();
    assert!(rendered.contains("invalid protocol version '   '"));
    assert!(rendered.contains("must not be empty"));
}

#[test]
fn clap_parse_error_is_reported_via_message() {
    let command = clap_command(rsync_core::version::PROGRAM_NAME);
    let error = command
        .try_get_matches_from(vec!["rsync", "--version=extra"])
        .unwrap_err();

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(
        [OsString::from(RSYNC), OsString::from("--version=extra")],
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(status, 1);
    assert!(stdout.is_empty());

    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains(error.to_string().trim()));
}

#[test]
fn delete_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.delete_mode, DeleteMode::During);
    assert!(!parsed.delete_excluded);
}

#[test]
fn delete_alias_del_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--del"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.delete_mode, DeleteMode::During);
    assert!(!parsed.delete_excluded);
}

#[test]
fn delete_after_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete-after"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.delete_mode, DeleteMode::After);
    assert!(!parsed.delete_excluded);
}

#[test]
fn delete_before_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete-before"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.delete_mode, DeleteMode::Before);
    assert!(!parsed.delete_excluded);
}

#[test]
fn delete_during_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete-during"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.delete_mode, DeleteMode::During);
    assert!(!parsed.delete_excluded);
}

#[test]
fn delete_delay_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete-delay"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert_eq!(parsed.delete_mode, DeleteMode::Delay);
    assert!(!parsed.delete_excluded);
}

#[test]
fn delete_excluded_flag_implies_delete() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--delete-excluded"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert!(parsed.delete_mode.is_enabled());
    assert!(parsed.delete_excluded);
}

#[test]
fn archive_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert!(parsed.archive);
    assert_eq!(parsed.owner, None);
    assert_eq!(parsed.group, None);
}

#[test]
fn long_archive_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--archive"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert!(parsed.archive);
}

#[test]
fn checksum_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--checksum"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert!(parsed.checksum);
}

#[test]
fn size_only_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--size-only"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert!(parsed.size_only);
}

#[test]
fn combined_archive_and_verbose_flags_are_supported() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("combo.txt");
    let destination = tmp.path().join("combo.out");
    std::fs::write(&source, b"combo").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("verbose output is UTF-8");
    assert!(rendered.contains("combo.txt"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"combo"
    );
}

#[test]
fn compress_flag_is_accepted_for_local_copies() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("compress.txt");
    let destination = tmp.path().join("compress.out");
    std::fs::write(&source, b"compressed").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-z"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"compressed"
    );
}

#[test]
fn compress_level_flag_is_accepted_for_local_copies() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("compress.txt");
    let destination = tmp.path().join("compress.out");
    std::fs::write(&source, b"payload").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=6"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"payload"
    );
}

#[test]
fn compress_level_zero_disables_local_compression() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("compress.txt");
    let destination = tmp.path().join("compress.out");
    std::fs::write(&source, b"payload").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=0"),
        OsString::from("-z"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"payload"
    );
}

#[cfg(unix)]
#[test]
fn server_mode_invokes_fallback_binary() {
    use std::fs;
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("server.sh");
    let marker_path = temp.path().join("marker.txt");

    fs::write(
        &script_path,
        r#"#!/bin/sh
set -eu
: "${SERVER_MARKER:?}"
printf 'invoked' > "$SERVER_MARKER"
exit 37
"#,
    )
    .expect("write script");

    let mut perms = fs::metadata(&script_path)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("set script perms");

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _marker_guard = EnvGuard::set("SERVER_MARKER", marker_path.as_os_str());

    let mut stdout = io::sink();
    let mut stderr = io::sink();
    let exit_code = run(
        [
            OsString::from(RSYNC),
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("."),
            OsString::from("dest"),
        ],
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(exit_code, 37);
    assert_eq!(fs::read(&marker_path).expect("read marker"), b"invoked");
}

#[cfg(unix)]
#[test]
fn server_mode_forwards_output_to_provided_handles() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("server_output.sh");

    let script = r#"#!/bin/sh
set -eu
printf 'fallback stdout line\n'
printf 'fallback stderr line\n' >&2
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit_code = run(
        [
            OsString::from(RSYNC),
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("."),
            OsString::from("dest"),
        ],
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(exit_code, 0);
    assert!(stdout.ends_with(b"fallback stdout line\n"));
    assert!(stderr.ends_with(b"fallback stderr line\n"));
}

#[cfg(unix)]
#[test]
fn server_mode_reports_disabled_fallback_override() {
    use std::io;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("no"));

    let mut stdout = io::sink();
    let mut stderr = Vec::new();
    let exit_code = run(
        [
            OsString::from(RSYNC),
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("."),
            OsString::from("dest"),
        ],
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(exit_code, 1);
    let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
    assert!(stderr_text.contains(&format!(
        "remote server mode is unavailable because {env} is disabled",
        env = CLIENT_FALLBACK_ENV,
    )));
}

#[cfg(unix)]
#[test]
fn remote_operands_invoke_fallback_binary() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");
    std::fs::File::create(&args_path).expect("create args file");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
echo fallback-stdout
echo fallback-stderr >&2
exit 7
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--dry-run"),
        OsString::from("remote::module/path"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 7);
    assert_eq!(
        String::from_utf8(stdout).expect("stdout UTF-8"),
        "fallback-stdout\n"
    );
    assert_eq!(
        String::from_utf8(stderr).expect("stderr UTF-8"),
        "fallback-stderr\n"
    );

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--dry-run"));
    assert!(recorded.contains("remote::module/path"));
    assert!(recorded.contains("dest"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_includes_ipv4_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");
    std::fs::File::create(&args_path).expect("create args file");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--ipv4"),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--ipv4"));
    assert!(!recorded.contains("--ipv6"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_includes_ipv6_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");
    std::fs::File::create(&args_path).expect("create args file");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--ipv6"),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--ipv6"));
    assert!(!recorded.contains("--ipv4"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_filter_shortcut() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");
    std::fs::File::create(&args_path).expect("create args file");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-F"),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let occurrences = recorded.lines().filter(|line| *line == "-F").count();
    assert_eq!(occurrences, 1);
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_double_filter_shortcut() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");
    std::fs::File::create(&args_path).expect("create args file");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-FF"),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let occurrences = recorded.lines().filter(|line| *line == "-F").count();
    assert_eq!(occurrences, 2);
}

#[cfg(unix)]
#[test]
fn remote_rsync_url_invokes_fallback_binary() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");
    std::fs::File::create(&args_path).expect("create args file");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("rsync://example.com/module"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--list-only"));
    assert!(recorded.contains("rsync://example.com/module"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_files_from_entries() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let list_path = temp.path().join("file-list.txt");
    std::fs::write(&list_path, b"alpha\nbeta\n").expect("write list");

    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");
    let files_copy_path = temp.path().join("files.bin");
    let dest_path = temp.path().join("dest");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
files_from=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--files-from" ]; then
files_from="$2"
break
  fi
  shift
done
if [ -n "$files_from" ]; then
  cat "$files_from" > "$FILES_COPY"
fi
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());
    let _files_guard = EnvGuard::set("FILES_COPY", files_copy_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let dest_display = dest_path.display().to_string();
    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--files-from"));
    assert!(recorded.contains("remote::module"));
    assert!(recorded.contains(&dest_display));

    let copied = std::fs::read(&files_copy_path).expect("read copied file list");
    assert_eq!(copied, b"alpha\nbeta\n");
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_partial_dir_argument() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let partial_dir = temp.path().join("partials");
    let dest_path = temp.path().join("dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--partial-dir={}", partial_dir.display())),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.lines().any(|line| line == "--partial"));
    assert!(recorded.lines().any(|line| line == "--partial-dir"));
    assert!(
        recorded
            .lines()
            .any(|line| line == partial_dir.display().to_string())
    );

    // Ensure destination operand still forwarded correctly alongside partial dir args.
    assert!(
        recorded
            .lines()
            .any(|line| line == dest_path.display().to_string())
    );
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_temp_dir_argument() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let temp_dir = temp.path().join("temp-stage");
    let dest_path = temp.path().join("dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--temp-dir={}", temp_dir.display())),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.lines().any(|line| line == "--temp-dir"));
    assert!(
        recorded
            .lines()
            .any(|line| line == temp_dir.display().to_string())
    );
    assert!(
        recorded
            .lines()
            .any(|line| line == dest_path.display().to_string())
    );
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_backup_arguments() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--backup"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--backup"));
    assert!(!args.contains(&"--backup-dir"));
    assert!(!args.contains(&"--suffix"));

    std::fs::write(&args_path, b"").expect("truncate args file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--backup-dir"),
        OsString::from("backups"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--backup"));
    assert!(args.contains(&"--backup-dir"));
    assert!(args.contains(&"backups"));

    std::fs::write(&args_path, b"").expect("truncate args file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--suffix"),
        OsString::from(".bak"),
        OsString::from("remote::module"),
        dest_path.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--backup"));
    assert!(args.contains(&"--suffix"));
    assert!(args.contains(&".bak"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_link_dest_arguments() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--link-dest"),
        OsString::from("baseline"),
        OsString::from("--link-dest"),
        OsString::from("/var/cache"),
        OsString::from("--link-dest=link"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(
        recorded
            .lines()
            .filter(|line| *line == "--link-dest")
            .count()
            >= 2
    );
    assert!(recorded.lines().any(|line| line == "baseline"));
    assert!(recorded.lines().any(|line| line == "/var/cache"));
    assert!(recorded.lines().any(|line| line == "--link-dest=link"));
    assert!(recorded.lines().any(|line| line == "--link-dest"));
    assert!(recorded.lines().any(|line| line == "link"));
    assert!(
        recorded
            .lines()
            .any(|line| line == dest_path.display().to_string())
    );
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_reference_destinations() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--link-dest=baseline"),
        OsString::from("--link-dest=/var/cache"),
        OsString::from("--compare-dest=compare"),
        OsString::from("--copy-dest=copy"),
        OsString::from("--link-dest=link"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.lines().any(|line| line == "--link-dest=baseline"));
    assert!(
        recorded
            .lines()
            .any(|line| line == "--link-dest=/var/cache")
    );
    assert!(recorded.lines().any(|line| line == "--compare-dest"));
    assert!(recorded.lines().any(|line| line == "compare"));
    assert!(recorded.lines().any(|line| line == "--copy-dest"));
    assert!(recorded.lines().any(|line| line == "copy"));
    assert!(recorded.lines().any(|line| line == "--link-dest"));
    assert!(recorded.lines().any(|line| line == "link"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_compress_level() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=7"),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--compress"));
    assert!(recorded.contains("--compress-level"));
    assert!(recorded.contains("7"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_skip_compress_arguments() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--skip-compress=gz/mp3"),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(
        recorded
            .lines()
            .any(|line| line == "--skip-compress=gz/mp3")
    );

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--skip-compress="),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.lines().any(|line| line == "--skip-compress="));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_compress_when_level_zero() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=0"),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--no-compress"));
    assert!(recorded.contains("--compress-level"));
    assert!(recorded.contains("0"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_streams_process_output() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");

    let script = r#"#!/bin/sh
echo "fallback stdout"
echo "fallback stderr" 1>&2
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert_eq!(stdout, b"fallback stdout\n");
    assert_eq!(stderr, b"fallback stderr\n");
}

#[cfg(unix)]
#[test]
fn remote_fallback_preserves_from0_entries() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let list_path = temp.path().join("file-list.bin");
    std::fs::write(&list_path, b"first\0second\0").expect("write list");

    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");
    let files_copy_path = temp.path().join("files.bin");
    let dest_path = temp.path().join("dest");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
files_from=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--files-from" ]; then
files_from="$2"
break
  fi
  shift
done
if [ -n "$files_from" ]; then
  cat "$files_from" > "$FILES_COPY"
fi
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());
    let _files_guard = EnvGuard::set("FILES_COPY", files_copy_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--from0"),
        OsString::from(format!("--files-from={}", list_path.display())),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let dest_display = dest_path.display().to_string();
    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--files-from"));
    assert!(recorded.contains("--from0"));
    assert!(recorded.contains(&dest_display));

    let copied = std::fs::read(&files_copy_path).expect("read copied file list");
    assert_eq!(copied, b"first\0second\0");
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_whole_file_flags() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-W"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--whole-file"));
    assert!(!args.contains(&"--no-whole-file"));
    let dest_string = dest_path.display().to_string();
    assert!(args.contains(&"--whole-file"));
    assert!(!args.contains(&"--no-whole-file"));
    assert!(args.contains(&dest_string.as_str()));

    std::fs::write(&args_path, b"").expect("truncate args file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-whole-file"),
        OsString::from("remote::module"),
        dest_path.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--no-whole-file"));
    assert!(!args.contains(&"--whole-file"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_hard_links_flags() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--hard-links"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--hard-links"));
    assert!(!args.contains(&"--no-hard-links"));

    std::fs::write(&args_path, b"").expect("truncate args file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-hard-links"),
        OsString::from("remote::module"),
        dest_path.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--no-hard-links"));
    assert!(!args.contains(&"--hard-links"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_append_flags() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--append"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--append"));
    assert!(!args.contains(&"--append-verify"));
    assert!(!args.contains(&"--no-append"));

    std::fs::write(&args_path, b"").expect("truncate args file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-append"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--no-append"));
    assert!(!args.contains(&"--append"));
    assert!(!args.contains(&"--append-verify"));

    std::fs::write(&args_path, b"").expect("truncate args file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--append-verify"),
        OsString::from("remote::module"),
        dest_path.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--append-verify"));
    assert!(!args.contains(&"--append"));
    assert!(!args.contains(&"--no-append"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_preallocate_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--preallocate"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--preallocate"));

    std::fs::write(&args_path, b"").expect("truncate args file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("remote::module"),
        dest_path.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(!recorded.lines().any(|line| line == "--preallocate"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_sanitises_bwlimit_argument() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");
    std::fs::File::create(&args_path).expect("create args file");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--bwlimit=1M:64K"),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--bwlimit"));
    assert!(!recorded.contains("1M:64K"));

    let mut lines = recorded.lines();
    while let Some(line) = lines.next() {
        if line == "--bwlimit" {
            let value = lines.next().expect("bwlimit value recorded");
            assert_eq!(value, "1048576:65536");
            return;
        }
    }

    panic!("--bwlimit argument not forwarded to fallback");
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_bwlimit_argument() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");
    std::fs::File::create(&args_path).expect("create args file");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-bwlimit"),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let mut lines = recorded.lines();
    while let Some(line) = lines.next() {
        if line == "--bwlimit" {
            let value = lines.next().expect("bwlimit value recorded");
            assert_eq!(value, "0");
            return;
        }
    }

    panic!("--bwlimit argument not forwarded to fallback");
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_safe_links_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--safe-links"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--safe-links"));

    std::fs::write(&args_path, b"").expect("truncate args file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("remote::module"),
        dest_path.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(!recorded.lines().any(|line| line == "--safe-links"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_implied_dirs_flags() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-implied-dirs"),
        OsString::from("remote::module"),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--no-implied-dirs"));
    assert!(!args.contains(&"--implied-dirs"));

    std::fs::write(&args_path, b"").expect("truncate args file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--implied-dirs"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--implied-dirs"));
    assert!(!args.contains(&"--no-implied-dirs"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_prune_empty_dirs_flags() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--prune-empty-dirs"),
        OsString::from("remote::module"),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--prune-empty-dirs"));
    assert!(!args.contains(&"--no-prune-empty-dirs"));

    std::fs::write(&args_path, b"").expect("truncate args file");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-prune-empty-dirs"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--no-prune-empty-dirs"));
    assert!(!args.contains(&"--prune-empty-dirs"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_delete_after_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--delete-after"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--delete"));
    assert!(args.contains(&"--delete-after"));
    assert!(!args.contains(&"--delete-delay"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_delete_before_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--delete-before"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--delete"));
    assert!(args.contains(&"--delete-before"));
    assert!(!args.contains(&"--delete-after"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_delete_during_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--delete-during"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--delete"));
    assert!(args.contains(&"--delete-during"));
    assert!(!args.contains(&"--delete-delay"));
    assert!(!args.contains(&"--delete-before"));
    assert!(!args.contains(&"--delete-after"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_delete_delay_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--delete-delay"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--delete"));
    assert!(args.contains(&"--delete-delay"));
    assert!(!args.contains(&"--delete-before"));
    assert!(!args.contains(&"--delete-after"));
    assert!(!args.contains(&"--delete-during"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_max_delete_limit() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--max-delete=5"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--delete"));
    assert!(args.contains(&"--max-delete=5"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_modify_window() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--modify-window=5"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--modify-window=5"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_min_max_size_arguments() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--min-size=1.5K"),
        OsString::from("--max-size=2M"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--min-size=1.5K"));
    assert!(args.contains(&"--max-size=2M"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_update_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--update"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--update"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_debug_flags() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--debug=io,events"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--debug=io"));
    assert!(args.contains(&"--debug=events"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_protect_args_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--protect-args"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--protect-args"));
    assert!(!args.contains(&"--no-protect-args"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_human_readable_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--human-readable"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--human-readable"));
    assert!(!args.contains(&"--no-human-readable"));
    assert!(!args.contains(&"--human-readable=2"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_human_readable_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-human-readable"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--no-human-readable"));
    assert!(!args.contains(&"--human-readable"));
    assert!(!args.contains(&"--human-readable=2"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_human_readable_level_two() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--human-readable=2"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--human-readable=2"));
    assert!(!args.contains(&"--human-readable"));
    assert!(!args.contains(&"--no-human-readable"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_super_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--super"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--super"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_super_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-super"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--no-super"));
    assert!(!args.contains(&"--super"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_info_flags() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=progress2"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--info=progress2"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_no_protect_args_alias() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-secluded-args"),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--no-protect-args"));
    assert!(!args.contains(&"--protect-args"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_respects_env_protect_args() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());
    let _protect_guard = EnvGuard::set("RSYNC_PROTECT_ARGS", OsStr::new("1"));

    let destination = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("remote::module"),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(args.contains(&"--protect-args"));
    assert!(args.contains(&"--info=progress2"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_respects_zero_compress_level() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=0"),
        OsString::from("remote::module"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    let args: Vec<&str> = recorded.lines().collect();
    assert!(!args.contains(&"--compress"));
    assert!(args.contains(&"--compress-level"));
    assert!(args.contains(&"0"));
    assert!(!args.contains(&"--whole-file"));
    assert!(args.contains(&"--no-whole-file"));
}

#[cfg(unix)]
#[test]
fn local_delta_transfer_executes_locally() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    std::fs::write(&args_path, b"untouched").expect("seed args file");

    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    std::fs::write(&source, b"contents").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-whole-file"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(&destination).expect("read destination"),
        b"contents"
    );

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert_eq!(recorded, "untouched");
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_connection_options() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");
    let password_path = temp.path().join("password.txt");
    std::fs::write(&password_path, b"secret\n").expect("write password");
    let mut permissions = std::fs::metadata(&password_path)
        .expect("password metadata")
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(&password_path, permissions).expect("set password perms");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--password-file={}", password_path.display())),
        OsString::from("--protocol=30"),
        OsString::from("--timeout=120"),
        OsString::from("--contimeout=75"),
        OsString::from("rsync://remote/module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let password_display = password_path.display().to_string();
    let dest_display = dest_path.display().to_string();
    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--password-file"));
    assert!(recorded.contains(&password_display));
    assert!(recorded.contains("--protocol"));
    assert!(recorded.contains("30"));
    assert!(recorded.contains("--timeout"));
    assert!(recorded.contains("120"));
    assert!(recorded.contains("--contimeout"));
    assert!(recorded.contains("75"));
    assert!(recorded.contains("rsync://remote/module"));
    assert!(recorded.contains(&dest_display));
}

#[test]
fn remote_fallback_forwards_rsh_option() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-e"),
        OsString::from("ssh -p 2222"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.lines().any(|line| line == "-e"));
    assert!(recorded.lines().any(|line| line == "ssh -p 2222"));
}

#[test]
fn remote_fallback_forwards_rsync_path_option() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--rsync-path=/opt/custom/rsync"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.lines().any(|line| line == "--rsync-path"));
    assert!(recorded.lines().any(|line| line == "/opt/custom/rsync"));
}

#[test]
fn remote_fallback_forwards_connect_program_option() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--connect-program=nc %H %P"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.lines().any(|line| line == "--connect-program"));
    assert!(recorded.lines().any(|line| line == "nc %H %P"));
}

#[test]
fn remote_fallback_forwards_port_option() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--port=10873"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.lines().any(|line| line == "--port=10873"));
}

#[test]
fn remote_fallback_forwards_remote_option() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--remote-option=--log-file=/tmp/rsync.log"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.lines().any(|line| line == "--remote-option"));
    assert!(
        recorded
            .lines()
            .any(|line| line == "--log-file=/tmp/rsync.log")
    );
}

#[test]
fn remote_fallback_forwards_remote_option_short_flag() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-M"),
        OsString::from("--log-format=%n"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.lines().any(|line| line == "--remote-option"));
    assert!(recorded.lines().any(|line| line == "--log-format=%n"));
}

#[test]
fn remote_fallback_reads_rsync_rsh_env() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _clear_guard = clear_rsync_rsh();
    let _env_guard = EnvGuard::set("RSYNC_RSH", OsStr::new("ssh -p 2200"));
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.lines().any(|line| line == "-e"));
    assert!(recorded.lines().any(|line| line == "ssh -p 2200"));
}

#[test]
fn remote_fallback_cli_rsh_overrides_env() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _clear_guard = clear_rsync_rsh();
    let _env_guard = EnvGuard::set("RSYNC_RSH", OsStr::new("ssh -p 2200"));
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-e"),
        OsString::from("ssh -p 2222"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.lines().any(|line| line == "ssh -p 2222"));
    assert!(!recorded.lines().any(|line| line == "ssh -p 2200"));
}

#[test]
fn rsync_path_requires_remote_operands() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    std::fs::write(&source, b"content").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--rsync-path=/opt/custom/rsync"),
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let message = String::from_utf8(stderr).expect("stderr utf8");
    assert!(message.contains("the --rsync-path option may only be used with remote connections"));
    assert!(!dest.exists());
}

#[test]
fn connect_program_requires_remote_operands() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    std::fs::write(&source, b"content").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--connect-program=/usr/bin/nc %H %P"),
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let message = String::from_utf8(stderr).expect("stderr utf8");
    assert!(
        message.contains(
            "the --connect-program option may only be used when accessing an rsync daemon"
        )
    );
    assert!(!dest.exists());
}

#[test]
fn remote_option_requires_remote_operands() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    std::fs::write(&source, b"content").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--remote-option=--log-file=/tmp/rsync.log"),
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let message = String::from_utf8(stderr).expect("stderr utf8");
    assert!(
        message.contains("the --remote-option option may only be used with remote connections")
    );
    assert!(!dest.exists());
}

#[cfg(all(unix, feature = "acl"))]
#[test]
fn remote_fallback_forwards_acls_toggle() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--acls"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--acls"));
    assert!(!recorded.contains("--no-acls"));
}

#[test]
fn remote_fallback_forwards_omit_dir_times_toggle() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--omit-dir-times"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--omit-dir-times"));
    assert!(!recorded.contains("--no-omit-dir-times"));
}

#[test]
fn remote_fallback_forwards_omit_link_times_toggle() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--omit-link-times"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--omit-link-times"));
    assert!(!recorded.contains("--no-omit-link-times"));
}

#[cfg(all(unix, feature = "acl"))]
#[test]
fn remote_fallback_forwards_no_acls_toggle() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-acls"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--no-acls"));
}

#[test]
fn remote_fallback_forwards_no_omit_dir_times_toggle() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-omit-dir-times"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--no-omit-dir-times"));
}

#[test]
fn remote_fallback_forwards_no_omit_link_times_toggle() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-omit-link-times"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--no-omit-link-times"));
}

#[test]
fn remote_fallback_forwards_one_file_system_toggles() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());

    let dest_path = temp.path().join("dest");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--one-file-system"),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--one-file-system"));

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-one-file-system"),
        OsString::from("remote::module"),
        dest_path.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--no-one-file-system"));
}

#[test]
fn dry_run_flag_skips_destination_mutation() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    fs::write(&source, b"contents").expect("write source");
    let destination = tmp.path().join("dest.txt");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--dry-run"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert!(!destination.exists());
}

#[test]
fn list_only_lists_entries_without_copying() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    fs::create_dir(&source_dir).expect("create src dir");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"contents").expect("write source file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let link_path = source_dir.join("link.txt");
        symlink("file.txt", &link_path).expect("create symlink");
    }
    let destination_dir = tmp.path().join("dest");
    fs::create_dir(&destination_dir).expect("create dest dir");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_dir.clone().into_os_string(),
        destination_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    assert!(rendered.contains("file.txt"));
    #[cfg(unix)]
    {
        assert!(rendered.contains("link.txt -> file.txt"));
    }
    assert!(!destination_dir.join("file.txt").exists());
}

#[test]
fn list_only_formats_directory_without_trailing_slash() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_dir.clone().into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("utf8 stdout");

    let mut directory_line = None;
    for line in rendered.lines() {
        if line.ends_with("src") {
            directory_line = Some(line.to_string());
            break;
        }
    }

    let directory_line = directory_line.expect("directory entry present");
    assert!(directory_line.starts_with('d'));
    assert!(!directory_line.ends_with('/'));
}

#[test]
fn list_only_matches_rsync_format_for_regular_file() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let file_path = source_dir.join("data.bin");
    fs::write(&file_path, vec![0u8; 1_234]).expect("write source file");
    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o644))
        .expect("set file permissions");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&file_path, timestamp, timestamp).expect("set file times");

    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_arg,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let file_line = rendered
        .lines()
        .find(|line| line.ends_with("data.bin"))
        .expect("file entry present");

    let expected_permissions = "-rw-r--r--";
    let expected_size = format_list_size(1_234, HumanReadableMode::Disabled);
    let system_time = SystemTime::UNIX_EPOCH
        + Duration::from_secs(u64::try_from(timestamp.unix_seconds()).expect("positive timestamp"))
        + Duration::from_nanos(u64::from(timestamp.nanoseconds()));
    let expected_timestamp = format_list_timestamp(Some(system_time));
    let expected = format!("{expected_permissions} {expected_size} {expected_timestamp} data.bin");

    assert_eq!(file_line, expected);

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("--human-readable"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let human_line = rendered
        .lines()
        .find(|line| line.ends_with("data.bin"))
        .expect("file entry present");
    let expected_human_size = format_list_size(1_234, HumanReadableMode::Enabled);
    let expected_human =
        format!("{expected_permissions} {expected_human_size} {expected_timestamp} data.bin");
    assert_eq!(human_line, expected_human);
}

#[test]
fn list_only_formats_special_permission_bits_like_rsync() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let sticky_exec = source_dir.join("exec-special");
    let sticky_plain = source_dir.join("plain-special");

    fs::write(&sticky_exec, b"exec").expect("write exec file");
    fs::write(&sticky_plain, b"plain").expect("write plain file");

    fs::set_permissions(&sticky_exec, fs::Permissions::from_mode(0o7777))
        .expect("set permissions with execute bits");
    fs::set_permissions(&sticky_plain, fs::Permissions::from_mode(0o7666))
        .expect("set permissions without execute bits");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&sticky_exec, timestamp, timestamp).expect("set exec times");
    set_file_times(&sticky_plain, timestamp, timestamp).expect("set plain times");

    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_arg,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let system_time = SystemTime::UNIX_EPOCH
        + Duration::from_secs(u64::try_from(timestamp.unix_seconds()).expect("positive timestamp"))
        + Duration::from_nanos(u64::from(timestamp.nanoseconds()));
    let expected_timestamp = format_list_timestamp(Some(system_time));

    let expected_exec = format!(
        "-rwsrwsrwt {} {expected_timestamp} exec-special",
        format_list_size(4, HumanReadableMode::Disabled)
    );
    let expected_plain = format!(
        "-rwSrwSrwT {} {expected_timestamp} plain-special",
        format_list_size(5, HumanReadableMode::Disabled)
    );

    let mut exec_line = None;
    let mut plain_line = None;
    for line in rendered.lines() {
        if line.ends_with("exec-special") {
            exec_line = Some(line.to_string());
        } else if line.ends_with("plain-special") {
            plain_line = Some(line.to_string());
        }
    }

    assert_eq!(exec_line.expect("exec entry"), expected_exec);
    assert_eq!(plain_line.expect("plain entry"), expected_plain);
}

#[test]
fn short_dry_run_flag_skips_destination_mutation() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    fs::write(&source, b"contents").expect("write source");
    let destination = tmp.path().join("dest.txt");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-n"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert!(!destination.exists());
}

#[cfg(unix)]
#[test]
fn verbose_output_includes_symlink_target() {
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    fs::create_dir(&source_dir).expect("create src dir");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"contents").expect("write source file");
    let link_path = source_dir.join("link.txt");
    symlink("file.txt", &link_path).expect("create symlink");

    let destination_dir = tmp.path().join("dest");
    fs::create_dir(&destination_dir).expect("create dest dir");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        source_dir.clone().into_os_string(),
        destination_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    assert!(rendered.contains("link.txt -> file.txt"));
}

#[cfg(unix)]
#[test]
fn out_format_renders_symlink_target_placeholder() {
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    fs::create_dir(&source_dir).expect("create src dir");
    let file = source_dir.join("file.txt");
    fs::write(&file, b"data").expect("write file");
    let link_path = source_dir.join("link.txt");
    symlink("file.txt", &link_path).expect("create symlink");

    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&dest_dir).expect("create dst dir");

    let config = ClientConfig::builder()
        .transfer_args([
            source_dir.as_os_str().to_os_string(),
            dest_dir.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };
    let event = summary
        .events()
        .iter()
        .find(|event| event.relative_path().to_string_lossy().contains("link.txt"))
        .expect("symlink event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%L"))
        .expect("parse %L")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %L");

    assert_eq!(output, b" -> file.txt\n");
}

#[cfg(unix)]
#[test]
fn out_format_renders_combined_name_and_target_placeholder() {
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    fs::create_dir(&source_dir).expect("create src dir");
    let file = source_dir.join("file.txt");
    fs::write(&file, b"data").expect("write file");
    let link_path = source_dir.join("link.txt");
    symlink("file.txt", &link_path).expect("create symlink");

    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&dest_dir).expect("create dst dir");

    let config = ClientConfig::builder()
        .transfer_args([
            source_dir.as_os_str().to_os_string(),
            dest_dir.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };
    let event = summary
        .events()
        .iter()
        .find(|event| event.relative_path().to_string_lossy().contains("link.txt"))
        .expect("symlink event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%N"))
        .expect("parse %N")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %N");

    assert_eq!(output, b"src/link.txt -> file.txt\n");
}

#[test]
fn operands_after_end_of_options_are_preserved() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("-source");
    let destination = tmp.path().join("dest.txt");
    fs::write(&source, b"dash source").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        fs::read(destination).expect("read destination"),
        b"dash source"
    );
}

fn spawn_stub_daemon(
    responses: Vec<&'static str>,
) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    spawn_stub_daemon_with_protocol(responses, "32.0")
}

fn spawn_stub_daemon_with_protocol(
    responses: Vec<&'static str>,
    expected_protocol: &'static str,
) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub daemon");
    let addr = listener.local_addr().expect("local addr");

    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            handle_connection(stream, responses, expected_protocol);
        }
    });

    (addr, handle)
}

fn spawn_auth_stub_daemon(
    challenge: &'static str,
    expected_credentials: String,
    responses: Vec<&'static str>,
) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth daemon");
    let addr = listener.local_addr().expect("local addr");

    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            handle_auth_connection(stream, challenge, &expected_credentials, &responses, "32.0");
        }
    });

    (addr, handle)
}

fn handle_connection(mut stream: TcpStream, responses: Vec<&'static str>, expected_protocol: &str) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");

    stream
        .write_all(LEGACY_DAEMON_GREETING.as_bytes())
        .expect("write greeting");
    stream.flush().expect("flush greeting");

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read client greeting");
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let expected_prefix = ["@RSYNCD: ", expected_protocol].concat();
    assert!(
        trimmed.starts_with(&expected_prefix),
        "client greeting {trimmed:?} did not begin with {expected_prefix:?}"
    );

    line.clear();
    reader.read_line(&mut line).expect("read request");
    assert_eq!(line, "#list\n");

    for response in responses {
        reader
            .get_mut()
            .write_all(response.as_bytes())
            .expect("write response");
    }
    reader.get_mut().flush().expect("flush response");
}

fn handle_auth_connection(
    mut stream: TcpStream,
    challenge: &'static str,
    expected_credentials: &str,
    responses: &[&'static str],
    expected_protocol: &str,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");

    stream
        .write_all(LEGACY_DAEMON_GREETING.as_bytes())
        .expect("write greeting");
    stream.flush().expect("flush greeting");

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read client greeting");
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let expected_prefix = ["@RSYNCD: ", expected_protocol].concat();
    assert!(
        trimmed.starts_with(&expected_prefix),
        "client greeting {trimmed:?} did not begin with {expected_prefix:?}"
    );

    line.clear();
    reader.read_line(&mut line).expect("read request");
    assert_eq!(line, "#list\n");

    reader
        .get_mut()
        .write_all(format!("@RSYNCD: AUTHREQD {challenge}\n").as_bytes())
        .expect("write challenge");
    reader.get_mut().flush().expect("flush challenge");

    line.clear();
    reader.read_line(&mut line).expect("read credentials");
    let received = line.trim_end_matches(['\n', '\r']);
    assert_eq!(received, expected_credentials);

    for response in responses {
        reader
            .get_mut()
            .write_all(response.as_bytes())
            .expect("write response");
    }
    reader.get_mut().flush().expect("flush response");
}

#[cfg(not(feature = "acl"))]
#[test]
fn acls_option_reports_unsupported_when_feature_disabled() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    std::fs::write(&source, b"data").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--acls"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("UTF-8 error");
    assert!(rendered.contains("POSIX ACLs are not supported on this client"));
    assert_contains_client_trailer(&rendered);
}

#[cfg(not(feature = "xattr"))]
#[test]
fn xattrs_option_reports_unsupported_when_feature_disabled() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    std::fs::write(&source, b"data").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--xattrs"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("UTF-8 error");
    assert!(rendered.contains("extended attributes are not supported on this client"));
    assert_contains_client_trailer(&rendered);
}

#[cfg(feature = "xattr")]
#[test]
fn xattrs_option_preserves_attributes() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    std::fs::write(&source, b"attr data").expect("write source");
    xattr::set(&source, "user.test", b"value").expect("set xattr");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--xattrs"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied = xattr::get(&destination, "user.test")
        .expect("read dest xattr")
        .expect("xattr present");
    assert_eq!(copied, b"value");
}

#[test]
fn format_size_combined_includes_exact_component() {
    assert_eq!(
        format_size(1_536, HumanReadableMode::Combined),
        "1.54K (1,536)"
    );
}

#[test]
fn format_progress_rate_zero_bytes_matches_mode() {
    assert_eq!(
        format_progress_rate(0, Duration::from_secs(1), HumanReadableMode::Disabled),
        "0.00kB/s"
    );
    assert_eq!(
        format_progress_rate(0, Duration::from_secs(1), HumanReadableMode::Enabled),
        "0.00B/s"
    );
    assert_eq!(
        format_progress_rate(0, Duration::from_secs(1), HumanReadableMode::Combined),
        "0.00B/s"
    );
}

#[test]
fn format_progress_rate_combined_includes_decimal_component() {
    let rendered = format_progress_rate(
        1_048_576,
        Duration::from_secs(1),
        HumanReadableMode::Combined,
    );
    assert_eq!(rendered, "1.05MB/s (1.00MB/s)");
}
