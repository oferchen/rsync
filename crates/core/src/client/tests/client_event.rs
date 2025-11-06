use super::error::{MAX_DELETE_EXIT_CODE, PROTOCOL_INCOMPATIBLE_EXIT_CODE, map_local_copy_error};
use super::module_list::{
    ConnectProgramConfig, DaemonAuthContext, DaemonAuthDigest, ProxyConfig, SensitiveBytes,
    compute_daemon_auth_response, connect_direct, connect_via_proxy, establish_proxy_tunnel,
    map_daemon_handshake_error, parse_proxy_spec, resolve_connect_timeout,
    resolve_daemon_addresses, set_test_daemon_password,
};
use super::run::build_local_copy_options;
use super::*;
use crate::bandwidth;
use crate::client::fallback::write_daemon_password;
use crate::fallback::CLIENT_FALLBACK_ENV;
use crate::version::RUST_VERSION;
use rsync_compress::zlib::CompressionLevel;
use rsync_engine::{LocalCopyError, SkipCompressList, signature::SignatureAlgorithm};
use rsync_meta::ChmodModifiers;
use rsync_protocol::{NegotiationError, ProtocolVersion};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::num::{NonZeroU8, NonZeroU64};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

const LEGACY_DAEMON_GREETING: &str = "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n";

#[cfg(unix)]
const CAPTURE_PASSWORD_SCRIPT: &str = r#"#!/bin/sh
set -eu
OUTPUT=""
for arg in "$@"; do
  case "$arg" in
CAPTURE=*)
  OUTPUT="${arg#CAPTURE=}"
  ;;
  esac
done
: "${OUTPUT:?}"
cat > "$OUTPUT"
"#;

#[cfg(unix)]
const CAPTURE_ARGS_SCRIPT: &str = r#"#!/bin/sh
set -eu
OUTPUT=""
for arg in "$@"; do
  case "$arg" in
CAPTURE=*)
  OUTPUT="${arg#CAPTURE=}"
  ;;
  esac
done
: "${OUTPUT:?}"
: > "$OUTPUT"
for arg in "$@"; do
  case "$arg" in
CAPTURE=*)
  ;;
*)
  printf '%s\n' "$arg" >> "$OUTPUT"
  ;;
  esac
done
"#;

#[cfg(unix)]
fn capture_password_script() -> String {
    CAPTURE_PASSWORD_SCRIPT.to_string()
}

#[cfg(unix)]
fn capture_args_script() -> String {
    CAPTURE_ARGS_SCRIPT.to_string()
}

#[test]
fn sensitive_bytes_zeroizes_on_drop() {
    let bytes = SensitiveBytes::new(b"topsecret".to_vec());
    let zeroed = bytes.into_zeroized_vec();
    assert!(zeroed.iter().all(|&byte| byte == 0));
}

#[test]
fn daemon_auth_context_zeroizes_secret_on_drop() {
    let context = DaemonAuthContext::new(
        "user".to_string(),
        b"supersecret".to_vec(),
        DaemonAuthDigest::Sha512,
    );
    let zeroed = context.into_zeroized_secret();
    assert!(zeroed.iter().all(|&byte| byte == 0));
}

#[test]
fn resolve_destination_path_returns_existing_candidate() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("dest");
    fs::create_dir_all(&root).expect("create dest root");
    let subdir = root.join("sub");
    fs::create_dir_all(&subdir).expect("create subdir");
    let file_path = subdir.join("file.txt");
    fs::write(&file_path, b"payload").expect("write file");

    let relative = Path::new("sub").join("file.txt");
    let resolved = ClientEvent::resolve_destination_path(root.as_path(), relative.as_path());

    assert_eq!(resolved, file_path);
}

#[test]
fn resolve_destination_path_returns_root_for_file_destination() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("target.bin");
    let relative = Path::new("target.bin");

    let resolved = ClientEvent::resolve_destination_path(root.as_path(), relative);

    assert_eq!(resolved, root);
}

#[test]
fn bandwidth_limit_from_components_returns_none_for_unlimited() {
    let components = bandwidth::BandwidthLimitComponents::unlimited();
    assert!(BandwidthLimit::from_components(components).is_none());
}

#[test]
fn bandwidth_limit_from_components_preserves_rate_and_burst() {
    let rate = NonZeroU64::new(8 * 1024).expect("non-zero");
    let burst = NonZeroU64::new(64 * 1024).expect("non-zero");
    let components = bandwidth::BandwidthLimitComponents::new(Some(rate), Some(burst));
    let limit = BandwidthLimit::from_components(components).expect("limit produced");

    assert_eq!(limit.bytes_per_second(), rate);
    assert_eq!(limit.burst_bytes(), Some(burst));
    assert!(limit.burst_specified());
}

