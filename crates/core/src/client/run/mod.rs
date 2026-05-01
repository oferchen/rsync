//! Client transfer execution and orchestration.
//!
//! This module implements the primary entry points for executing file transfers,
//! including [`run_client`] and [`run_client_with_observer`]. These functions
//! coordinate local copies and remote transfers over SSH and rsync daemon
//! protocols, mirroring the dispatch logic in upstream `main.c:start_client()`.
//!
//! The orchestration layer handles:
//! - Configuration validation and argument parsing
//! - Progress tracking and event collection
//! - Filter rule compilation and application
//! - Batch mode file replay and recording
//! - Remote transfer role determination
//!
//! # Upstream Reference
//!
//! - `main.c:start_client()` - Top-level client dispatch
//! - `main.c:do_cmd()` - SSH fork/exec and role selection
//! - `main.c:read_batch()` - Batch file replay entry point
//! - `options.c` - Argument validation and server options building
//!
//! # Examples
//!
//! Basic local transfer:
//!
//! ```ignore
//! use core::client::{ClientConfig, run_client};
//!
//! let config = ClientConfig::builder()
//!     .transfer_args(["source/", "dest/"])
//!     .recursive(true)
//!     .build();
//!
//! let summary = run_client(config)?;
//! println!("Transferred {} files", summary.files_copied());
//! ```
//!
//! Transfer with progress reporting:
//!
//! ```ignore
//! use core::client::{ClientConfig, run_client_with_observer};
//!
//! let mut observer = |update| {
//!     println!("Progress: {}/{}", update.index(), update.total());
//! };
//!
//! let config = ClientConfig::builder()
//!     .transfer_args(["large_source/", "dest/"])
//!     .build();
//!
//! run_client_with_observer(config, Some(&mut observer))?;
//! ```

mod batch;
mod filters;

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

#[cfg(feature = "tracing")]
use tracing::instrument;

use engine::local_copy::{FilterProgram, LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};

use super::config::{BandwidthLimit, ClientConfig, DeleteMode};
use super::error::{ClientError, map_local_copy_error, missing_operands_error};
use super::progress::{ClientProgressForwarder, ClientProgressObserver};
use super::remote;
use super::summary::ClientSummary;

/// Runs the client orchestration using the provided configuration.
///
/// Mirrors upstream `main.c:start_client()` by dispatching to the local copy
/// engine, SSH transport, or daemon protocol based on the operand format.
/// Both paths return a summary of the work performed.
///
/// # Arguments
///
/// * `config` - The client configuration specifying sources, destination,
///   and transfer options.
///
/// # Returns
///
/// Returns `Ok(ClientSummary)` on successful transfer with statistics about
/// files copied, bytes transferred, etc. Returns `Err(ClientError)` if the
/// transfer fails or configuration is invalid.
///
/// # Errors
///
/// Returns an error if:
/// - No transfer operands are provided (missing source or destination)
/// - The destination directory cannot be accessed due to permission denied
/// - Filter rules fail to compile due to invalid patterns
/// - The local copy engine fails during file transfer
/// - Remote SSH or daemon transfer fails
/// - Batch file operations fail (creation, header writing, or flushing)
///
/// # Examples
///
/// ```no_run
/// use core::client::{run_client, ClientConfig};
///
/// let config = ClientConfig::builder()
///     .transfer_args(vec!["source.txt", "dest.txt"])
///     .build();
///
/// let summary = run_client(config)?;
/// println!("Copied {} files", summary.files_copied());
/// # Ok::<(), core::client::ClientError>(())
/// ```
#[cfg_attr(feature = "tracing", instrument(skip(config)))]
pub fn run_client(config: ClientConfig) -> Result<ClientSummary, ClientError> {
    run_client_internal(config, None)
}

