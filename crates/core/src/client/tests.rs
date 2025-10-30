use super::module_list::{
    ConnectProgramConfig, DaemonAuthContext, ProxyConfig, SensitiveBytes,
    compute_daemon_auth_response, connect_direct, connect_via_proxy, establish_proxy_tunnel,
    map_daemon_handshake_error, parse_proxy_spec, resolve_connect_timeout,
    resolve_daemon_addresses, set_test_daemon_password,
};
use super::*;
use crate::bandwidth;
use crate::client::fallback::write_daemon_password;
use crate::fallback::CLIENT_FALLBACK_ENV;
use crate::version::RUST_VERSION;
use rsync_compress::zlib::CompressionLevel;
use rsync_engine::{SkipCompressList, signature::SignatureAlgorithm};
use rsync_meta::ChmodModifiers;
use rsync_protocol::{NegotiationError, ProtocolVersion};
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
    let context = DaemonAuthContext::new("user".to_string(), b"supersecret".to_vec());
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
        skip_compress: None,
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
        no_motd: false,
        fallback_binary: None,
        rsync_path: None,
        remainder: Vec::new(),
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

#[test]
fn builder_enabling_compress_sets_default_level() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .compress(true)
        .build();

    assert!(config.compress());
    assert!(config.compression_setting().is_enabled());
    assert_eq!(
        config.compression_setting().level_or_default(),
        CompressionLevel::Default
    );
}

#[test]
fn builder_sets_min_file_size_limit() {
    let config = ClientConfig::builder().min_file_size(Some(1_024)).build();

    assert_eq!(config.min_file_size(), Some(1_024));

    let cleared = ClientConfig::builder()
        .min_file_size(Some(2048))
        .min_file_size(None)
        .build();

    assert_eq!(cleared.min_file_size(), None);
}

#[test]
fn builder_sets_max_file_size_limit() {
    let config = ClientConfig::builder().max_file_size(Some(8_192)).build();

    assert_eq!(config.max_file_size(), Some(8_192));

    let cleared = ClientConfig::builder()
        .max_file_size(Some(4_096))
        .max_file_size(None)
        .build();

    assert_eq!(cleared.max_file_size(), None);
}

#[test]
fn builder_disabling_compress_clears_override() {
    let level = NonZeroU8::new(5).unwrap();
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .compression_level(Some(CompressionLevel::precise(level)))
        .compress(false)
        .build();

    assert!(!config.compress());
    assert!(config.compression_setting().is_disabled());
    assert_eq!(config.compression_level(), None);
}

#[test]
fn builder_enables_delete() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete(true)
        .build();

    assert!(config.delete());
    assert_eq!(config.delete_mode(), DeleteMode::During);
}

#[test]
fn builder_enables_delete_after() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete_after(true)
        .build();

    assert!(config.delete());
    assert!(config.delete_after());
    assert_eq!(config.delete_mode(), DeleteMode::After);
}

#[test]
fn builder_enables_delete_delay() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete_delay(true)
        .build();

    assert!(config.delete());
    assert!(config.delete_delay());
    assert_eq!(config.delete_mode(), DeleteMode::Delay);
}

#[test]
fn builder_enables_delete_before() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete_before(true)
        .build();

    assert!(config.delete());
    assert!(config.delete_before());
    assert_eq!(config.delete_mode(), DeleteMode::Before);
}

#[test]
fn builder_enables_delete_excluded() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete_excluded(true)
        .build();

    assert!(config.delete_excluded());
    assert!(!ClientConfig::default().delete_excluded());
}

#[test]
fn builder_sets_max_delete_limit() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .max_delete(Some(4))
        .build();

    assert_eq!(config.max_delete(), Some(4));
    assert_eq!(ClientConfig::default().max_delete(), None);
}

#[test]
fn builder_enables_checksum() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .checksum(true)
        .build();

    assert!(config.checksum());
}

#[test]
fn builder_applies_checksum_seed_to_signature_algorithm() {
    let choice = StrongChecksumChoice::parse("xxh64").expect("checksum choice parsed");
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .checksum_choice(choice)
        .checksum_seed(Some(7))
        .build();

    assert_eq!(config.checksum_seed(), Some(7));
    match config.checksum_signature_algorithm() {
        SignatureAlgorithm::Xxh64 { seed } => assert_eq!(seed, 7),
        other => panic!("unexpected signature algorithm: {other:?}"),
    }
}

#[test]
fn builder_sets_bandwidth_limit() {
    let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(4096).unwrap());
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .bandwidth_limit(Some(limit))
        .build();

    assert_eq!(config.bandwidth_limit(), Some(limit));
}

#[test]
fn bandwidth_limit_to_limiter_preserves_rate_and_burst() {
    let rate = NonZeroU64::new(8 * 1024 * 1024).expect("non-zero rate");
    let burst = NonZeroU64::new(256 * 1024).expect("non-zero burst");
    let limit = BandwidthLimit::from_rate_and_burst(rate, Some(burst));

    let limiter = limit.to_limiter();

    assert_eq!(limiter.limit_bytes(), rate);
    assert_eq!(limiter.burst_bytes(), Some(burst));
}

#[test]
fn bandwidth_limit_into_limiter_transfers_configuration() {
    let rate = NonZeroU64::new(4 * 1024 * 1024).expect("non-zero rate");
    let limit = BandwidthLimit::from_rate_and_burst(rate, None);

    let limiter = limit.into_limiter();

    assert_eq!(limiter.limit_bytes(), rate);
    assert_eq!(limiter.burst_bytes(), None);
}

#[test]
fn bandwidth_limit_fallback_argument_returns_bytes_per_second() {
    let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(2048).unwrap());
    assert_eq!(limit.fallback_argument(), OsString::from("2048"));
}

#[test]
fn bandwidth_limit_fallback_argument_includes_burst_when_specified() {
    let rate = NonZeroU64::new(8 * 1024).unwrap();
    let burst = NonZeroU64::new(32 * 1024).unwrap();
    let limit = BandwidthLimit::from_rate_and_burst(rate, Some(burst));

    assert_eq!(limit.fallback_argument(), OsString::from("8192:32768"));
}

#[test]
fn bandwidth_limit_fallback_argument_preserves_explicit_zero_burst() {
    let rate = NonZeroU64::new(4 * 1024).unwrap();
    let components =
        bandwidth::BandwidthLimitComponents::new_with_specified(Some(rate), None, true);

    let limit = BandwidthLimit::from_components(components).expect("limit produced");

    assert!(limit.burst_specified());
    assert_eq!(limit.burst_bytes(), None);
    assert_eq!(limit.fallback_argument(), OsString::from("4096:0"));

    let round_trip = limit.components();
    assert!(round_trip.burst_specified());
    assert_eq!(round_trip.burst(), None);
}

#[test]
fn bandwidth_limit_fallback_unlimited_argument_returns_zero() {
    assert_eq!(
        BandwidthLimit::fallback_unlimited_argument(),
        OsString::from("0"),
    );
}

#[test]
fn builder_sets_compression_level() {
    let level = NonZeroU8::new(7).unwrap();
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .compress(true)
        .compression_level(Some(CompressionLevel::precise(level)))
        .build();

    assert!(config.compress());
    assert_eq!(
        config.compression_level(),
        Some(CompressionLevel::precise(level))
    );
    assert_eq!(ClientConfig::default().compression_level(), None);
}

#[test]
fn builder_enables_update() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .update(true)
        .build();

    assert!(config.update());
    assert!(!ClientConfig::default().update());
}

#[test]
fn builder_sets_timeout() {
    let timeout = TransferTimeout::Seconds(NonZeroU64::new(30).unwrap());
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .timeout(timeout)
        .build();

    assert_eq!(config.timeout(), timeout);
    assert_eq!(ClientConfig::default().timeout(), TransferTimeout::Default);
}

#[test]
fn builder_sets_connect_timeout() {
    let timeout = TransferTimeout::Seconds(NonZeroU64::new(12).unwrap());
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .connect_timeout(timeout)
        .build();

    assert_eq!(config.connect_timeout(), timeout);
    assert_eq!(
        ClientConfig::default().connect_timeout(),
        TransferTimeout::Default
    );
}

#[test]
fn builder_records_modify_window() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .modify_window(Some(5))
        .build();

    assert_eq!(config.modify_window(), Some(5));
    assert_eq!(config.modify_window_duration(), Duration::from_secs(5));
    assert_eq!(
        ClientConfig::default().modify_window_duration(),
        Duration::ZERO
    );
}

#[test]
fn builder_collects_reference_directories() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .compare_destination(PathBuf::from("compare"))
        .copy_destination(PathBuf::from("copy"))
        .link_destination(PathBuf::from("link"))
        .build();

    let references = config.reference_directories();
    assert_eq!(references.len(), 3);
    assert_eq!(references[0].kind(), ReferenceDirectoryKind::Compare);
    assert_eq!(references[1].kind(), ReferenceDirectoryKind::Copy);
    assert_eq!(references[2].kind(), ReferenceDirectoryKind::Link);
    assert_eq!(references[0].path(), PathBuf::from("compare").as_path());
}

