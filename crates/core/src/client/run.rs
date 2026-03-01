//! Client transfer execution and orchestration.
//!
//! This module implements the primary entry points for executing file transfers,
//! including [`run_client`] and [`run_client_with_observer`]. These functions
//! coordinate local copies and remote transfers over SSH and rsync daemon
//! protocols.
//!
//! The orchestration layer handles:
//! - Configuration validation and argument parsing
//! - Progress tracking and event collection
//! - Filter rule compilation and application
//! - Batch mode file replay and recording
//! - Remote transfer role determination
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

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(feature = "tracing")]
use tracing::instrument;

use engine::batch::BatchWriter;
use engine::local_copy::{
    DirMergeRule, ExcludeIfPresentRule, FilterProgram, FilterProgramEntry, LocalCopyExecution,
    LocalCopyOptions, LocalCopyPlan,
};
use filters::FilterRule as EngineFilterRule;

use super::config::{BandwidthLimit, ClientConfig, DeleteMode, FilterRuleKind, FilterRuleSpec};
use super::error::{
    ClientError, compile_filter_error, map_local_copy_error, missing_operands_error,
};
use super::progress::{ClientProgressForwarder, ClientProgressObserver};
use super::remote;
use super::summary::ClientSummary;

/// Runs the client orchestration using the provided configuration.
///
/// The helper executes the local copy engine for local transfers, or the
/// native SSH transport for remote transfers. Both paths return a summary
/// of the work performed.
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
/// the behaviour of `--info=progress2`.
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
        if batch_cfg.is_read_mode() {
            // upstream: main.c:1464-1473 — reject remote destinations with --read-batch
            let has_remote_dest = config.transfer_args().iter().any(|arg| {
                let s = arg.to_string_lossy();
                s.starts_with("rsync://") || s.contains("::") || remote::operand_is_remote(arg)
            });
            if has_remote_dest {
                use crate::message::Role;
                use crate::rsync_error;
                return Err(ClientError::new(
                    super::FEATURE_UNAVAILABLE_EXIT_CODE,
                    rsync_error!(
                        super::FEATURE_UNAVAILABLE_EXIT_CODE,
                        "remote destination is not allowed with --read-batch"
                    )
                    .with_role(Role::Client),
                ));
            }
            // Replay the batch file instead of performing a normal transfer
            return replay_batch(batch_cfg, &config);
        }

        // For write modes, create the BatchWriter
        match BatchWriter::new((*batch_cfg).clone()) {
            Ok(writer) => {
                // Wrap in Arc<Mutex<...>> for thread-safe shared access
                Some(Arc::new(Mutex::new(writer)))
            }
            Err(e) => {
                use crate::message::Role;
                use crate::rsync_error;
                let msg = format!(
                    "failed to create batch file '{}': {}",
                    batch_cfg.batch_file_path().display(),
                    e
                );
                return Err(ClientError::new(
                    1,
                    rsync_error!(1, "{}", msg).with_role(Role::Client),
                ));
            }
        }
    } else {
        None
    };

    // Check for remote operands and dispatch appropriately
    let has_daemon_url = config.transfer_args().iter().any(|arg| {
        arg.to_string_lossy().starts_with("rsync://") || arg.to_string_lossy().contains("::")
    });

    if has_daemon_url {
        // Daemon data transfer via rsync:// URLs
        return remote::run_daemon_transfer(&config, observer);
    }

    let has_remote = config
        .transfer_args()
        .iter()
        .any(|arg| remote::operand_is_remote(arg));

    if has_remote {
        return remote::run_ssh_transfer(&config, observer);
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

    let filter_program = compile_filter_program(config.filter_rules())?;
    let mut options = build_local_copy_options(&config, filter_program);

    // Attach batch writer to options if in batch write mode
    let batch_writer_for_options = if let Some(ref writer) = batch_writer {
        // Determine preserve_xattrs conditionally based on feature
        #[cfg(all(unix, feature = "xattr"))]
        let preserve_xattrs = config.preserve_xattrs();
        #[cfg(not(all(unix, feature = "xattr")))]
        let preserve_xattrs = false;

        // Write batch header with stream flags before starting transfer
        let batch_flags = engine::batch::BatchFlags {
            recurse: config.recursive(),
            preserve_uid: config.preserve_owner(),
            preserve_gid: config.preserve_group(),
            preserve_links: config.links(),
            preserve_hard_links: config.preserve_hard_links(),
            always_checksum: config.checksum(),
            xfer_dirs: config.dirs(),
            do_compression: config.compress(),
            preserve_xattrs,
            inplace: config.inplace(),
            append: config.append(),
            append_verify: config.append_verify(),
            ..Default::default()
        };

        {
            let mut w = writer.lock().map_err(|_| {
                use crate::message::Role;
                use crate::rsync_error;
                ClientError::new(
                    1,
                    rsync_error!(1, "batch writer lock poisoned").with_role(Role::Client),
                )
            })?;
            if let Err(e) = w.write_header(batch_flags) {
                use crate::message::Role;
                use crate::rsync_error;
                let msg = format!("failed to write batch header: {e}");
                return Err(ClientError::new(
                    1,
                    rsync_error!(1, "{}", msg).with_role(Role::Client),
                ));
            }
        }

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
        // Finalize and close the batch file
        {
            let mut writer = writer_arc.lock().map_err(|_| {
                use crate::message::Role;
                use crate::rsync_error;
                ClientError::new(
                    1,
                    rsync_error!(1, "batch writer lock poisoned").with_role(Role::Client),
                )
            })?;
            if let Err(e) = writer.flush() {
                use crate::message::Role;
                use crate::rsync_error;
                let msg = format!("failed to flush batch file: {e}");
                return Err(ClientError::new(
                    1,
                    rsync_error!(1, "{}", msg).with_role(Role::Client),
                ));
            }
        } // Drop the lock and BatchWriter will be cleaned up by Drop impl

        // Generate the .sh replay script
        if let Err(e) = engine::batch::script::generate_script(batch_cfg) {
            use crate::message::Role;
            use crate::rsync_error;
            let msg = format!("failed to generate batch script: {e}");
            return Err(ClientError::new(
                1,
                rsync_error!(1, "{}", msg).with_role(Role::Client),
            ));
        }
    }

    Ok(summary)
}