/// Runs the client orchestration while reporting progress events.
///
/// When an observer is supplied the transfer emits progress updates mirroring
/// the behaviour of upstream rsync's `--info=progress2`.
///
/// # Arguments
///
/// * `config` - The client configuration specifying sources, destination,
///   and transfer options.
/// * `observer` - Optional progress observer to receive transfer updates.
///   Pass `None` for no progress reporting.
///
/// # Returns
///
/// Returns `Ok(ClientSummary)` on successful transfer with statistics about
/// files copied, bytes transferred, etc. Returns `Err(ClientError)` if the
/// transfer fails or configuration is invalid.
///
/// # Errors
///
/// Returns an error if:
/// - No transfer operands are provided (missing source or destination)
/// - The destination directory cannot be accessed due to permission denied
/// - Filter rules fail to compile due to invalid patterns
/// - The local copy engine fails during file transfer
/// - Remote SSH or daemon transfer fails
/// - Batch file operations fail (creation, header writing, or flushing)
///
/// # Examples
///
/// ```no_run
/// use core::client::{run_client_with_observer, ClientConfig, ClientProgressUpdate};
///
/// struct MyObserver;
/// impl core::client::ClientProgressObserver for MyObserver {
///     fn on_update(&mut self, update: &ClientProgressUpdate) {
///         println!("Progress: {}/{}", update.index(), update.total());
///     }
/// }
///
/// let config = ClientConfig::builder()
///     .transfer_args(vec!["source/", "dest/"])
///     .build();
///
/// let mut observer = MyObserver;
/// let summary = run_client_with_observer(config, Some(&mut observer))?;
/// # Ok::<(), core::client::ClientError>(())
/// ```
#[cfg_attr(feature = "tracing", instrument(skip(config, observer)))]
pub fn run_client_with_observer(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    run_client_internal(config, observer)
}

#[cfg_attr(
    feature = "tracing",
    instrument(skip(config, observer), name = "client_internal")
)]
fn run_client_internal(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    if !config.has_transfer_request() {
        return Err(missing_operands_error());
    }

    // Handle batch mode configuration
    let batch_writer = if let Some(batch_cfg) = config.batch_config() {
        if let Some(result) = batch::handle_batch_read(batch_cfg, &config) {
            return result;
        }
        Some(batch::create_batch_writer(batch_cfg)?)
    } else {
        None
    };

    // Check for remote operands and dispatch appropriately
    let has_daemon_url = config.transfer_args().iter().any(|arg| {
        arg.to_string_lossy().starts_with("rsync://") || arg.to_string_lossy().contains("::")
    });

    if has_daemon_url {
        // Daemon data transfer via rsync:// URLs
        return remote::run_daemon_transfer(&config, observer, batch_writer);
    }

    let has_remote = config
        .transfer_args()
        .iter()
        .any(|arg| remote::operand_is_remote(arg));

    if has_remote {
        // When the embedded-ssh feature is enabled and any operand uses an
        // ssh:// URL, dispatch to the embedded SSH transport instead of
        // spawning the system ssh binary.
        #[cfg(feature = "embedded-ssh")]
        {
            let has_ssh_url = config
                .transfer_args()
                .iter()
                .any(|arg| remote::is_ssh_url(&arg.to_string_lossy()));

            if has_ssh_url {
                let summary =
                    remote::run_embedded_ssh_transfer(&config, observer, batch_writer.clone())?;

                if let Some(ref writer_arc) = batch_writer
                    && let Some(batch_cfg) = config.batch_config()
                {
                    batch::finalize_batch(writer_arc, batch_cfg, &summary)?;
                }

                return Ok(summary);
            }
        }

        let summary = remote::run_ssh_transfer(&config, observer, batch_writer.clone())?;

        // Finalize batch file if batch mode was active
        if let Some(ref writer_arc) = batch_writer
            && let Some(batch_cfg) = config.batch_config()
        {
            batch::finalize_batch(writer_arc, batch_cfg, &summary)?;
        }

        return Ok(summary);
    }

    // Local copy path
    let plan = match LocalCopyPlan::from_operands(config.transfer_args()) {
        Ok(plan) => plan,
        Err(error) => return Err(map_local_copy_error(error)),
    };

    // Mirror upstream: validate destination directory access early (main.c:751)
    // This returns FILE_SELECTION error (3) instead of PARTIAL_TRANSFER (23)
    // Check the destination itself if it's a directory, otherwise check its parent
    use std::fs;
    let dest_to_check = if plan.destination().is_dir() {
        plan.destination()
    } else if let Some(parent) = plan.destination().parent() {
        parent
    } else {
        plan.destination()
    };

    // Try to read the directory - this will fail with Permission Denied if not accessible
    if let Err(error) = fs::read_dir(dest_to_check) {
        // Only return FILE_SELECTION error for permission denied
        // Other errors (like NotFound) should proceed normally
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            return Err(super::error::destination_access_error(dest_to_check, error));
        }
    }

    let filter_program = filters::compile_filter_program(config.filter_rules())?;
    let mut options = build_local_copy_options(&config, filter_program);

    // Attach batch writer to options if in batch write mode
    let batch_writer_for_options = if let Some(ref writer) = batch_writer {
        batch::write_batch_header(writer, &config)?;
        Some(writer.clone())
    } else {
        None
    };

    if let Some(ref writer_arc) = batch_writer_for_options {
        options = options.batch_writer(Some(writer_arc.clone()));
    }

    let mode = if config.dry_run() || config.list_only() {
        LocalCopyExecution::DryRun
    } else {
        LocalCopyExecution::Apply
    };

    let collect_events = config.collect_events();

    if collect_events {
        options = options.collect_events(true);
    }

    let mut handler_adapter = observer
        .map(|observer| ClientProgressForwarder::new(observer, &plan, options.clone()))
        .transpose()?;

    let summary = if collect_events {
        plan.execute_with_report_and_handler(
            mode,
            options,
            handler_adapter
                .as_mut()
                .map(ClientProgressForwarder::as_handler_mut),
        )
        .map(ClientSummary::from_report)
    } else {
        plan.execute_with_options_and_handler(
            mode,
            options,
            handler_adapter
                .as_mut()
                .map(ClientProgressForwarder::as_handler_mut),
        )
        .map(ClientSummary::from_summary)
    };

    let summary = summary.map_err(map_local_copy_error)?;

    // Finalize batch file if batch mode was active
    if let Some(ref writer_arc) = batch_writer
        && let Some(batch_cfg) = config.batch_config()
    {
        batch::finalize_batch(writer_arc, batch_cfg, &summary)?;
    }

    Ok(summary)
}