#[test]
fn local_copy_options_apply_explicit_timeout() {
    let timeout = TransferTimeout::Seconds(NonZeroU64::new(5).unwrap());
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .timeout(timeout)
        .build();

    let options = build_local_copy_options(&config, None);
    assert_eq!(options.timeout(), Some(Duration::from_secs(5)));
}

#[test]
fn local_copy_options_apply_modify_window() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .modify_window(Some(3))
        .build();

    let options = build_local_copy_options(&config, None);
    assert_eq!(options.modify_window(), Duration::from_secs(3));

    let default_config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();
    assert_eq!(
        build_local_copy_options(&default_config, None).modify_window(),
        Duration::ZERO
    );
}

#[test]
fn local_copy_options_omit_timeout_when_unset() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    let options = build_local_copy_options(&config, None);
    assert!(options.timeout().is_none());
}

#[test]
fn local_copy_options_delay_updates_enable_partial_transfers() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delay_updates(true)
        .build();

    let enabled_options = build_local_copy_options(&enabled, None);
    assert!(enabled_options.delay_updates_enabled());
    assert!(enabled_options.partial_enabled());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    let disabled_options = build_local_copy_options(&disabled, None);
    assert!(!disabled_options.delay_updates_enabled());
    assert!(!disabled_options.partial_enabled());
}

#[test]
fn local_copy_options_honour_temp_directory_setting() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .temp_directory(Some(PathBuf::from(".staging")))
        .build();

    let options = build_local_copy_options(&config, None);
    assert_eq!(options.temp_directory_path(), Some(Path::new(".staging")));

    let default_config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(
        build_local_copy_options(&default_config, None)
            .temp_directory_path()
            .is_none()
    );
}

#[test]
fn resolve_connect_timeout_prefers_explicit_setting() {
    let explicit = TransferTimeout::Seconds(NonZeroU64::new(5).unwrap());
    let resolved =
        resolve_connect_timeout(explicit, TransferTimeout::Default, Duration::from_secs(30));
    assert_eq!(resolved, Some(Duration::from_secs(5)));
}

#[test]
fn resolve_connect_timeout_uses_transfer_timeout_when_default() {
    let transfer = TransferTimeout::Seconds(NonZeroU64::new(8).unwrap());
    let resolved =
        resolve_connect_timeout(TransferTimeout::Default, transfer, Duration::from_secs(30));
    assert_eq!(resolved, Some(Duration::from_secs(8)));
}

#[test]
fn resolve_connect_timeout_disables_when_requested() {
    let resolved = resolve_connect_timeout(
        TransferTimeout::Disabled,
        TransferTimeout::Seconds(NonZeroU64::new(9).unwrap()),
        Duration::from_secs(30),
    );
    assert!(resolved.is_none());

    let resolved_default = resolve_connect_timeout(
        TransferTimeout::Default,
        TransferTimeout::Disabled,
        Duration::from_secs(30),
    );
    assert!(resolved_default.is_none());
}

#[test]
fn connect_direct_applies_io_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let daemon_addr = DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("daemon addr");

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1];
            let _ = stream.read(&mut buf);
        }
    });

    let timeout = Some(Duration::from_secs(4));
    let mut stream = connect_direct(
        &daemon_addr,
        Some(Duration::from_secs(10)),
        timeout,
        AddressMode::Default,
        None,
    )
    .expect("connect directly");

    assert_eq!(stream.read_timeout().expect("read timeout"), timeout);
    assert_eq!(stream.write_timeout().expect("write timeout"), timeout);

    // Wake the accept loop and close cleanly.
    let _ = stream.write_all(&[0]);
    handle.join().expect("listener thread");
}

#[test]
fn connect_via_proxy_applies_io_timeout() {
    let proxy_listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
    let proxy_addr = proxy_listener.local_addr().expect("proxy addr");
    let proxy = ProxyConfig {
        host: proxy_addr.ip().to_string(),
        port: proxy_addr.port(),
        credentials: None,
    };

    let target = DaemonAddress::new(String::from("daemon.example"), 873).expect("daemon addr");

    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = proxy_listener.accept() {
            let mut reader = BufReader::new(stream);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).expect("read request") == 0 {
                    return;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
            }

            let mut stream = reader.into_inner();
            stream
                .write_all(b"HTTP/1.0 200 Connection established\r\n\r\n")
                .expect("respond to connect");
            let _ = stream.flush();
        }
    });

    let timeout = Some(Duration::from_secs(6));
    let stream = connect_via_proxy(&target, &proxy, Some(Duration::from_secs(9)), timeout, None)
        .expect("proxy connect");

    assert_eq!(stream.read_timeout().expect("read timeout"), timeout);
    assert_eq!(stream.write_timeout().expect("write timeout"), timeout);

    drop(stream);
    handle.join().expect("proxy thread");
}

#[test]
fn builder_sets_numeric_ids() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .numeric_ids(true)
        .build();

    assert!(config.numeric_ids());

    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!config.numeric_ids());
}

#[test]
fn builder_preserves_owner_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .owner(true)
        .build();

    assert!(config.preserve_owner());
    assert!(!config.preserve_group());
}

#[test]
fn builder_preserves_group_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .group(true)
        .build();

    assert!(config.preserve_group());
    assert!(!config.preserve_owner());
}

#[test]
fn builder_preserves_permissions_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .permissions(true)
        .build();

    assert!(config.preserve_permissions());
    assert!(!config.preserve_times());
}

#[test]
fn builder_preserves_times_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .times(true)
        .build();

    assert!(config.preserve_times());
    assert!(!config.preserve_permissions());
}

#[test]
fn builder_sets_chmod_modifiers() {
    let modifiers = ChmodModifiers::parse("a+rw").expect("chmod parses");
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .chmod(Some(modifiers.clone()))
        .build();

    assert_eq!(config.chmod(), Some(&modifiers));
}

#[test]
fn builder_preserves_devices_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .devices(true)
        .build();

    assert!(config.preserve_devices());
    assert!(!config.preserve_specials());
}

#[test]
fn builder_preserves_specials_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .specials(true)
        .build();

    assert!(config.preserve_specials());
    assert!(!config.preserve_devices());
}

#[test]
fn builder_preserves_hard_links_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .hard_links(true)
        .build();

    assert!(config.preserve_hard_links());
    assert!(!ClientConfig::default().preserve_hard_links());
}

#[cfg(feature = "acl")]
#[test]
fn builder_preserves_acls_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .acls(true)
        .build();

    assert!(config.preserve_acls());
    assert!(!ClientConfig::default().preserve_acls());
}

#[cfg(feature = "xattr")]
#[test]
fn builder_preserves_xattrs_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .xattrs(true)
        .build();

    assert!(config.preserve_xattrs());
    assert!(!ClientConfig::default().preserve_xattrs());
}

#[test]
fn builder_preserves_remove_source_files_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .remove_source_files(true)
        .build();

    assert!(config.remove_source_files());
    assert!(!ClientConfig::default().remove_source_files());
}

#[test]
fn builder_controls_omit_dir_times_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .omit_dir_times(true)
        .build();

    assert!(config.omit_dir_times());
    assert!(!ClientConfig::default().omit_dir_times());
}

#[test]
fn builder_controls_omit_link_times_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .omit_link_times(true)
        .build();

    assert!(config.omit_link_times());
    assert!(!ClientConfig::default().omit_link_times());
}

#[test]
fn builder_enables_sparse() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .sparse(true)
        .build();

    assert!(config.sparse());
}

#[test]
fn builder_enables_size_only() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .size_only(true)
        .build();

    assert!(config.size_only());
    assert!(!ClientConfig::default().size_only());
}

#[test]
fn builder_configures_implied_dirs_flag() {
    let default_config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(default_config.implied_dirs());
    assert!(ClientConfig::default().implied_dirs());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .implied_dirs(false)
        .build();

    assert!(!disabled.implied_dirs());

    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .implied_dirs(true)
        .build();

    assert!(enabled.implied_dirs());
}

#[test]
fn builder_sets_mkpath_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .mkpath(true)
        .build();

    assert!(config.mkpath());
    assert!(!ClientConfig::default().mkpath());
}

#[test]
fn builder_sets_inplace() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .inplace(true)
        .build();

    assert!(config.inplace());

    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!config.inplace());
}

#[test]
fn builder_sets_delay_updates() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delay_updates(true)
        .build();

    assert!(config.delay_updates());

    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!config.delay_updates());
}

#[test]
fn builder_sets_copy_dirlinks() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .copy_dirlinks(true)
        .build();

    assert!(enabled.copy_dirlinks());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!disabled.copy_dirlinks());
}

#[test]
fn builder_sets_copy_unsafe_links() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .copy_unsafe_links(true)
        .build();

    assert!(enabled.copy_unsafe_links());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!disabled.copy_unsafe_links());
}

#[test]
fn builder_sets_keep_dirlinks() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .keep_dirlinks(true)
        .build();

    assert!(enabled.keep_dirlinks());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!disabled.keep_dirlinks());
}