#[test]
fn bandwidth_limit_components_conversion_preserves_configuration() {
    let rate = NonZeroU64::new(12 * 1024).expect("non-zero");
    let burst = NonZeroU64::new(256 * 1024).expect("non-zero");
    let limit = BandwidthLimit::from_rate_and_burst(rate, Some(burst));

    let components = limit.components();
    assert_eq!(components.rate(), Some(rate));
    assert_eq!(components.burst(), Some(burst));
    assert!(components.burst_specified());

    let round_trip = BandwidthLimit::from_components(components).expect("limit produced");
    assert_eq!(round_trip, limit);
}

#[test]
fn bandwidth_limit_into_components_supports_from_trait() {
    let rate = NonZeroU64::new(4 * 1024).expect("non-zero");
    let burst = NonZeroU64::new(32 * 1024).expect("non-zero");
    let limit = BandwidthLimit::from_rate_and_burst(rate, Some(burst));

    let via_ref: bandwidth::BandwidthLimitComponents = (&limit).into();
    let via_value: bandwidth::BandwidthLimitComponents = limit.into();

    assert_eq!(via_ref.rate(), Some(rate));
    assert_eq!(via_ref.burst(), Some(burst));
    assert_eq!(via_value.rate(), Some(rate));
    assert_eq!(via_value.burst(), Some(burst));
}

#[test]
fn resolve_destination_path_preserves_missing_entries() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("dest");
    fs::create_dir_all(&root).expect("create destination root");
    let relative = Path::new("missing.bin");

    let resolved = ClientEvent::resolve_destination_path(root.as_path(), relative);

    assert_eq!(resolved, root.join(relative));
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
const FALLBACK_SCRIPT: &str = r#"#!/bin/sh
set -eu

while [ "$#" -gt 0 ]; do
  case "$1" in
--files-from)
  FILE="$2"
  cat "$FILE"
  shift 2
  ;;
--from0)
  shift
  ;;
*)
  shift
  ;;
  esac
done

printf 'fallback stdout\n'
printf 'fallback stderr\n' >&2
exit 42
"#;

#[cfg(unix)]
const SIGNAL_EXIT_SCRIPT: &str = r#"#!/bin/sh
set -eu

kill -TERM "$$"
while :; do
  sleep 1
done
"#;

static ENV_GUARD: OnceLock<Mutex<()>> = OnceLock::new();

fn env_lock() -> &'static Mutex<()> {
    ENV_GUARD.get_or_init(|| Mutex::new(()))
}

#[cfg(unix)]
fn write_fallback_script(dir: &Path) -> PathBuf {
    let path = dir.join("fallback.sh");
    fs::write(&path, FALLBACK_SCRIPT).expect("script written");
    let metadata = fs::metadata(&path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("script permissions set");
    path
}

#[cfg(unix)]
fn write_signal_script(dir: &Path) -> PathBuf {
    let path = dir.join("signal.sh");
    fs::write(&path, SIGNAL_EXIT_SCRIPT).expect("script written");
    let metadata = fs::metadata(&path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("script permissions set");
    path
}

fn baseline_fallback_args() -> RemoteFallbackArgs {
    RemoteFallbackArgs {
        dry_run: false,
        list_only: false,
        remote_shell: None,
        remote_options: Vec::new(),
        connect_program: None,
        port: None,
        bind_address: None,
        protect_args: None,
        human_readable: None,
        address_mode: AddressMode::Default,
        archive: false,
        delete: false,
        delete_mode: DeleteMode::Disabled,
        delete_excluded: false,
        max_delete: None,
        min_size: None,
        max_size: None,
        block_size: None,
        checksum: false,
        checksum_choice: None,
        checksum_seed: None,
        size_only: false,
        ignore_existing: false,
        ignore_missing_args: false,
        update: false,
        modify_window: None,
        compress: false,
        compress_disabled: false,
        compress_level: None,
        compress_choice: None,
        skip_compress: None,
        stop_after: None,
        stop_at: None,
        chown: None,
        owner: None,
        group: None,
        chmod: Vec::new(),
        perms: None,
        super_mode: None,
        times: None,
        omit_dir_times: None,
        omit_link_times: None,
        numeric_ids: None,
        hard_links: None,
        copy_links: None,
        copy_dirlinks: false,
        copy_unsafe_links: None,
        keep_dirlinks: None,
        safe_links: false,
        sparse: None,
        devices: None,
        specials: None,
        relative: None,
        one_file_system: None,
        implied_dirs: None,
        mkpath: false,
        prune_empty_dirs: None,
        verbosity: 0,
        progress: false,
        stats: false,
        itemize_changes: false,
        partial: false,
        preallocate: false,
        delay_updates: false,
        partial_dir: None,
        temp_directory: None,
        backup: false,
        backup_dir: None,
        backup_suffix: None,
        link_dests: Vec::new(),
        remove_source_files: false,
        append: None,
        append_verify: false,
        inplace: None,
        msgs_to_stderr: false,
        whole_file: None,
        bwlimit: None,
        excludes: Vec::new(),
        includes: Vec::new(),
        exclude_from: Vec::new(),
        include_from: Vec::new(),
        filters: Vec::new(),
        rsync_filter_shortcuts: 0,
        compare_destinations: Vec::new(),
        copy_destinations: Vec::new(),
        link_destinations: Vec::new(),
        cvs_exclude: false,
        info_flags: Vec::new(),
        debug_flags: Vec::new(),
        files_from_used: false,
        file_list_entries: Vec::new(),
        from0: false,
        password_file: None,
        daemon_password: None,
        protocol: None,
        timeout: TransferTimeout::Default,
        connect_timeout: TransferTimeout::Default,
        out_format: None,
        log_file: None,
        log_file_format: None,
        no_motd: false,
        fallback_binary: None,
        rsync_path: None,
        remainder: Vec::new(),
        write_batch: None,
        only_write_batch: None,
        read_batch: None,
        #[cfg(feature = "acl")]
        acls: None,
        #[cfg(feature = "xattr")]
        xattrs: None,
    }
}

#[cfg(unix)]
struct FailingWriter;

#[cfg(unix)]
impl Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("forced failure"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("forced failure"))
    }
}