/// Builder for [`LocalCopyOptions`] derived from a [`ClientConfig`] and
/// optional [`FilterProgram`].
///
/// This encapsulates the translation from CLI-facing configuration to
/// engine options using a Builder-style facade, keeping
/// `build_local_copy_options` small and testable.
///
/// The option mapping mirrors upstream `options.c:server_options()` which
/// translates CLI flags into the compact server argument format.
struct LocalCopyOptionsBuilder<'a> {
    config: &'a ClientConfig,
    filter_program: Option<FilterProgram>,
}

impl<'a> LocalCopyOptionsBuilder<'a> {
    const fn new(config: &'a ClientConfig, filter_program: Option<FilterProgram>) -> Self {
        Self {
            config,
            filter_program,
        }
    }

    fn build(self) -> LocalCopyOptions {
        let config = self.config;
        let mut options = LocalCopyOptions::default();

        options = self.apply_recursion_and_delete(options, config);
        options = self.apply_core_limits_and_bandwidth(options, config);
        options = self.apply_compression(options, config);
        options = self.apply_metadata_preservation(options, config);
        options = self.apply_behavioral_flags(options, config);
        options = self.apply_paths_and_backups(options, config);
        options = self.apply_time_and_timeout(options, config);
        options = self.apply_reference_directories(options, config);
        options = self.apply_iconv(options, config);
        options = self.apply_filter_program(options);

        options
    }

    /// Resolves the user's `--iconv` request into a
    /// [`FilenameConverter`](protocol::iconv::FilenameConverter) and
    /// attaches it to the local-copy options.
    ///
    /// This is the local-copy mirror of the SSH/daemon path's
    /// `apply_common_server_flags`
    /// (`crates/core/src/client/remote/flags.rs:203-228`), which writes the
    /// converter onto `transfer::config::ConnectionConfig::iconv`. The
    /// local-copy executor lives in the engine crate and does not traverse
    /// the SSH/daemon `ServerConfig` builder, so the bridge has to be
    /// re-applied here.
    ///
    /// When `IconvSetting::Unspecified` or `IconvSetting::Disabled`,
    /// `resolve_converter` returns `None`, leaving the engine's
    /// pass-through behaviour untouched. This mirrors upstream rsync's
    /// behaviour when `--iconv` is absent or `--no-iconv` is supplied.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c::iconv_for_local`
    /// - `options.c::recv_iconv_settings`
    /// - `compat.c:716-718`
    fn apply_iconv(&self, options: LocalCopyOptions, config: &ClientConfig) -> LocalCopyOptions {
        options.with_iconv(config.iconv().resolve_converter())
    }