#[test]
fn builder_enables_stats() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .stats(true)
        .build();

    assert!(enabled.stats());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!disabled.stats());
}

#[cfg(unix)]
#[test]
fn remote_fallback_invocation_forwards_streams() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let script = write_fallback_script(temp.path());

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut args = baseline_fallback_args();
    args.files_from_used = true;
    args.file_list_entries = vec![OsString::from("alpha"), OsString::from("beta")];
    args.fallback_binary = Some(script.into_os_string());

    let exit_code = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");

    assert_eq!(exit_code, 42);
    assert_eq!(
        String::from_utf8(stdout).expect("stdout utf8"),
        "alpha\nbeta\nfallback stdout\n"
    );
    assert_eq!(
        String::from_utf8(stderr).expect("stderr utf8"),
        "fallback stderr\n"
    );
}

#[cfg(unix)]
#[test]
fn remote_fallback_writes_password_to_stdin() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("password.txt");
    let script_path = temp.path().join("capture-password.sh");
    let script = capture_password_script();
    fs::write(&script_path, script).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.password_file = Some(PathBuf::from("-"));
    args.daemon_password = Some(b"topsecret".to_vec());
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");

    assert_eq!(exit, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let captured = fs::read(&capture_path).expect("captured password");
    assert_eq!(captured, b"topsecret\n");
}

#[test]
fn map_local_copy_error_reports_delete_limit() {
    let mapped = map_local_copy_error(LocalCopyError::delete_limit_exceeded(2));
    assert_eq!(mapped.exit_code(), MAX_DELETE_EXIT_CODE);
    let rendered = mapped.message().to_string();
    assert!(
        rendered.contains("Deletions stopped due to --max-delete limit (2 entries skipped)"),
        "unexpected diagnostic: {rendered}"
    );
}

#[test]
fn write_daemon_password_appends_newline_and_zeroizes_buffer() {
    let mut output = Vec::new();
    let mut secret = b"swordfish".to_vec();

    write_daemon_password(&mut output, &mut secret).expect("write succeeds");

    assert_eq!(output, b"swordfish\n");
    assert!(secret.iter().all(|&byte| byte == 0));
}

#[test]
fn write_daemon_password_handles_existing_newline() {
    let mut output = Vec::new();
    let mut secret = b"hunter2\n".to_vec();

    write_daemon_password(&mut output, &mut secret).expect("write succeeds");

    assert_eq!(output, b"hunter2\n");
    assert!(secret.iter().all(|&byte| byte == 0));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_copy_links_toggle() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.copy_links = Some(true);
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--copy-links"));

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.copy_links = Some(false);
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--no-copy-links"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_copy_unsafe_links_toggle() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.copy_unsafe_links = Some(true);
    args.safe_links = true;
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--copy-unsafe-links"));

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.copy_unsafe_links = Some(false);
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(
        captured
            .lines()
            .any(|line| line == "--no-copy-unsafe-links")
    );
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_checksum_choice() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.checksum = true;
    args.checksum_choice = Some(OsString::from("xxh128"));
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");

    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--checksum"));
    assert!(
        captured
            .lines()
            .any(|line| line == "--checksum-choice=xxh128")
    );
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_checksum_seed() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.checksum = true;
    args.checksum_seed = Some(123);
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");

    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--checksum"));
    assert!(captured.lines().any(|line| line == "--checksum-seed=123"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_copy_dirlinks_flag() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.copy_dirlinks = true;
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--copy-dirlinks"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_ignore_missing_args_flag() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.ignore_missing_args = true;
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--ignore-missing-args"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_one_file_system_toggle() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.one_file_system = Some(true);
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--one-file-system"));

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.one_file_system = Some(false);
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--no-one-file-system"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_backup_arguments() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.backup = true;
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--backup"));
    assert!(!captured.lines().any(|line| line == "--backup-dir"));
    assert!(!captured.lines().any(|line| line == "--suffix"));

    fs::write(&capture_path, b"").expect("truncate capture");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.backup = true;
    args.backup_dir = Some(PathBuf::from("backups"));
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--backup"));
    assert!(captured.lines().any(|line| line == "--backup-dir"));
    assert!(captured.lines().any(|line| line == "backups"));

    fs::write(&capture_path, b"").expect("truncate capture");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.backup = true;
    args.backup_suffix = Some(OsString::from(".bak"));
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--backup"));
    assert!(captured.lines().any(|line| line == "--suffix"));
    assert!(captured.lines().any(|line| line == ".bak"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_keep_dirlinks_flags() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.keep_dirlinks = Some(true);
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--keep-dirlinks"));

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.keep_dirlinks = Some(false);
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--no-keep-dirlinks"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_safe_links_flag() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.safe_links = true;
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--safe-links"));

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.safe_links = false;
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(!captured.lines().any(|line| line == "--safe-links"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_chmod_arguments() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.chmod = vec![OsString::from("Du+rwx"), OsString::from("Fgo-w")];
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");

    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--chmod=Du+rwx"));
    assert!(captured.lines().any(|line| line == "--chmod=Fgo-w"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_reference_directory_flags() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.compare_destinations = vec![OsString::from("compare-one"), OsString::from("compare-two")];
    args.copy_destinations = vec![OsString::from("copy-one")];
    args.link_destinations = vec![OsString::from("link-one"), OsString::from("link-two")];
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");

    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    let lines: Vec<&str> = captured.lines().collect();
    let expected_pairs = [
        ("--compare-dest", "compare-one"),
        ("--compare-dest", "compare-two"),
        ("--copy-dest", "copy-one"),
        ("--link-dest", "link-one"),
        ("--link-dest", "link-two"),
    ];

    for (flag, path) in expected_pairs {
        assert!(
            lines
                .windows(2)
                .any(|window| window[0] == flag && window[1] == path),
            "missing pair {flag} {path} in {:?}",
            lines
        );
    }
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_cvs_exclude_flag() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.into_os_string());
    args.cvs_exclude = true;
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--cvs-exclude"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_mkpath_flag() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.mkpath = true;
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--mkpath"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_partial_dir_argument() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.partial = true;
    args.partial_dir = Some(PathBuf::from(".rsync-partial"));
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--partial"));
    assert!(captured.lines().any(|line| line == "--partial-dir"));
    assert!(captured.lines().any(|line| line == ".rsync-partial"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_delay_updates_flag() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.delay_updates = true;
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--delay-updates"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_itemize_changes_flag() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    const ITEMIZE_FORMAT: &str = "%i %n%L";

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.itemize_changes = true;
    args.out_format = Some(OsString::from(ITEMIZE_FORMAT));
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");

    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--itemize-changes"));
    assert!(captured.lines().any(|line| line == "--out-format"));
    assert!(captured.lines().any(|line| line == ITEMIZE_FORMAT));
}

#[cfg(unix)]
#[test]
fn run_client_or_fallback_uses_fallback_for_remote_operands() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let script = write_fallback_script(temp.path());

    let config = ClientConfig::builder()
        .transfer_args([OsString::from("remote::module"), OsString::from("/tmp/dst")])
        .build();

    let mut args = baseline_fallback_args();
    args.remainder = vec![OsString::from("remote::module"), OsString::from("/tmp/dst")];
    args.fallback_binary = Some(script.into_os_string());

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let context = RemoteFallbackContext::new(&mut stdout, &mut stderr, args);

    let outcome =
        run_client_or_fallback(config, None, Some(context)).expect("fallback invocation succeeds");

    match outcome {
        ClientOutcome::Fallback(summary) => {
            assert_eq!(summary.exit_code(), 42);
        }
        ClientOutcome::Local(_) => panic!("expected fallback outcome"),
    }

    assert_eq!(
        String::from_utf8(stdout).expect("stdout utf8"),
        "fallback stdout\n"
    );
    assert_eq!(
        String::from_utf8(stderr).expect("stderr utf8"),
        "fallback stderr\n"
    );
}

#[cfg(unix)]
#[test]
fn run_client_or_fallback_handles_delta_mode_locally() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let script = write_fallback_script(temp.path());

    let source_path = temp.path().join("source.txt");
    let dest_path = temp.path().join("dest.txt");
    fs::write(&source_path, b"delta-test").expect("source created");

    let source = OsString::from(source_path.as_os_str());
    let dest = OsString::from(dest_path.as_os_str());

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), dest.clone()])
        .whole_file(false)
        .build();

    let mut args = baseline_fallback_args();
    args.remainder = vec![source, dest];
    args.whole_file = Some(false);
    args.fallback_binary = Some(script.into_os_string());

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let context = RemoteFallbackContext::new(&mut stdout, &mut stderr, args);

    let outcome =
        run_client_or_fallback(config, None, Some(context)).expect("local delta copy succeeds");

    match outcome {
        ClientOutcome::Local(summary) => {
            assert_eq!(summary.files_copied(), 1);
        }
        ClientOutcome::Fallback(_) => panic!("unexpected fallback execution"),
    }

    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(fs::read(dest_path).expect("dest contents"), b"delta-test");
}

#[test]
fn remote_fallback_reports_launch_errors() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let missing = temp.path().join("missing-rsync");

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(missing.into_os_string());

    let error = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect_err("spawn failure reported");

    assert_eq!(error.exit_code(), 1);
    let message = format!("{error}");
    assert!(message.contains("failed to launch fallback rsync binary"));
}

#[cfg(unix)]
#[test]
fn remote_fallback_reports_stdout_forward_errors() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let script = write_fallback_script(temp.path());

    let mut stdout = FailingWriter;
    let mut stderr = Vec::new();

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script.into_os_string());

    let error = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect_err("stdout forwarding failure surfaces");

    assert_eq!(error.exit_code(), 1);
    let message = format!("{error}");
    assert!(message.contains("failed to forward fallback stdout"));
}

#[test]
fn remote_fallback_reports_disabled_override() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set_os(CLIENT_FALLBACK_ENV, OsStr::new("no"));
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let args = baseline_fallback_args();
    let error = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect_err("disabled override prevents fallback execution");

    assert_eq!(error.exit_code(), 1);
    let message = format!("{error}");
    assert!(message.contains(&format!(
        "remote transfers are unavailable because {env} is disabled",
        env = CLIENT_FALLBACK_ENV,
    )));
}

