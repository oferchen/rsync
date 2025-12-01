use std::ffi::OsStr;
use std::io::Write;
use std::time::Duration;

use engine::local_copy::{
    DirMergeRule, ExcludeIfPresentRule, FilterProgram, FilterProgramEntry, LocalCopyExecution,
    LocalCopyOptions, LocalCopyPlan, ReferenceDirectory as EngineReferenceDirectory,
    ReferenceDirectoryKind as EngineReferenceDirectoryKind,
};
use filters::FilterRule as EngineFilterRule;

use super::config::{
    ClientConfig, DeleteMode, FilterRuleKind, FilterRuleSpec, ReferenceDirectoryKind,
};
use super::error::{
    ClientError, compile_filter_error, map_local_copy_error, missing_operands_error,
};
use super::fallback::RemoteFallbackContext;
use super::outcome::ClientOutcome;
use super::progress::{ClientProgressForwarder, ClientProgressObserver};
use super::remote;
use super::summary::ClientSummary;

/// Runs the client orchestration using the provided configuration.
///
/// The helper executes the local copy engine for local transfers, or the
/// native SSH transport for remote transfers. Both paths return a summary
/// of the work performed.
pub fn run_client(config: ClientConfig) -> Result<ClientSummary, ClientError> {
    run_client_internal(config, None)
}

/// Runs the client orchestration while reporting progress events.
///
/// When an observer is supplied the transfer emits progress updates mirroring
/// the behaviour of `--info=progress2`.
pub fn run_client_with_observer(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    run_client_internal(config, observer)
}

/// Executes the client flow, delegating to a fallback `rsync` binary when provided.
///
/// The caller may supply a [`RemoteFallbackContext`] that describes how to invoke
/// an upstream `rsync` binary for remote transfers while the native engine
/// evolves.
pub fn run_client_or_fallback<Out, Err>(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
    _fallback: Option<RemoteFallbackContext<'_, Out, Err>>,
) -> Result<ClientOutcome, ClientError>
where
    Out: Write,
    Err: Write,
{
    run_client_internal(config, observer).map(|summary| ClientOutcome::Local(Box::new(summary)))
}

fn run_client_internal(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    if !config.has_transfer_request() {
        return Err(missing_operands_error());
    }

    // Check for remote operands and dispatch to SSH transport
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

    let filter_program = compile_filter_program(config.filter_rules())?;
    let mut options = build_local_copy_options(&config, filter_program);

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

    summary.map_err(map_local_copy_error)
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
    fn new(config: &'a ClientConfig, filter_program: Option<FilterProgram>) -> Self {
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

    fn apply_recursion_and_delete(
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
                    .map(|limit| limit.bytes_per_second()),
            )
            .bandwidth_burst(
                config
                    .bandwidth_limit()
                    .and_then(|limit| limit.burst_bytes()),
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
        options = options
            .with_stop_at(config.stop_at())
            .whole_file(config.whole_file())
            .open_noatime(config.open_noatime())
            .owner(config.preserve_owner())
            .with_owner_override(config.owner_override())
            .group(config.preserve_group())
            .with_group_override(config.group_override())
            .with_chmod(config.chmod().cloned())
            .executability(config.preserve_executability())
            .permissions(config.preserve_permissions())
            .times(config.preserve_times())
            .omit_dir_times(config.omit_dir_times())
            .omit_link_times(config.omit_link_times())
            .with_user_mapping(config.user_mapping().cloned())
            .with_group_mapping(config.group_mapping().cloned());

        #[cfg(feature = "acl")]
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
            .devices(config.preserve_devices())
            .specials(config.preserve_specials())
            .one_file_system(config.one_file_system())
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
            .with_temp_directory(config.temp_directory().map(|path| path.to_path_buf()))
            .backup(config.backup())
            .with_backup_directory(config.backup_directory().map(|path| path.to_path_buf()))
            .with_backup_suffix(config.backup_suffix().map(OsStr::to_os_string))
            .with_partial_directory(config.partial_directory().map(|path| path.to_path_buf()))
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
            let references = config.reference_directories().iter().map(|reference| {
                let kind = match reference.kind() {
                    ReferenceDirectoryKind::Compare => EngineReferenceDirectoryKind::Compare,
                    ReferenceDirectoryKind::Copy => EngineReferenceDirectoryKind::Copy,
                    ReferenceDirectoryKind::Link => EngineReferenceDirectoryKind::Link,
                };
                EngineReferenceDirectory::new(kind, reference.path().to_path_buf())
            });
            options = options.extend_reference_directories(references);
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
                EngineFilterRule::include(rule.pattern().to_string())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver())
                    .with_perishable(rule.is_perishable())
                    .with_xattr_only(rule.is_xattr_only()),
            )),
            FilterRuleKind::Exclude => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::exclude(rule.pattern().to_string())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver())
                    .with_perishable(rule.is_perishable())
                    .with_xattr_only(rule.is_xattr_only()),
            )),
            FilterRuleKind::Clear => entries.push(FilterProgramEntry::Clear),
            FilterRuleKind::Protect => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::protect(rule.pattern().to_string())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver())
                    .with_perishable(rule.is_perishable()),
            )),
            FilterRuleKind::Risk => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::risk(rule.pattern().to_string())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver())
                    .with_perishable(rule.is_perishable()),
            )),
            FilterRuleKind::DirMerge => {
                entries.push(FilterProgramEntry::DirMerge(DirMergeRule::new(
                    rule.pattern().to_string(),
                    rule.dir_merge_options().cloned().unwrap_or_default(),
                )))
            }
            FilterRuleKind::ExcludeIfPresent => entries.push(FilterProgramEntry::ExcludeIfPresent(
                ExcludeIfPresentRule::new(rule.pattern().to_string()),
            )),
        }
    }

    FilterProgram::new(entries)
        .map(Some)
        .map_err(|error| compile_filter_error(error.pattern(), &error))
}