    const fn apply_recursion_and_delete(
        &self,
        mut options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        options = options.recursive(config.recursive());

        if config.delete_mode().is_enabled() || config.delete_excluded() {
            options = options.delete(true);
        }

        options = match config.delete_mode() {
            DeleteMode::Before => options.delete_before(true),
            DeleteMode::After => options.delete_after(true),
            DeleteMode::Delay => options.delete_delay(true),
            DeleteMode::During | DeleteMode::Disabled => options,
        };

        options
            .delete_excluded(config.delete_excluded())
            .max_deletions(config.max_delete())
    }

    fn apply_core_limits_and_bandwidth(
        &self,
        options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        options
            .min_file_size(config.min_file_size())
            .max_file_size(config.max_file_size())
            .with_block_size_override(config.block_size_override())
            .remove_source_files(config.remove_source_files())
            .bandwidth_limit(
                config
                    .bandwidth_limit()
                    .map(BandwidthLimit::bytes_per_second),
            )
            .bandwidth_burst(
                config
                    .bandwidth_limit()
                    .and_then(BandwidthLimit::burst_bytes),
            )
    }

    fn apply_compression(
        &self,
        options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        // When writing batch files, force zlib compression for cross-tool
        // interop. The batch header records do_compression but not which
        // algorithm was used, so upstream rsync (which defaults to zlib)
        // cannot decode zstd/lz4 batch data. Force zlib to match upstream.
        // upstream: batch.c tees compressed wire bytes using zlib by default.
        let algorithm = if config.batch_config().is_some_and(|bc| bc.is_write_mode()) {
            compress::algorithm::CompressionAlgorithm::Zlib
        } else {
            config.compression_algorithm()
        };
        options
            .with_compression_algorithm(algorithm)
            .with_default_compression_level(config.compression_setting().level_or_default())
            .with_skip_compress(config.skip_compress().clone())
            .compress(config.compress())
            .with_compression_level_override(config.compression_level())
    }

    fn apply_metadata_preservation(
        &self,
        mut options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        // Resolve --copy-as USER[:GROUP] specification into numeric IDs
        let copy_as_ids = config
            .copy_as()
            .and_then(|spec| ::metadata::parse_copy_as_spec(spec).ok());

        options = options
            .with_stop_at(config.stop_at())
            .whole_file_option(config.whole_file_raw())
            .open_noatime(config.open_noatime())
            .owner(config.preserve_owner())
            .with_owner_override(config.owner_override())
            .group(config.preserve_group())
            .with_group_override(config.group_override())
            .with_copy_as(copy_as_ids)
            .with_chmod(config.chmod().cloned())
            .executability(config.preserve_executability())
            .permissions(config.preserve_permissions())
            .times(config.preserve_times())
            .atimes(config.preserve_atimes())
            .crtimes(config.preserve_crtimes())
            .omit_dir_times(config.omit_dir_times())
            .omit_link_times(config.omit_link_times())
            .with_user_mapping(config.user_mapping().cloned())
            .with_group_mapping(config.group_mapping().cloned());

        #[cfg(all(unix, feature = "acl"))]
        {
            options = options.acls(config.preserve_acls());
        }

        // IMPORTANT: LocalCopyOptions::xattrs is only implemented on Unix.
        // Match the engine crate's cfg so Windows builds with the `xattr`
        // feature do not try to call a non-existent method.
        #[cfg(all(feature = "xattr", unix))]
        {
            options = options.xattrs(config.preserve_xattrs());
        }

        options
    }

    fn apply_behavioral_flags(
        &self,
        options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        options
            .checksum(config.checksum())
            .with_checksum_algorithm(config.checksum_signature_algorithm())
            .size_only(config.size_only())
            .ignore_times(config.ignore_times())
            .ignore_existing(config.ignore_existing())
            .existing_only(config.existing_only())
            .ignore_missing_args(config.ignore_missing_args())
            .delete_missing_args(config.delete_missing_args())
            .update(config.update())
            .with_modify_window(config.modify_window_duration())
            .numeric_ids(config.numeric_ids())
            .preallocate(config.preallocate())
            .fsync(config.fsync())
            .hard_links(config.preserve_hard_links())
            .links(config.links())
            .sparse(config.sparse())
            .copy_links(config.copy_links())
            .copy_dirlinks(config.copy_dirlinks())
            .copy_devices_as_files(config.copy_devices())
            .copy_unsafe_links(config.copy_unsafe_links())
            .keep_dirlinks(config.keep_dirlinks())
            .safe_links(config.safe_links())
            .munge_links(config.munge_links())
            .devices(config.preserve_devices())
            .specials(config.preserve_specials())
            .with_one_file_system_level(config.one_file_system_level())
            .relative_paths(config.relative_paths())
            .dirs(config.dirs())
            .implied_dirs(config.implied_dirs())
            .mkpath(config.mkpath())
            .prune_empty_dirs(config.prune_empty_dirs())
            .inplace(config.inplace())
            .append(config.append())
            .append_verify(config.append_verify())
            .partial(config.partial())
            .force_replacements(config.force_replacements())
    }