#[test]
fn builder_forces_event_collection() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .force_event_collection(true)
        .build();

    assert!(config.force_event_collection());
    assert!(config.collect_events());
}

#[test]
fn run_client_reports_missing_operands() {
    let config = ClientConfig::builder().build();
    let error = run_client(config).expect_err("missing operands should error");

    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("missing source operands"));
    assert!(
        rendered.contains(&format!("[client={}]", RUST_VERSION)),
        "expected missing operands error to include client trailer"
    );
}

#[test]
fn run_client_handles_delta_transfer_mode_locally() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.bin");
    let destination = tmp.path().join("dest.bin");
    fs::write(&source, b"payload").expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ])
        .whole_file(false)
        .build();

    let summary = run_client(config).expect("delta mode executes locally");

    assert_eq!(fs::read(&destination).expect("read dest"), b"payload");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), b"payload".len() as u64);
}

#[test]
fn run_client_copies_single_file() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    fs::write(&source, b"example").expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .permissions(true)
        .times(true)
        .build();

    assert!(config.preserve_permissions());
    assert!(config.preserve_times());

    let summary = run_client(config).expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), b"example");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), b"example".len() as u64);
    assert!(!summary.compression_used());
    assert!(summary.compressed_bytes().is_none());
}

#[test]
fn run_client_with_compress_records_compressed_bytes() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.bin");
    let destination = tmp.path().join("dest.bin");
    let payload = vec![b'Z'; 32 * 1024];
    fs::write(&source, &payload).expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .compress(true)
        .build();

    let summary = run_client(config).expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), payload);
    assert!(summary.compression_used());
    let compressed = summary
        .compressed_bytes()
        .expect("compressed bytes recorded");
    assert!(compressed > 0);
    assert!(compressed <= summary.bytes_copied());
}

#[test]
fn run_client_skip_compress_disables_compression_for_matching_suffix() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("archive.gz");
    let destination = tmp.path().join("dest.gz");
    let payload = vec![b'X'; 16 * 1024];
    fs::write(&source, &payload).expect("write source");

    let skip = SkipCompressList::parse("gz").expect("parse list");
    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .compress(true)
        .skip_compress(skip)
        .build();

    let summary = run_client(config).expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), payload);
    assert!(!summary.compression_used());
    assert!(summary.compressed_bytes().is_none());
}

#[test]
fn skip_compress_from_env_parses_list() {
    let _guard = EnvGuard::set("RSYNC_SKIP_COMPRESS", "gz,zip");
    let list = skip_compress_from_env("RSYNC_SKIP_COMPRESS")
        .expect("parse env list")
        .expect("list present");

    assert!(list.matches_path(Path::new("file.gz")));
    assert!(list.matches_path(Path::new("archive.zip")));
    assert!(!list.matches_path(Path::new("note.txt")));
}

#[test]
fn skip_compress_from_env_absent_returns_none() {
    let _guard = EnvGuard::remove("RSYNC_SKIP_COMPRESS");
    assert!(
        skip_compress_from_env("RSYNC_SKIP_COMPRESS")
            .expect("absent env")
            .is_none()
    );
}

#[test]
fn skip_compress_from_env_reports_invalid_specification() {
    let _guard = EnvGuard::set("RSYNC_SKIP_COMPRESS", "[");
    let error = skip_compress_from_env("RSYNC_SKIP_COMPRESS")
        .expect_err("invalid specification should error");
    let rendered = error.to_string();
    assert!(rendered.contains("RSYNC_SKIP_COMPRESS"));
    assert!(rendered.contains("invalid"));
}

#[cfg(unix)]
#[test]
fn skip_compress_from_env_rejects_non_utf8_values() {
    use std::os::unix::ffi::OsStrExt;

    let bytes = OsStr::from_bytes(&[0xFF]);
    let _guard = EnvGuard::set_os("RSYNC_SKIP_COMPRESS", bytes);
    let error =
        skip_compress_from_env("RSYNC_SKIP_COMPRESS").expect_err("non UTF-8 value should error");
    assert!(
        error
            .to_string()
            .contains("RSYNC_SKIP_COMPRESS accepts only UTF-8")
    );
}

#[test]
fn run_client_remove_source_files_deletes_source() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    fs::write(&source, b"move me").expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .remove_source_files(true)
        .build();

    let summary = run_client(config).expect("copy succeeds");

    assert_eq!(summary.sources_removed(), 1);
    assert!(!source.exists(), "source should be removed after transfer");
    assert_eq!(fs::read(&destination).expect("read dest"), b"move me");
}

#[test]
fn run_client_remove_source_files_preserves_matched_source() {
    use filetime::{FileTime, set_file_times};

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    let payload = b"stable";
    fs::write(&source, payload).expect("write source");
    fs::write(&destination, payload).expect("write destination");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&source, timestamp, timestamp).expect("set source times");
    set_file_times(&destination, timestamp, timestamp).expect("set dest times");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .remove_source_files(true)
        .times(true)
        .build();

    let summary = run_client(config).expect("transfer succeeds");

    assert_eq!(summary.sources_removed(), 0, "unchanged sources remain");
    assert!(source.exists(), "matched source should not be removed");
    assert_eq!(fs::read(&destination).expect("read dest"), payload);
}

#[test]
fn run_client_dry_run_skips_copy() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    fs::write(&source, b"dry-run").expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .dry_run(true)
        .build();

    let summary = run_client(config).expect("dry-run succeeds");

    assert!(!destination.exists());
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn run_client_delete_removes_extraneous_entries() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

    let dest_root = tmp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_operand, dest_root.clone().into_os_string()])
        .delete(true)
        .build();

    let summary = run_client(config).expect("copy succeeds");

    assert_eq!(
        fs::read(dest_root.join("keep.txt")).expect("read keep"),
        b"fresh"
    );
    assert!(!dest_root.join("extra.txt").exists());
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn run_client_delete_respects_dry_run() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

    let dest_root = tmp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_operand, dest_root.clone().into_os_string()])
        .dry_run(true)
        .delete(true)
        .build();

    let summary = run_client(config).expect("dry-run succeeds");

    assert_eq!(
        fs::read(dest_root.join("keep.txt")).expect("read keep"),
        b"stale"
    );
    assert!(dest_root.join("extra.txt").exists());
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn run_client_update_skips_newer_destination() {
    use filetime::{FileTime, set_file_times};

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-update.txt");
    let destination = tmp.path().join("dest-update.txt");
    fs::write(&source, b"fresh").expect("write source");
    fs::write(&destination, b"existing").expect("write destination");

    let older = FileTime::from_unix_time(1_700_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, older, older).expect("set source times");
    set_file_times(&destination, newer, newer).expect("set dest times");

    let summary = run_client(
        ClientConfig::builder()
            .transfer_args([
                source.clone().into_os_string(),
                destination.clone().into_os_string(),
            ])
            .update(true)
            .build(),
    )
    .expect("run client");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    assert_eq!(
        fs::read(destination).expect("read destination"),
        b"existing"
    );
}

#[test]
fn run_client_respects_filter_rules() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let config = ClientConfig::builder()
        .transfer_args([source_root.clone(), dest_root.clone()])
        .extend_filter_rules([FilterRuleSpec::exclude("*.tmp".to_string())])
        .build();

    let summary = run_client(config).expect("copy succeeds");

    assert!(dest_root.join("source").join("keep.txt").exists());
    assert!(!dest_root.join("source").join("skip.tmp").exists());
    assert!(summary.files_copied() >= 1);
}

