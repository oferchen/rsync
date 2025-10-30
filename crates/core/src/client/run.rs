use std::ffi::OsStr;
use std::io::{self, Write};
use std::time::Duration;

use rsync_engine::local_copy::{
    DirMergeRule, ExcludeIfPresentRule, FilterProgram, FilterProgramEntry, LocalCopyArgumentError,
    LocalCopyErrorKind, LocalCopyExecution, LocalCopyOptions, LocalCopyPlan,
    ReferenceDirectory as EngineReferenceDirectory,
    ReferenceDirectoryKind as EngineReferenceDirectoryKind,
};
use rsync_filters::FilterRule as EngineFilterRule;

use super::config::{
    ClientConfig, DeleteMode, FilterRuleKind, FilterRuleSpec, ReferenceDirectoryKind,
};
use super::error::{
    ClientError, compile_filter_error, map_local_copy_error, missing_operands_error,
};
use super::fallback::{RemoteFallbackContext, run_remote_transfer_fallback};
use super::outcome::{ClientOutcome, FallbackSummary};
use super::progress::{ClientProgressForwarder, ClientProgressObserver};
use super::summary::ClientSummary;
/// Runs the client orchestration using the provided configuration.
///
/// The helper executes the local copy engine and returns a summary of the
/// work performed. Remote operands trigger a feature-unavailable error until
/// SSH and daemon transports are wired into the native engine.
pub fn run_client(config: ClientConfig) -> Result<ClientSummary, ClientError> {
    match run_client_internal::<io::Sink, io::Sink>(config, None, None) {
        Ok(ClientOutcome::Local(summary)) => Ok(*summary),
        Ok(ClientOutcome::Fallback(_)) => unreachable!("fallback unavailable without context"),
        Err(error) => Err(error),
    }
}

/// Runs the client orchestration while reporting progress events.
///
/// When an observer is supplied the transfer emits progress updates mirroring
/// the behaviour of `--info=progress2`.
pub fn run_client_with_observer(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    match run_client_internal::<io::Sink, io::Sink>(config, observer, None) {
        Ok(ClientOutcome::Local(summary)) => Ok(*summary),
        Ok(ClientOutcome::Fallback(_)) => unreachable!("fallback unavailable without context"),
        Err(error) => Err(error),
    }
}

/// Executes the client flow, delegating to a fallback `rsync` binary when provided.
///
/// The caller may supply a [`RemoteFallbackContext`] that describes how to invoke
/// an upstream `rsync` binary for remote transfers while the native engine
/// evolves.
pub fn run_client_or_fallback<Out, Err>(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
    fallback: Option<RemoteFallbackContext<'_, Out, Err>>,
) -> Result<ClientOutcome, ClientError>
where
    Out: Write,
    Err: Write,
{
    run_client_internal(config, observer, fallback)
}