#[test]
fn builder_collects_transfer_arguments() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("source"), OsString::from("dest")])
        .build();

    assert_eq!(
        config.transfer_args(),
        &[OsString::from("source"), OsString::from("dest")]
    );
    assert!(config.has_transfer_request());
    assert!(!config.dry_run());
}

#[test]
fn builder_records_bind_address() {
    let bind = BindAddress::new(
        OsString::from("127.0.0.1"),
        "127.0.0.1:0".parse().expect("socket"),
    );
    let config = ClientConfig::builder()
        .bind_address(Some(bind.clone()))
        .build();

    let recorded = config.bind_address().expect("bind address present");
    assert_eq!(recorded.raw(), bind.raw());
    assert_eq!(recorded.socket(), bind.socket());
}

#[test]
fn builder_append_round_trip() {
    let enabled = ClientConfig::builder().append(true).build();
    assert!(enabled.append());
    assert!(!enabled.append_verify());

    let disabled = ClientConfig::builder().append(false).build();
    assert!(!disabled.append());
    assert!(!disabled.append_verify());
}

#[test]
fn builder_safe_links_round_trip() {
    let enabled = ClientConfig::builder().safe_links(true).build();
    assert!(enabled.safe_links());

    let disabled = ClientConfig::builder().safe_links(false).build();
    assert!(!disabled.safe_links());

    let default_config = ClientConfig::builder().build();
    assert!(!default_config.safe_links());
}

#[test]
fn builder_append_verify_implies_append() {
    let verified = ClientConfig::builder().append_verify(true).build();
    assert!(verified.append());
    assert!(verified.append_verify());

    let cleared = ClientConfig::builder()
        .append(true)
        .append_verify(true)
        .append_verify(false)
        .build();
    assert!(cleared.append());
    assert!(!cleared.append_verify());
}

#[test]
fn builder_backup_round_trip() {
    let enabled = ClientConfig::builder().backup(true).build();
    assert!(enabled.backup());

    let disabled = ClientConfig::builder().build();
    assert!(!disabled.backup());
}

#[test]
fn builder_backup_directory_implies_backup() {
    let config = ClientConfig::builder()
        .backup_directory(Some(PathBuf::from("backups")))
        .build();

    assert!(config.backup());
    assert_eq!(
        config.backup_directory(),
        Some(std::path::Path::new("backups"))
    );

    let cleared = ClientConfig::builder()
        .backup_directory(None::<PathBuf>)
        .build();
    assert!(!cleared.backup());
    assert!(cleared.backup_directory().is_none());
}

#[test]
fn builder_backup_suffix_implies_backup() {
    let config = ClientConfig::builder()
        .backup_suffix(Some(OsString::from(".bak")))
        .build();

    assert!(config.backup());
    assert_eq!(config.backup_suffix(), Some(OsStr::new(".bak")));

    let cleared = ClientConfig::builder()
        .backup(true)
        .backup_suffix(None::<OsString>)
        .build();
    assert!(cleared.backup());
    assert_eq!(cleared.backup_suffix(), None);
}

#[test]
fn builder_enables_dry_run() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .dry_run(true)
        .build();

    assert!(config.dry_run());
}

#[test]
fn builder_enables_list_only() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .list_only(true)
        .build();

    assert!(config.list_only());
}

#[test]
fn builder_sets_compression_setting() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .compression_setting(CompressionSetting::level(CompressionLevel::Best))
        .build();

    assert_eq!(
        config.compression_setting(),
        CompressionSetting::level(CompressionLevel::Best)
    );
}

#[test]
fn builder_defaults_disable_compression() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!config.compress());
    assert!(config.compression_setting().is_disabled());
}