#[test]
fn run_client_filter_clear_resets_previous_rules() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

    let config = ClientConfig::builder()
        .transfer_args([source_root.clone(), dest_root.clone()])
        .extend_filter_rules([
            FilterRuleSpec::exclude("*.tmp".to_string()),
            FilterRuleSpec::clear(),
            FilterRuleSpec::exclude("keep.txt".to_string()),
        ])
        .build();

    let summary = run_client(config).expect("copy succeeds");

    let copied_root = dest_root.join("source");
    assert!(copied_root.join("skip.tmp").exists());
    assert!(!copied_root.join("keep.txt").exists());
    assert!(summary.files_copied() >= 1);
}

#[test]
fn run_client_copies_directory_tree() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let nested = source_root.join("nested");
    let source_file = nested.join("file.txt");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(&source_file, b"tree").expect("write source file");

    let dest_root = tmp.path().join("destination");

    let config = ClientConfig::builder()
        .transfer_args([source_root.clone(), dest_root.clone()])
        .build();

    let summary = run_client(config).expect("directory copy succeeds");

    let copied_file = dest_root.join("nested").join("file.txt");
    assert_eq!(fs::read(copied_file).expect("read copied"), b"tree");
    assert!(summary.files_copied() >= 1);
    assert!(summary.directories_created() >= 1);
}

#[cfg(unix)]
#[test]
fn run_client_copies_symbolic_link() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().expect("tempdir");
    let target_file = tmp.path().join("target.txt");
    fs::write(&target_file, b"symlink target").expect("write target");

    let source_link = tmp.path().join("source-link");
    symlink(&target_file, &source_link).expect("create source symlink");

    let destination_link = tmp.path().join("dest-link");
    let config = ClientConfig::builder()
        .transfer_args([source_link.clone(), destination_link.clone()])
        .force_event_collection(true)
        .build();

    let summary = run_client(config).expect("link copy succeeds");

    let copied = fs::read_link(destination_link).expect("read copied link");
    assert_eq!(copied, target_file);
    assert_eq!(summary.symlinks_copied(), 1);

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::SymlinkCopied))
        .expect("symlink event present");
    let recorded_target = event
        .metadata()
        .and_then(ClientEntryMetadata::symlink_target)
        .expect("symlink target recorded");
    assert_eq!(recorded_target, target_file.as_path());
}

#[cfg(unix)]
#[test]
fn run_client_preserves_symbolic_links_in_directories() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");

    let target_file = tmp.path().join("target.txt");
    fs::write(&target_file, b"data").expect("write target");
    let link_path = nested.join("link");
    symlink(&target_file, &link_path).expect("create link");

    let dest_root = tmp.path().join("destination");
    let config = ClientConfig::builder()
        .transfer_args([source_root.clone(), dest_root.clone()])
        .force_event_collection(true)
        .build();

    let summary = run_client(config).expect("directory copy succeeds");

    let copied_link = dest_root.join("nested").join("link");
    let copied_target = fs::read_link(copied_link).expect("read copied link");
    assert_eq!(copied_target, target_file);
    assert_eq!(summary.symlinks_copied(), 1);

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::SymlinkCopied))
        .expect("symlink event present");
    let recorded_target = event
        .metadata()
        .and_then(ClientEntryMetadata::symlink_target)
        .expect("symlink target recorded");
    assert_eq!(recorded_target, target_file.as_path());
}

#[cfg(unix)]
#[test]
fn run_client_preserves_file_metadata() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-metadata.txt");
    let destination = tmp.path().join("dest-metadata.txt");
    fs::write(&source, b"metadata").expect("write source");

    let mode = 0o640;
    fs::set_permissions(&source, PermissionsExt::from_mode(mode)).expect("set source permissions");
    let atime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
    let mtime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
    set_file_times(&source, atime, mtime).expect("set source timestamps");

    let source_metadata = fs::metadata(&source).expect("source metadata");
    assert_eq!(source_metadata.permissions().mode() & 0o777, mode);
    let src_atime = FileTime::from_last_access_time(&source_metadata);
    let src_mtime = FileTime::from_last_modification_time(&source_metadata);
    assert_eq!(src_atime, atime);
    assert_eq!(src_mtime, mtime);

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .permissions(true)
        .times(true)
        .build();

    let summary = run_client(config).expect("copy succeeds");

    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.permissions().mode() & 0o777, mode);
    let dest_atime = FileTime::from_last_access_time(&dest_metadata);
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn run_client_preserves_directory_metadata() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source-dir");
    fs::create_dir(&source_dir).expect("create source dir");

    let mode = 0o751;
    fs::set_permissions(&source_dir, PermissionsExt::from_mode(mode))
        .expect("set directory permissions");
    let atime = FileTime::from_unix_time(1_700_010_000, 0);
    let mtime = FileTime::from_unix_time(1_700_020_000, 789_000_000);
    set_file_times(&source_dir, atime, mtime).expect("set directory timestamps");

    let destination_dir = tmp.path().join("dest-dir");
    let config = ClientConfig::builder()
        .transfer_args([source_dir.clone(), destination_dir.clone()])
        .permissions(true)
        .times(true)
        .build();

    assert!(config.preserve_permissions());
    assert!(config.preserve_times());

    let summary = run_client(config).expect("directory copy succeeds");

    let dest_metadata = fs::metadata(&destination_dir).expect("dest metadata");
    assert!(dest_metadata.is_dir());
    assert_eq!(dest_metadata.permissions().mode() & 0o777, mode);
    let dest_atime = FileTime::from_last_access_time(&dest_metadata);
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
    assert!(summary.directories_created() >= 1);
}

#[cfg(unix)]
#[test]
fn run_client_updates_existing_directory_metadata() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source-tree");
    let source_nested = source_dir.join("nested");
    fs::create_dir_all(&source_nested).expect("create source tree");

    let source_mode = 0o745;
    fs::set_permissions(&source_nested, PermissionsExt::from_mode(source_mode))
        .expect("set source nested permissions");
    let source_atime = FileTime::from_unix_time(1_700_030_000, 1_000_000);
    let source_mtime = FileTime::from_unix_time(1_700_040_000, 2_000_000);
    set_file_times(&source_nested, source_atime, source_mtime)
        .expect("set source nested timestamps");

    let dest_root = tmp.path().join("dest-root");
    fs::create_dir(&dest_root).expect("create dest root");
    let dest_dir = dest_root.join("source-tree");
    let dest_nested = dest_dir.join("nested");
    fs::create_dir_all(&dest_nested).expect("pre-create destination tree");

    let dest_mode = 0o711;
    fs::set_permissions(&dest_nested, PermissionsExt::from_mode(dest_mode))
        .expect("set dest nested permissions");
    let dest_atime = FileTime::from_unix_time(1_600_000_000, 0);
    let dest_mtime = FileTime::from_unix_time(1_600_100_000, 0);
    set_file_times(&dest_nested, dest_atime, dest_mtime).expect("set dest nested timestamps");

    let config = ClientConfig::builder()
        .transfer_args([source_dir.clone(), dest_root.clone()])
        .permissions(true)
        .times(true)
        .build();

    assert!(config.preserve_permissions());
    assert!(config.preserve_times());

    let _summary = run_client(config).expect("directory copy succeeds");

    let copied_nested = dest_root.join("source-tree").join("nested");
    let copied_metadata = fs::metadata(&copied_nested).expect("dest metadata");
    assert!(copied_metadata.is_dir());
    assert_eq!(copied_metadata.permissions().mode() & 0o777, source_mode);
    let copied_atime = FileTime::from_last_access_time(&copied_metadata);
    let copied_mtime = FileTime::from_last_modification_time(&copied_metadata);
    assert_eq!(copied_atime, source_atime);
    assert_eq!(copied_mtime, source_mtime);
}

#[cfg(unix)]
#[test]
fn run_client_sparse_copy_creates_holes() {
    use std::os::unix::fs::MetadataExt;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("sparse-source.bin");
    let mut source_file = fs::File::create(&source).expect("create source");
    source_file.write_all(&[0x11]).expect("write leading");
    source_file
        .seek(SeekFrom::Start(1024 * 1024))
        .expect("seek to hole");
    source_file.write_all(&[0x22]).expect("write middle");
    source_file
        .seek(SeekFrom::Start(4 * 1024 * 1024))
        .expect("seek to tail");
    source_file.write_all(&[0x33]).expect("write tail");
    source_file.set_len(6 * 1024 * 1024).expect("extend source");

    let dense_dest = tmp.path().join("dense.bin");
    let sparse_dest = tmp.path().join("sparse.bin");

    let dense_config = ClientConfig::builder()
        .transfer_args([
            source.clone().into_os_string(),
            dense_dest.clone().into_os_string(),
        ])
        .permissions(true)
        .times(true)
        .build();
    let summary = run_client(dense_config).expect("dense copy succeeds");
    assert!(summary.events().is_empty());

    let sparse_config = ClientConfig::builder()
        .transfer_args([
            source.into_os_string(),
            sparse_dest.clone().into_os_string(),
        ])
        .permissions(true)
        .times(true)
        .sparse(true)
        .build();
    let summary = run_client(sparse_config).expect("sparse copy succeeds");
    assert!(summary.events().is_empty());

    let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");

    assert_eq!(dense_meta.len(), sparse_meta.len());
    assert!(sparse_meta.blocks() < dense_meta.blocks());
}