    fn apply_paths_and_backups(
        &self,
        options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        options
            .with_temp_directory(config.temp_directory().map(Path::to_path_buf))
            .backup(config.backup())
            .with_backup_directory(config.backup_directory().map(Path::to_path_buf))
            .with_backup_suffix(config.backup_suffix().map(OsStr::to_os_string))
            .with_partial_directory(config.partial_directory().map(Path::to_path_buf))
            .delay_updates(config.delay_updates())
            .extend_link_dests(config.link_dest_paths().iter().cloned())
    }

    fn apply_time_and_timeout(
        &self,
        options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        options.with_timeout(
            config
                .timeout()
                .as_seconds()
                .map(|seconds| Duration::from_secs(seconds.get())),
        )
    }

    fn apply_reference_directories(
        &self,
        mut options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        if !config.reference_directories().is_empty() {
            options = options
                .extend_reference_directories(config.reference_directories().iter().cloned());
        }
        options
    }

    fn apply_filter_program(self, options: LocalCopyOptions) -> LocalCopyOptions {
        options.with_filter_program(self.filter_program)
    }
}

/// Builds [`LocalCopyOptions`] reflecting the provided client configuration and optional filter
/// program.
///
/// This helper mirrors the internal wiring used by [`run_client`](super::run_client) so that unit
/// tests can validate the translation layer without re-invoking the entire transfer engine.
#[doc(hidden)]
pub fn build_local_copy_options(
    config: &ClientConfig,
    filter_program: Option<FilterProgram>,
) -> LocalCopyOptions {
    LocalCopyOptionsBuilder::new(config, filter_program).build()
}

#[cfg(test)]
mod iconv_wiring_tests {
    use std::ffi::OsString;

    use super::build_local_copy_options;
    use crate::client::config::{ClientConfig, IconvSetting};

    fn config_with_iconv(setting: IconvSetting) -> ClientConfig {
        ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .iconv(setting)
            .build()
    }

    #[test]
    fn local_copy_options_iconv_unset_yields_none() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();
        let options = build_local_copy_options(&config, None);
        assert!(options.iconv().is_none());
    }

    #[test]
    fn local_copy_options_iconv_disabled_yields_none() {
        let config = config_with_iconv(IconvSetting::Disabled);
        let options = build_local_copy_options(&config, None);
        assert!(options.iconv().is_none());
    }

    #[test]
    fn local_copy_options_iconv_locale_default_yields_some() {
        let config = config_with_iconv(IconvSetting::LocaleDefault);
        let options = build_local_copy_options(&config, None);
        let converter = options
            .iconv()
            .expect("locale-default iconv should produce a converter");
        // converter_from_locale() currently returns identity; the contract
        // is that *some* converter is attached, regardless of identity.
        assert!(converter.is_identity());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn local_copy_options_iconv_explicit_yields_non_identity_converter() {
        let config = config_with_iconv(IconvSetting::Explicit {
            local: "UTF-8".to_owned(),
            remote: Some("ISO-8859-1".to_owned()),
        });
        let options = build_local_copy_options(&config, None);
        let converter = options
            .iconv()
            .expect("explicit iconv pair should produce a converter");
        assert!(!converter.is_identity());
        assert_eq!(converter.local_encoding_name(), "UTF-8");
    }

    #[test]
    fn local_copy_options_iconv_unsupported_charset_falls_back_to_none() {
        let config = config_with_iconv(IconvSetting::Explicit {
            local: "definitely-not-a-real-charset".to_owned(),
            remote: Some("also-fake".to_owned()),
        });
        let options = build_local_copy_options(&config, None);
        assert!(options.iconv().is_none());
    }
}