fn run_client_internal<Out, Err>(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
    fallback: Option<RemoteFallbackContext<'_, Out, Err>>,
) -> Result<ClientOutcome, ClientError>
where
    Out: Write,
    Err: Write,
{
    if !config.has_transfer_request() {
        return Err(missing_operands_error());
    }

    let mut fallback = fallback;

    let plan = match LocalCopyPlan::from_operands(config.transfer_args()) {
        Ok(plan) => plan,
        Err(error) => {
            let requires_fallback =
                matches!(
                    error.kind(),
                    LocalCopyErrorKind::InvalidArgument(
                        LocalCopyArgumentError::RemoteOperandUnsupported
                    )
                ) || matches!(error.kind(), LocalCopyErrorKind::MissingSourceOperands);

            if let Some(ctx) = requires_fallback.then(|| fallback.take()).flatten() {
                return invoke_fallback(ctx);
            }

            return Err(map_local_copy_error(error));
        }
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

    summary
        .map(|summary| ClientOutcome::Local(Box::new(summary)))
        .map_err(map_local_copy_error)
}

pub(crate) fn build_local_copy_options(
    config: &ClientConfig,
    filter_program: Option<FilterProgram>,
) -> LocalCopyOptions {
    let mut options = LocalCopyOptions::default();
    if config.delete_mode().is_enabled() || config.delete_excluded() {
        options = options.delete(true);
    }
    options = match config.delete_mode() {
        DeleteMode::Before => options.delete_before(true),
        DeleteMode::After => options.delete_after(true),
        DeleteMode::Delay => options.delete_delay(true),
        DeleteMode::During | DeleteMode::Disabled => options,
    };
    options = options
        .delete_excluded(config.delete_excluded())
        .max_deletions(config.max_delete())
        .min_file_size(config.min_file_size())
        .max_file_size(config.max_file_size())
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
        .with_default_compression_level(config.compression_setting().level_or_default())
        .with_skip_compress(config.skip_compress().clone())
        .whole_file(config.whole_file())
        .compress(config.compress())
        .with_compression_level_override(config.compression_level())
        .owner(config.preserve_owner())
        .with_owner_override(config.owner_override())
        .group(config.preserve_group())
        .with_group_override(config.group_override())
        .with_chmod(config.chmod().cloned())
        .permissions(config.preserve_permissions())
        .times(config.preserve_times())
        .omit_dir_times(config.omit_dir_times())
        .omit_link_times(config.omit_link_times())
        .checksum(config.checksum())
        .with_checksum_algorithm(config.checksum_signature_algorithm())
        .size_only(config.size_only())
        .ignore_existing(config.ignore_existing())
        .ignore_missing_args(config.ignore_missing_args())
        .update(config.update())
        .with_modify_window(config.modify_window_duration())
        .with_filter_program(filter_program)
        .numeric_ids(config.numeric_ids())
        .preallocate(config.preallocate())
        .hard_links(config.preserve_hard_links())
        .sparse(config.sparse())
        .copy_links(config.copy_links())
        .copy_dirlinks(config.copy_dirlinks())
        .copy_unsafe_links(config.copy_unsafe_links())
        .keep_dirlinks(config.keep_dirlinks())
        .safe_links(config.safe_links())
        .devices(config.preserve_devices())
        .specials(config.preserve_specials())
        .relative_paths(config.relative_paths())
        .implied_dirs(config.implied_dirs())
        .mkpath(config.mkpath())
        .prune_empty_dirs(config.prune_empty_dirs())
        .inplace(config.inplace())
        .append(config.append())
        .append_verify(config.append_verify())
        .partial(config.partial())
        .with_temp_directory(config.temp_directory().map(|path| path.to_path_buf()))
        .backup(config.backup())
        .with_backup_directory(config.backup_directory().map(|path| path.to_path_buf()))
        .with_backup_suffix(config.backup_suffix().map(OsStr::to_os_string))
        .with_partial_directory(config.partial_directory().map(|path| path.to_path_buf()))
        .delay_updates(config.delay_updates())
        .extend_link_dests(config.link_dest_paths().iter().cloned())
        .with_timeout(
            config
                .timeout()
                .as_seconds()
                .map(|seconds| Duration::from_secs(seconds.get())),
        );
    #[cfg(feature = "acl")]
    {
        options = options.acls(config.preserve_acls());
    }
    #[cfg(feature = "xattr")]
    {
        options = options.xattrs(config.preserve_xattrs());
    }

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

fn invoke_fallback<Out, Err>(
    ctx: RemoteFallbackContext<'_, Out, Err>,
) -> Result<ClientOutcome, ClientError>
where
    Out: Write,
    Err: Write,
{
    let (stdout, stderr, args) = ctx.split();
    run_remote_transfer_fallback(stdout, stderr, args)
        .map(|code| ClientOutcome::Fallback(FallbackSummary::new(code)))
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
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver()),
            )),
            FilterRuleKind::Exclude => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::exclude(rule.pattern().to_string())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver()),
            )),
            FilterRuleKind::Clear => entries.push(FilterProgramEntry::Clear),
            FilterRuleKind::Protect => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::protect(rule.pattern().to_string())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver()),
            )),
            FilterRuleKind::Risk => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::risk(rule.pattern().to_string())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver()),
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