#[test]
fn run_client_merges_directory_contents_when_trailing_separator_present() {
    let tmp = tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    let file_path = nested.join("file.txt");
    fs::write(&file_path, b"contents").expect("write file");

    let dest_root = tmp.path().join("dest");
    let mut source_arg = source_root.clone().into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.clone().into_os_string()])
        .build();

    let summary = run_client(config).expect("directory contents copy succeeds");

    assert!(dest_root.is_dir());
    assert!(dest_root.join("nested").is_dir());
    assert_eq!(
        fs::read(dest_root.join("nested").join("file.txt")).expect("read copied"),
        b"contents"
    );
    assert!(!dest_root.join("source").exists());
    assert!(summary.files_copied() >= 1);
}

#[test]
fn module_list_request_detects_remote_url() {
    let operands = vec![OsString::from("rsync://example.com:8730/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 8730);
}

#[test]
fn module_list_request_accepts_mixed_case_scheme() {
    let operands = vec![OsString::from("RSyNc://Example.COM/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "Example.COM");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_honours_custom_default_port() {
    let operands = vec![OsString::from("rsync://example.com/")];
    let request = ModuleListRequest::from_operands_with_port(&operands, 10_873)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 10_873);
}

#[test]
fn module_list_request_rejects_remote_transfer() {
    let operands = vec![OsString::from("rsync://example.com/module")];
    let request = ModuleListRequest::from_operands(&operands).expect("parse succeeds");
    assert!(request.is_none());
}

#[test]
fn module_list_request_accepts_username_in_rsync_url() {
    let operands = vec![OsString::from("rsync://user@example.com/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 873);
    assert_eq!(request.username(), Some("user"));
}

#[test]
fn module_list_request_accepts_username_in_legacy_syntax() {
    let operands = vec![OsString::from("user@example.com::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 873);
    assert_eq!(request.username(), Some("user"));
}

#[test]
fn module_list_request_supports_ipv6_in_rsync_url() {
    let operands = vec![OsString::from("rsync://[2001:db8::1]/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "2001:db8::1");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_supports_ipv6_in_legacy_syntax() {
    let operands = vec![OsString::from("[fe80::1]::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "fe80::1");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_decodes_percent_encoded_host() {
    let operands = vec![OsString::from("rsync://example%2Ecom/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_supports_ipv6_zone_identifier() {
    let operands = vec![OsString::from("rsync://[fe80::1%25eth0]/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_supports_raw_ipv6_zone_identifier() {
    let operands = vec![OsString::from("[fe80::1%eth0]::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_decodes_percent_encoded_username() {
    let operands = vec![OsString::from("user%2Bname@localhost::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.username(), Some("user+name"));
    assert_eq!(request.address().host(), "localhost");
}

#[test]
fn module_list_request_rejects_truncated_percent_encoding_in_username() {
    let operands = vec![OsString::from("user%2@localhost::")];
    let error =
        ModuleListRequest::from_operands(&operands).expect_err("invalid encoding should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("invalid percent-encoding in daemon username")
    );
}

#[test]
fn module_list_request_defaults_to_localhost_for_shorthand() {
    let operands = vec![OsString::from("::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "localhost");
    assert_eq!(request.address().port(), 873);
    assert!(request.username().is_none());
}

#[test]
fn module_list_request_preserves_username_with_default_host() {
    let operands = vec![OsString::from("user@::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "localhost");
    assert_eq!(request.address().port(), 873);
    assert_eq!(request.username(), Some("user"));
}

#[test]
fn module_list_options_reports_address_mode() {
    let options = ModuleListOptions::default().with_address_mode(AddressMode::Ipv6);
    assert_eq!(options.address_mode(), AddressMode::Ipv6);

    let default_options = ModuleListOptions::default();
    assert_eq!(default_options.address_mode(), AddressMode::Default);
}

#[test]
fn module_list_options_records_bind_address() {
    let socket = "198.51.100.4:0".parse().expect("socket");
    let options = ModuleListOptions::default().with_bind_address(Some(socket));
    assert_eq!(options.bind_address(), Some(socket));

    let default_options = ModuleListOptions::default();
    assert!(default_options.bind_address().is_none());
}

#[test]
fn resolve_daemon_addresses_filters_ipv4_mode() {
    let address = DaemonAddress::new(String::from("127.0.0.1"), 873).expect("address");
    let addresses =
        resolve_daemon_addresses(&address, AddressMode::Ipv4).expect("ipv4 resolution succeeds");

    assert!(!addresses.is_empty());
    assert!(addresses.iter().all(std::net::SocketAddr::is_ipv4));
}

#[test]
fn resolve_daemon_addresses_rejects_missing_ipv6_addresses() {
    let address = DaemonAddress::new(String::from("127.0.0.1"), 873).expect("address");
    let error = resolve_daemon_addresses(&address, AddressMode::Ipv6)
        .expect_err("IPv6 filtering should fail for IPv4-only host");

    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("does not have IPv6 addresses"));
}

#[test]
fn resolve_daemon_addresses_filters_ipv6_mode() {
    let address = DaemonAddress::new(String::from("::1"), 873).expect("address");
    let addresses =
        resolve_daemon_addresses(&address, AddressMode::Ipv6).expect("ipv6 resolution succeeds");

    assert!(!addresses.is_empty());
    assert!(addresses.iter().all(std::net::SocketAddr::is_ipv6));
}

#[test]
fn daemon_address_accepts_ipv6_zone_identifier() {
    let address =
        DaemonAddress::new(String::from("fe80::1%eth0"), 873).expect("zone identifier accepted");
    assert_eq!(address.host(), "fe80::1%eth0");
    assert_eq!(address.port(), 873);

    let display = format!("{}", address.socket_addr_display());
    assert_eq!(display, "[fe80::1%eth0]:873");
}

#[test]
fn module_list_request_parses_ipv6_zone_identifier() {
    let operands = vec![OsString::from("rsync://fe80::1%eth0/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request present");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);

    let bracketed = vec![OsString::from("rsync://[fe80::1%25eth0]/")];
    let request = ModuleListRequest::from_operands(&bracketed)
        .expect("parse succeeds")
        .expect("request present");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_rejects_truncated_percent_encoding() {
    let operands = vec![OsString::from("rsync://example%2/")];
    let error = ModuleListRequest::from_operands(&operands)
        .expect_err("truncated percent encoding should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("invalid percent-encoding in daemon host")
    );
}

#[test]
fn daemon_address_trims_host_whitespace() {
    let address =
        DaemonAddress::new("  example.com  ".to_string(), 873).expect("address trims host");
    assert_eq!(address.host(), "example.com");
    assert_eq!(address.port(), 873);
}

#[test]
fn module_list_request_rejects_empty_username() {
    let operands = vec![OsString::from("@example.com::")];
    let error =
        ModuleListRequest::from_operands(&operands).expect_err("empty username should be rejected");
    let rendered = error.message().to_string();
    assert!(rendered.contains("daemon username must be non-empty"));
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
}

#[test]
fn module_list_request_rejects_ipv6_module_transfer() {
    let operands = vec![OsString::from("[fe80::1]::module")];
    let request = ModuleListRequest::from_operands(&operands).expect("parse succeeds");
    assert!(request.is_none());
}

#[test]
fn module_list_request_requires_bracketed_ipv6_host() {
    let operands = vec![OsString::from("fe80::1::")];
    let error = ModuleListRequest::from_operands(&operands)
        .expect_err("unbracketed IPv6 host should be rejected");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("IPv6 daemon addresses must be enclosed in brackets")
    );
}

#[test]
fn run_module_list_collects_entries() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@RSYNCD: MOTD Welcome to the test daemon\n",
        "@RSYNCD: MOTD Maintenance window at 02:00 UTC\n",
        "@RSYNCD: OK\n",
        "alpha\tPrimary module\n",
        "beta\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(
        list.motd_lines(),
        &[
            String::from("Welcome to the test daemon"),
            String::from("Maintenance window at 02:00 UTC"),
        ]
    );
    assert!(list.capabilities().is_empty());
    assert_eq!(list.entries().len(), 2);
    assert_eq!(list.entries()[0].name(), "alpha");
    assert_eq!(list.entries()[0].comment(), Some("Primary module"));
    assert_eq!(list.entries()[1].name(), "beta");
    assert_eq!(list.entries()[1].comment(), None);

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_uses_connect_program_command() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let command = OsString::from(
        "sh -c 'CONNECT_HOST=%H\n\
         CONNECT_PORT=%P\n\
         printf \"@RSYNCD: 31.0\\n\"\n\
         read greeting\n\
         printf \"@RSYNCD: OK\\n\"\n\
         read request\n\
         printf \"example\\t$CONNECT_HOST:$CONNECT_PORT\\n@RSYNCD: EXIT\\n\"'",
    );

    let _prog_guard = EnvGuard::set_os("RSYNC_CONNECT_PROG", &command);
    let _shell_guard = EnvGuard::remove("RSYNC_SHELL");
    let _proxy_guard = EnvGuard::remove("RSYNC_PROXY");

    let request = ModuleListRequest::from_components(
        DaemonAddress::new("example.com".to_string(), 873).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("connect program listing succeeds");
    assert_eq!(list.entries().len(), 1);
    let entry = &list.entries()[0];
    assert_eq!(entry.name(), "example");
    assert_eq!(entry.comment(), Some("example.com:873"));
}

#[test]
fn connect_program_token_expansion_matches_upstream_rules() {
    let template = OsString::from("netcat %H %P %%");
    let config = ConnectProgramConfig::new(template, None).expect("config");
    let rendered = config
        .format_command("daemon.example", 10873)
        .expect("rendered command");

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(rendered.as_bytes(), b"netcat daemon.example 10873 %");
    }

    #[cfg(not(unix))]
    {
        assert_eq!(rendered, OsString::from("netcat daemon.example 10873 %"));
    }
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_port_option() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.port = Some(10_873);
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");

    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--port=10873"));
}

#[test]
fn run_module_list_collects_motd_after_acknowledgement() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@RSYNCD: OK\n",
        "@RSYNCD: MOTD: Post-acknowledgement notice\n",
        "gamma\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(
        list.motd_lines(),
        &[String::from("Post-acknowledgement notice")]
    );
    assert!(list.capabilities().is_empty());
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "gamma");
    assert!(list.entries()[0].comment().is_none());

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_suppresses_motd_when_requested() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@RSYNCD: MOTD Welcome to the test daemon\n",
        "@RSYNCD: OK\n",
        "alpha\tPrimary module\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list =
        run_module_list_with_options(request, ModuleListOptions::default().suppress_motd(true))
            .expect("module list succeeds");
    assert!(list.motd_lines().is_empty());
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "alpha");
    assert_eq!(list.entries()[0].comment(), Some("Primary module"));

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_collects_warnings() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@WARNING: Maintenance scheduled\n",
        "@RSYNCD: OK\n",
        "delta\n",
        "@WARNING: Additional notice\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "delta");
    assert_eq!(
        list.warnings(),
        &[
            String::from("Maintenance scheduled"),
            String::from("Additional notice")
        ]
    );
    assert!(list.capabilities().is_empty());

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_collects_capabilities() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@RSYNCD: CAP modules uid\n",
        "@RSYNCD: OK\n",
        "epsilon\n",
        "@RSYNCD: CAP compression\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "epsilon");
    assert_eq!(
        list.capabilities(),
        &[String::from("modules uid"), String::from("compression")]
    );

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_via_proxy_connects_through_tunnel() {
    let responses = vec!["@RSYNCD: OK\n", "theta\n", "@RSYNCD: EXIT\n"];
    let (daemon_addr, daemon_handle) = spawn_stub_daemon(responses);
    let (proxy_addr, request_rx, proxy_handle) =
        spawn_stub_proxy(daemon_addr, None, DEFAULT_PROXY_STATUS_LINE);

    let _env_lock = env_lock().lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set(
        "RSYNC_PROXY",
        &format!("{}:{}", proxy_addr.ip(), proxy_addr.port()),
    );

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(daemon_addr.ip().to_string(), daemon_addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "theta");

    let captured = request_rx.recv().expect("proxy request");
    assert!(
        captured
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("CONNECT "))
    );

    proxy_handle.join().expect("proxy thread");
    daemon_handle.join().expect("daemon thread");
}

#[test]
fn run_module_list_via_proxy_includes_auth_header() {
    let responses = vec!["@RSYNCD: OK\n", "iota\n", "@RSYNCD: EXIT\n"];
    let (daemon_addr, daemon_handle) = spawn_stub_daemon(responses);
    let expected_header = "Proxy-Authorization: Basic dXNlcjpzZWNyZXQ=";
    let (proxy_addr, request_rx, proxy_handle) = spawn_stub_proxy(
        daemon_addr,
        Some(expected_header),
        DEFAULT_PROXY_STATUS_LINE,
    );

    let _env_lock = env_lock().lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set(
        "RSYNC_PROXY",
        &format!("user:secret@{}:{}", proxy_addr.ip(), proxy_addr.port()),
    );

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(daemon_addr.ip().to_string(), daemon_addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "iota");

    let captured = request_rx.recv().expect("proxy request");
    assert!(captured.contains(expected_header));

    proxy_handle.join().expect("proxy thread");
    daemon_handle.join().expect("daemon thread");
}

#[test]
fn establish_proxy_tunnel_formats_ipv6_authority_without_brackets() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy listener");
    let addr = listener.local_addr().expect("proxy addr");
    let expected_line = "CONNECT fe80::1%eth0:873 HTTP/1.0\r\n";

    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept proxy connection");
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read CONNECT request");
        assert_eq!(line, expected_line);

        line.clear();
        reader.read_line(&mut line).expect("read blank line");
        assert!(line == "\r\n" || line == "\n");

        let mut stream = reader.into_inner();
        stream
            .write_all(b"HTTP/1.0 200 Connection established\r\n\r\n")
            .expect("write proxy response");
        stream.flush().expect("flush proxy response");
    });

    let daemon_addr = DaemonAddress::new(String::from("fe80::1%eth0"), 873).expect("daemon addr");
    let proxy = ProxyConfig {
        host: String::from("proxy.example"),
        port: addr.port(),
        credentials: None,
    };

    let mut stream = TcpStream::connect(addr).expect("connect to proxy listener");
    establish_proxy_tunnel(&mut stream, &daemon_addr, &proxy).expect("tunnel negotiation succeeds");

    handle.join().expect("proxy thread completes");
}

#[test]
fn run_module_list_accepts_lowercase_proxy_status_line() {
    let responses = vec!["@RSYNCD: OK\n", "kappa\n", "@RSYNCD: EXIT\n"];
    let (daemon_addr, daemon_handle) = spawn_stub_daemon(responses);
    let (proxy_addr, _request_rx, proxy_handle) =
        spawn_stub_proxy(daemon_addr, None, LOWERCASE_PROXY_STATUS_LINE);

    let _env_lock = env_lock().lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set(
        "RSYNC_PROXY",
        &format!("{}:{}", proxy_addr.ip(), proxy_addr.port()),
    );

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(daemon_addr.ip().to_string(), daemon_addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "kappa");

    proxy_handle.join().expect("proxy thread");
    daemon_handle.join().expect("daemon thread");
}

#[test]
fn run_module_list_reports_invalid_proxy_configuration() {
    let _env_lock = env_lock().lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set("RSYNC_PROXY", "invalid-proxy");

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(String::from("localhost"), 873).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("invalid proxy should fail");
    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("RSYNC_PROXY must be in HOST:PORT form")
    );
}

#[test]
fn parse_proxy_spec_accepts_http_scheme() {
    let proxy =
        parse_proxy_spec("http://user:secret@proxy.example:8080").expect("http proxy parses");
    assert_eq!(proxy.host, "proxy.example");
    assert_eq!(proxy.port, 8080);
    assert_eq!(
        proxy.authorization_header(),
        Some(String::from("dXNlcjpzZWNyZXQ="))
    );
}

#[test]
fn parse_proxy_spec_decodes_percent_encoded_credentials() {
    let proxy = parse_proxy_spec("http://user%3Aname:p%40ss%25word@proxy.example:1080")
        .expect("percent-encoded proxy parses");
    assert_eq!(proxy.host, "proxy.example");
    assert_eq!(proxy.port, 1080);
    assert_eq!(
        proxy.authorization_header(),
        Some(String::from("dXNlcjpuYW1lOnBAc3Mld29yZA=="))
    );
}

#[test]
fn parse_proxy_spec_accepts_https_scheme() {
    let proxy = parse_proxy_spec("https://proxy.example:3128").expect("https proxy parses");
    assert_eq!(proxy.host, "proxy.example");
    assert_eq!(proxy.port, 3128);
    assert!(proxy.authorization_header().is_none());
}

#[test]
fn parse_proxy_spec_rejects_unknown_scheme() {
    let error = match parse_proxy_spec("socks5://proxy:1080") {
        Ok(_) => panic!("invalid proxy scheme should be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("RSYNC_PROXY scheme must be http:// or https://")
    );
}

#[test]
fn parse_proxy_spec_rejects_path_component() {
    let error = match parse_proxy_spec("http://proxy.example:3128/path") {
        Ok(_) => panic!("proxy specification with path should be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("RSYNC_PROXY must not include a path component")
    );
}

#[test]
fn parse_proxy_spec_rejects_invalid_percent_encoding_in_credentials() {
    let error = match parse_proxy_spec("user%zz:secret@proxy.example:8080") {
        Ok(_) => panic!("invalid percent-encoding should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("RSYNC_PROXY username contains invalid percent-encoding")
    );

    let error = match parse_proxy_spec("user:secret%@proxy.example:8080") {
        Ok(_) => panic!("truncated percent-encoding should be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("RSYNC_PROXY password contains truncated percent-encoding")
    );
}

#[test]
fn run_module_list_reports_daemon_error() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec!["@ERROR: unavailable\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("daemon error should surface");
    assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    assert!(error.message().to_string().contains("unavailable"));

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_reports_daemon_error_without_colon() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec!["@ERROR unavailable\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("daemon error should surface");
    assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    assert!(error.message().to_string().contains("unavailable"));

    handle.join().expect("server thread");
}

#[test]
fn map_daemon_handshake_error_converts_error_payload() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(
        io::ErrorKind::InvalidData,
        NegotiationError::MalformedLegacyGreeting {
            input: "@ERROR module unavailable".to_string(),
        },
    );

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    assert!(mapped.message().to_string().contains("module unavailable"));
}

#[test]
fn map_daemon_handshake_error_converts_plain_invalid_data_error() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(io::ErrorKind::InvalidData, "@ERROR daemon unavailable");

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    assert!(mapped.message().to_string().contains("daemon unavailable"));
}

#[test]
fn map_daemon_handshake_error_converts_other_malformed_greetings() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(
        io::ErrorKind::InvalidData,
        NegotiationError::MalformedLegacyGreeting {
            input: "@RSYNCD? unexpected".to_string(),
        },
    );

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), PROTOCOL_INCOMPATIBLE_EXIT_CODE);
    assert!(mapped.message().to_string().contains("@RSYNCD? unexpected"));
}

#[test]
fn map_daemon_handshake_error_propagates_other_failures() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(io::ErrorKind::TimedOut, "timed out");

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), SOCKET_IO_EXIT_CODE);
    let rendered = mapped.message().to_string();
    assert!(rendered.contains("timed out"));
    assert!(rendered.contains("negotiate with"));
}

#[test]
fn run_module_list_reports_daemon_error_with_case_insensitive_prefix() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec!["@error:\tunavailable\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("daemon error should surface");
    assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    assert!(error.message().to_string().contains("unavailable"));

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_reports_authentication_required() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec!["@RSYNCD: AUTHREQD modules\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("auth requirement should surface");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("requires authentication"));
    assert!(rendered.contains("username"));

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_requires_password_for_authentication() {
    let responses = vec!["@RSYNCD: AUTHREQD challenge\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(None);

    let error = run_module_list(request).expect_err("missing password should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(error.message().to_string().contains("RSYNC_PASSWORD"));

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_authenticates_with_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth daemon");
    let addr = listener.local_addr().expect("local addr");
    let challenge = "abc123";
    let expected = compute_daemon_auth_response(b"secret", challenge);

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("write timeout");

            stream
                .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                .expect("write greeting");
            stream.flush().expect("flush greeting");

            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read client greeting");
            assert_eq!(line, LEGACY_DAEMON_GREETING);

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
            assert_eq!(received, format!("user {expected}"));

            for response in ["@RSYNCD: OK\n", "secured\n", "@RSYNCD: EXIT\n"] {
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
            reader.get_mut().flush().expect("flush response");
        }
    });

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(Some(b"secret".to_vec()));
    let list = run_module_list(request).expect("module list succeeds");
    set_test_daemon_password(None);

    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "secured");

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_authenticates_with_password_override() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind override daemon");
    let addr = listener.local_addr().expect("local addr");
    let challenge = "override";
    let expected = compute_daemon_auth_response(b"override-secret", challenge);

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("write timeout");

            stream
                .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                .expect("write greeting");
            stream.flush().expect("flush greeting");

            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read client greeting");
            assert_eq!(line, LEGACY_DAEMON_GREETING);

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
            assert_eq!(received, format!("user {expected}"));

            for response in ["@RSYNCD: OK\n", "override\n", "@RSYNCD: EXIT\n"] {
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
            reader.get_mut().flush().expect("flush response");
        }
    });

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(Some(b"wrong".to_vec()));
    let list = run_module_list_with_password(
        request,
        Some(b"override-secret".to_vec()),
        TransferTimeout::Default,
    )
    .expect("module list succeeds");
    set_test_daemon_password(None);

    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "override");

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_authenticates_with_split_challenge() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind split auth daemon");
    let addr = listener.local_addr().expect("local addr");
    let challenge = "split123";
    let expected = compute_daemon_auth_response(b"secret", challenge);

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("write timeout");

            stream
                .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                .expect("write greeting");
            stream.flush().expect("flush greeting");

            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read client greeting");
            assert_eq!(line, LEGACY_DAEMON_GREETING);

            line.clear();
            reader.read_line(&mut line).expect("read request");
            assert_eq!(line, "#list\n");

            reader
                .get_mut()
                .write_all(b"@RSYNCD: AUTHREQD\n")
                .expect("write authreqd");
            reader.get_mut().flush().expect("flush authreqd");

            reader
                .get_mut()
                .write_all(format!("@RSYNCD: AUTH {challenge}\n").as_bytes())
                .expect("write challenge");
            reader.get_mut().flush().expect("flush challenge");

            line.clear();
            reader.read_line(&mut line).expect("read credentials");
            let received = line.trim_end_matches(['\n', '\r']);
            assert_eq!(received, format!("user {expected}"));

            for response in ["@RSYNCD: OK\n", "protected\n", "@RSYNCD: EXIT\n"] {
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
            reader.get_mut().flush().expect("flush response");
        }
    });

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(Some(b"secret".to_vec()));
    let list = run_module_list(request).expect("module list succeeds");
    set_test_daemon_password(None);

    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "protected");

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_reports_authentication_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth daemon");
    let addr = listener.local_addr().expect("local addr");
    let challenge = "abcdef";
    let expected = compute_daemon_auth_response(b"secret", challenge);

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("write timeout");

            stream
                .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                .expect("write greeting");
            stream.flush().expect("flush greeting");

            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read client greeting");
            assert_eq!(line, LEGACY_DAEMON_GREETING);

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
            assert_eq!(received, format!("user {expected}"));

            reader
                .get_mut()
                .write_all(b"@RSYNCD: AUTHFAILED credentials rejected\n")
                .expect("write failure");
            reader
                .get_mut()
                .write_all(b"@RSYNCD: EXIT\n")
                .expect("write exit");
            reader.get_mut().flush().expect("flush failure");
        }
    });

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(Some(b"secret".to_vec()));
    let error = run_module_list(request).expect_err("auth failure surfaces");
    set_test_daemon_password(None);

    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("rejected provided credentials")
    );

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_reports_access_denied() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec!["@RSYNCD: DENIED host rules\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("denied response should surface");
    assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("denied access"));
    assert!(rendered.contains("host rules"));

    handle.join().expect("server thread");
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var_os(key);
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }

    fn set_os(key: &'static str, value: &OsStr) -> Self {
        let previous = env::var_os(key);
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = env::var_os(key);
        #[allow(unsafe_code)]
        unsafe {
            env::remove_var(key);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.previous.take() {
            #[allow(unsafe_code)]
            unsafe {
                env::set_var(self.key, value);
            }
        } else {
            #[allow(unsafe_code)]
            unsafe {
                env::remove_var(self.key);
            }
        }
    }
}