/// Builder for [`LocalCopyOptions`] derived from a [`ClientConfig`] and
/// optional [`FilterProgram`].
///
/// This encapsulates the translation from CLI-facing configuration to
/// engine options using a Builder-style façade, keeping
/// `build_local_copy_options` small and testable.
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
        options = self.apply_filter_program(options);

        options
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
        options
            .with_compression_algorithm(config.compression_algorithm())
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

fn compile_filter_program(rules: &[FilterRuleSpec]) -> Result<Option<FilterProgram>, ClientError> {
    if rules.is_empty() {
        return Ok(None);
    }

    let mut entries = Vec::new();
    for rule in rules {
        match rule.kind() {
            FilterRuleKind::Include => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::include(rule.pattern().to_owned())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver())
                    .with_perishable(rule.is_perishable())
                    .with_xattr_only(rule.is_xattr_only()),
            )),
            FilterRuleKind::Exclude => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::exclude(rule.pattern().to_owned())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver())
                    .with_perishable(rule.is_perishable())
                    .with_xattr_only(rule.is_xattr_only()),
            )),
            FilterRuleKind::Clear => entries.push(FilterProgramEntry::Clear),
            FilterRuleKind::Protect => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::protect(rule.pattern().to_owned())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver())
                    .with_perishable(rule.is_perishable()),
            )),
            FilterRuleKind::Risk => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::risk(rule.pattern().to_owned())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver())
                    .with_perishable(rule.is_perishable()),
            )),
            FilterRuleKind::DirMerge => {
                entries.push(FilterProgramEntry::DirMerge(DirMergeRule::new(
                    rule.pattern().to_owned(),
                    rule.dir_merge_options().cloned().unwrap_or_default(),
                )))
            }
            FilterRuleKind::ExcludeIfPresent => entries.push(FilterProgramEntry::ExcludeIfPresent(
                ExcludeIfPresentRule::new(rule.pattern().to_owned()),
            )),
        }
    }

    FilterProgram::new(entries)
        .map(Some)
        .map_err(|error| compile_filter_error(error.pattern(), &error))
}

/// Replay a batch file to reconstruct the transfer at the destination.
///
/// Delegates to [`batch::replay::replay`] for the actual delta-application
/// logic, then wraps the result in a [`ClientSummary`].
fn replay_batch(
    batch_cfg: &engine::batch::BatchConfig,
    config: &ClientConfig,
) -> Result<ClientSummary, ClientError> {
    use crate::message::Role;
    use crate::rsync_error;

    let dest_root = if config.transfer_args().is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(&config.transfer_args()[0])
    };

    let result = engine::batch::replay::replay(batch_cfg, &dest_root, config.verbosity().into())
        .map_err(|e| {
            let msg = format!("batch replay failed: {e}");
            ClientError::new(1, rsync_error!(1, "{}", msg).with_role(Role::Client))
        })?;

    #[cfg(feature = "tracing")]
    {
        if result.recurse {
            tracing::info!("Batch mode enabled: recurse");
        }
        tracing::info!(
            file_count = result.file_count,
            total_size = result.total_size,
            "Batch replay complete"
        );
    }
    let _ = &result;

    use engine::local_copy::LocalCopySummary;
    Ok(ClientSummary::from_summary(LocalCopySummary::default()))
}