const DEFAULT_PROXY_STATUS_LINE: &str = "HTTP/1.0 200 Connection established";
const LOWERCASE_PROXY_STATUS_LINE: &str = "http/1.1 200 Connection Established";

fn spawn_stub_proxy(
    target: std::net::SocketAddr,
    expected_header: Option<&'static str>,
    status_line: &'static str,
) -> (
    std::net::SocketAddr,
    mpsc::Receiver<String>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
    let addr = listener.local_addr().expect("proxy addr");
    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream);
            let mut captured = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).expect("read request line") == 0 {
                    break;
                }
                captured.push_str(&line);
                if line == "\r\n" || line == "\n" {
                    break;
                }
            }

            if let Some(expected) = expected_header {
                assert!(captured.contains(expected), "missing proxy header");
            }

            tx.send(captured).expect("send captured request");

            let mut client_stream = reader.into_inner();
            let mut server_stream = TcpStream::connect(target).expect("connect daemon");
            client_stream
                .write_all(status_line.as_bytes())
                .expect("write proxy response");
            client_stream
                .write_all(b"\r\n\r\n")
                .expect("terminate proxy status");

            let mut client_clone = client_stream.try_clone().expect("clone client");
            let mut server_clone = server_stream.try_clone().expect("clone server");

            let forward = thread::spawn(move || {
                let _ = io::copy(&mut client_clone, &mut server_stream);
            });
            let backward = thread::spawn(move || {
                let _ = io::copy(&mut server_clone, &mut client_stream);
            });

            let _ = forward.join();
            let _ = backward.join();
        }
    });

    (addr, rx, handle)
}

fn spawn_stub_daemon(
    responses: Vec<&'static str>,
) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub daemon");
    let addr = listener.local_addr().expect("local addr");

    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            handle_connection(stream, responses);
        }
    });

    (addr, handle)
}

fn handle_connection(mut stream: TcpStream, responses: Vec<&'static str>) {
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
    assert_eq!(line, LEGACY_DAEMON_GREETING);

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

    let stream = reader.into_inner();
    let _ = stream.shutdown(Shutdown::Both);
}
