#![deny(unsafe_code)]

use std::ffi::OsString;
use std::io::Write;
use std::num::NonZeroU32;

use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;
use core::client::{
    BandwidthLimit, CompressionSetting, SkipCompressList, force_no_compress_from_env,
    parse_skip_compress_list, skip_compress_from_env,
};
use logging_sink::MessageSink;

use super::super::{
    parse_bandwidth_limit, parse_block_size_argument, parse_compress_choice, parse_compress_level,
    parse_compress_level_argument, parse_debug_flags, parse_info_flags, parse_max_delete_argument,
    parse_modify_window_argument, parse_size_limit_argument,
};
use super::messages::fail_with_message;
use crate::frontend::{
    arguments::BandwidthArgument,
    defaults::ITEMIZE_CHANGES_FORMAT,
    execution::{CompressLevelArg, DEBUG_HELP_TEXT, INFO_HELP_TEXT},
    out_format::{OutFormat, parse_out_format},
    progress::{NameOutputLevel, ProgressMode, ProgressSetting},
};

/// Inputs used to derive execution settings that require additional parsing.
pub(crate) struct SettingsInputs<'a> {
    pub(crate) info: &'a [OsString],
    pub(crate) debug: &'a [OsString],
    pub(crate) itemize_changes: bool,
    pub(crate) out_format: Option<&'a OsString>,
    pub(crate) initial_progress: ProgressSetting,
    pub(crate) initial_stats: bool,
    pub(crate) initial_name_level: NameOutputLevel,
    pub(crate) initial_name_overridden: bool,
    pub(crate) bwlimit: &'a Option<BandwidthArgument>,
    pub(crate) max_delete: &'a Option<OsString>,
    pub(crate) min_size: &'a Option<OsString>,
    pub(crate) max_size: &'a Option<OsString>,
    pub(crate) block_size: &'a Option<OsString>,
    pub(crate) max_alloc: &'a Option<OsString>,
    pub(crate) modify_window: &'a Option<OsString>,
    pub(crate) compress_flag: bool,
    pub(crate) no_compress: bool,
    pub(crate) compress_level: &'a Option<OsString>,
    pub(crate) compress_choice: &'a Option<OsString>,
    pub(crate) skip_compress: &'a Option<OsString>,
    pub(crate) log_file: Option<&'a OsString>,
    pub(crate) log_file_format: Option<&'a OsString>,
}

/// Derived execution settings gathered from [`derive_settings`].
#[allow(dead_code)]
pub(crate) struct DerivedSettings {
    pub(crate) out_format_template: Option<OutFormat>,
    pub(crate) progress_setting: ProgressSetting,
    pub(crate) progress_mode: Option<ProgressMode>,
    pub(crate) stats: bool,
    pub(crate) name_level: NameOutputLevel,
    pub(crate) name_overridden: bool,
    pub(crate) debug_flags_list: Vec<OsString>,
    pub(crate) bandwidth_limit: Option<BandwidthLimit>,
    pub(crate) max_delete_limit: Option<u64>,
    pub(crate) min_size_limit: Option<u64>,
    pub(crate) max_size_limit: Option<u64>,
    pub(crate) block_size_override: Option<NonZeroU32>,
    pub(crate) max_alloc_limit: Option<u64>,
    pub(crate) modify_window_setting: Option<u64>,
    pub(crate) compress: bool,
    pub(crate) compress_disabled: bool,
    pub(crate) compression_level_override: Option<CompressionLevel>,
    pub(crate) compress_level_cli: Option<OsString>,
    pub(crate) skip_compress_list: Option<SkipCompressList>,
    pub(crate) compression_setting: CompressionSetting,
    pub(crate) compress_choice_cli: Option<OsString>,
    pub(crate) compression_algorithm: Option<CompressionAlgorithm>,
    pub(crate) log_file_path: Option<OsString>,
    pub(crate) log_file_format_cli: Option<OsString>,
    pub(crate) log_file_template: Option<OutFormat>,
}

/// Outcome of parsing additional execution settings.
pub(crate) enum SettingsOutcome {
    /// Parsing produced fully-resolved settings.
    Proceed(Box<DerivedSettings>),
    /// Parsing requested an early exit with the supplied exit code.
    Exit(i32),
}

/// Result of parsing info flags.
struct InfoFlagsResult {
    progress_setting: ProgressSetting,
    stats: bool,
    name_level: NameOutputLevel,
    name_overridden: bool,
}

/// Parses --info flags and returns display settings.
fn parse_info_settings<Out, Err>(
    stdout: &mut Out,
    stderr: &mut MessageSink<Err>,
    info_args: &[OsString],
    initial_progress: ProgressSetting,
    initial_stats: bool,
    initial_name_level: NameOutputLevel,
    initial_name_overridden: bool,
) -> Result<InfoFlagsResult, i32>
where
    Out: Write,
    Err: Write,
{
    let mut progress_setting = initial_progress;
    let mut stats = initial_stats;
    let mut name_level = initial_name_level;
    let mut name_overridden = initial_name_overridden;

    if info_args.is_empty() {
        return Ok(InfoFlagsResult {
            progress_setting,
            stats,
            name_level,
            name_overridden,
        });
    }

    match parse_info_flags(info_args) {
        Ok(settings) => {
            if settings.help_requested {
                if stdout.write_all(INFO_HELP_TEXT.as_bytes()).is_err() {
                    let _ = write!(stderr.writer_mut(), "{INFO_HELP_TEXT}");
                    return Err(1);
                }
                return Err(0);
            }

            match settings.progress {
                ProgressSetting::Unspecified => {}
                value => progress_setting = value,
            }
            if let Some(level) = settings.stats {
                stats = level > 0;
            }
            if let Some(level) = settings.name {
                name_level = level;
                name_overridden = true;
            }

            // Apply info flags to verbosity config
            for info_arg in info_args {
                if let Some(s) = info_arg.to_str() {
                    for token in s.split(',') {
                        let token = token.trim();
                        if !token.is_empty() && token != "help" {
                            let _ = logging::apply_info_flag(token);
                        }
                    }
                }
            }

            Ok(InfoFlagsResult {
                progress_setting,
                stats,
                name_level,
                name_overridden,
            })
        }
        Err(message) => Err(fail_with_message(message, stderr)),
    }
}

/// Parses --debug flags and returns debug flag list.
fn parse_debug_settings<Out, Err>(
    stdout: &mut Out,
    stderr: &mut MessageSink<Err>,
    debug_args: &[OsString],
) -> Result<Vec<OsString>, i32>
where
    Out: Write,
    Err: Write,
{
    if debug_args.is_empty() {
        return Ok(Vec::new());
    }

    match parse_debug_flags(debug_args) {
        Ok(settings) => {
            if settings.help_requested {
                if stdout.write_all(DEBUG_HELP_TEXT.as_bytes()).is_err() {
                    let _ = write!(stderr.writer_mut(), "{DEBUG_HELP_TEXT}");
                    return Err(1);
                }
                return Err(0);
            }

            let flags: Vec<OsString> = settings
                .iter_enabled_flags()
                .map(|(name, level)| OsString::from(format!("{name}{level}")))
                .collect();

            // Apply debug flags to verbosity config
            for debug_arg in debug_args {
                if let Some(s) = debug_arg.to_str() {
                    for token in s.split(',') {
                        let token = token.trim();
                        if !token.is_empty() && token != "help" {
                            let _ = logging::apply_debug_flag(token);
                        }
                    }
                }
            }

            Ok(flags)
        }
        Err(message) => Err(fail_with_message(message, stderr)),
    }
}

/// Result of parsing size/limit arguments.
struct SizeLimitsResult {
    bandwidth_limit: Option<BandwidthLimit>,
    max_delete_limit: Option<u64>,
    min_size_limit: Option<u64>,
    max_size_limit: Option<u64>,
    block_size_override: Option<NonZeroU32>,
    max_alloc_limit: Option<u64>,
    modify_window_setting: Option<u64>,
}

/// Input parameters for size/limit parsing, grouped to reduce argument count.
struct SizeLimitsInputs<'a> {
    bwlimit: &'a Option<BandwidthArgument>,
    max_delete: &'a Option<OsString>,
    min_size: &'a Option<OsString>,
    max_size: &'a Option<OsString>,
    block_size: &'a Option<OsString>,
    max_alloc: &'a Option<OsString>,
    modify_window: &'a Option<OsString>,
}

/// Parses bandwidth and size limit arguments.
fn parse_size_limits<Err>(
    stderr: &mut MessageSink<Err>,
    inputs: SizeLimitsInputs<'_>,
) -> Result<SizeLimitsResult, i32>
where
    Err: Write,
{
    let bandwidth_limit = match inputs.bwlimit.as_ref() {
        Some(BandwidthArgument::Limit(value)) => match parse_bandwidth_limit(value.as_os_str()) {
            Ok(limit) => limit,
            Err(message) => return Err(fail_with_message(message, stderr)),
        },
        Some(BandwidthArgument::Disabled) | None => None,
    };

    let max_delete_limit = match inputs.max_delete {
        Some(value) => match parse_max_delete_argument(value.as_os_str()) {
            Ok(limit) => Some(limit),
            Err(message) => return Err(fail_with_message(message, stderr)),
        },
        None => None,
    };

    let min_size_limit = match inputs.min_size.as_ref() {
        Some(value) => match parse_size_limit_argument(value.as_os_str(), "--min-size") {
            Ok(limit) => Some(limit),
            Err(message) => return Err(fail_with_message(message, stderr)),
        },
        None => None,
    };

    let max_size_limit = match inputs.max_size.as_ref() {
        Some(value) => match parse_size_limit_argument(value.as_os_str(), "--max-size") {
            Ok(limit) => Some(limit),
            Err(message) => return Err(fail_with_message(message, stderr)),
        },
        None => None,
    };

    let block_size_override = match inputs.block_size.as_ref() {
        Some(value) => match parse_block_size_argument(value.as_os_str()) {
            Ok(size) => Some(size),
            Err(message) => return Err(fail_with_message(message, stderr)),
        },
        None => None,
    };

    let max_alloc_limit = match inputs.max_alloc.as_ref() {
        Some(value) => match parse_size_limit_argument(value.as_os_str(), "--max-alloc") {
            Ok(limit) => Some(limit),
            Err(message) => return Err(fail_with_message(message, stderr)),
        },
        None => None,
    };

    let modify_window_setting = match inputs.modify_window.as_ref() {
        Some(value) => match parse_modify_window_argument(value.as_os_str()) {
            Ok(window) => Some(window),
            Err(message) => return Err(fail_with_message(message, stderr)),
        },
        None => None,
    };

    Ok(SizeLimitsResult {
        bandwidth_limit,
        max_delete_limit,
        min_size_limit,
        max_size_limit,
        block_size_override,
        max_alloc_limit,
        modify_window_setting,
    })
}

/// Result of parsing compression settings.
struct CompressionResult {
    compress: bool,
    compress_disabled: bool,
    compression_level_override: Option<CompressionLevel>,
    compress_level_cli: Option<OsString>,
    skip_compress_list: Option<SkipCompressList>,
    compression_setting: CompressionSetting,
    compress_choice_cli: Option<OsString>,
    compression_algorithm: Option<CompressionAlgorithm>,
}

/// Parses all compression-related settings.
fn parse_compression_settings<Err>(
    stderr: &mut MessageSink<Err>,
    compress_flag: bool,
    no_compress: bool,
    compress_level: &Option<OsString>,
    compress_choice: &Option<OsString>,
    skip_compress: &Option<OsString>,
) -> Result<CompressionResult, i32>
where
    Err: Write,
{
    let mut compress = compress_flag;
    let mut compression_level_override = None;
    let mut compression_algorithm = None;
    let mut compress_choice_cli = compress_choice.clone();
    let mut compress_choice_disabled = false;

    let mut compress_level_setting = match compress_level {
        Some(value) => match parse_compress_level(value.as_os_str()) {
            Ok(setting) => Some(setting),
            Err(message) => return Err(fail_with_message(message, stderr)),
        },
        None => None,
    };

    if let Some(choice) = compress_choice.as_ref() {
        match parse_compress_choice(choice.as_os_str()) {
            Ok(None) => {
                compress = false;
                compression_level_override = None;
                compress_level_setting = Some(CompressLevelArg::Disable);
                compress_choice_disabled = true;
                compress_choice_cli = None;
            }
            Ok(Some(algorithm)) => {
                compression_algorithm = Some(algorithm);
                if !no_compress {
                    compress = true;
                }
            }
            Err(message) => return Err(fail_with_message(message, stderr)),
        }
    }

    if let Some(ref setting) = compress_level_setting {
        match setting {
            CompressLevelArg::Disable => {
                compress = false;
            }
            CompressLevelArg::Level(level) => {
                if !no_compress {
                    compress = true;
                    compression_level_override = Some(CompressionLevel::precise(*level));
                }
            }
        }
    }

    let mut compress_disabled = no_compress
        || compress_choice_disabled
        || matches!(compress_level_setting, Some(CompressLevelArg::Disable));

    let force_no_compress = match force_no_compress_from_env("OC_RSYNC_FORCE_NO_COMPRESS") {
        Ok(value) => value,
        Err(message) => return Err(fail_with_message(message, stderr)),
    };

    if force_no_compress == Some(true) {
        compress = false;
        compression_level_override = None;
        compress_level_setting = Some(CompressLevelArg::Disable);
        compress_disabled = true;
        compression_algorithm = None;
        compress_choice_cli = None;
    }

    if compress_disabled {
        compress_choice_cli = None;
    }

    let compress_level_cli = match (compress_level_setting.as_ref(), compress_disabled) {
        (Some(CompressLevelArg::Level(level)), false) => {
            Some(OsString::from(level.get().to_string()))
        }
        (Some(CompressLevelArg::Disable), _) => Some(OsString::from("0")),
        _ => None,
    };

    let skip_compress_list = if let Some(value) = skip_compress.as_ref() {
        match parse_skip_compress_list(value.as_os_str()) {
            Ok(list) => Some(list),
            Err(message) => return Err(fail_with_message(message, stderr)),
        }
    } else {
        match skip_compress_from_env("RSYNC_SKIP_COMPRESS") {
            Ok(value) => value,
            Err(message) => return Err(fail_with_message(message, stderr)),
        }
    };

    let compression_setting = match compress_level_setting {
        Some(CompressLevelArg::Disable) => CompressionSetting::disabled(),
        Some(CompressLevelArg::Level(level)) => {
            CompressionSetting::level(CompressionLevel::precise(level))
        }
        None => {
            if let Some(value) = compress_level {
                match parse_compress_level_argument(value.as_os_str()) {
                    Ok(setting) => {
                        compress = !setting.is_disabled();
                        setting
                    }
                    Err(message) => {
                        return Err(fail_with_message(message, stderr));
                    }
                }
            } else {
                CompressionSetting::default()
            }
        }
    };

    Ok(CompressionResult {
        compress,
        compress_disabled,
        compression_level_override,
        compress_level_cli,
        skip_compress_list,
        compression_setting,
        compress_choice_cli,
        compression_algorithm,
    })
}

/// Result of parsing log file settings.
struct LogFileResult {
    log_file_path: Option<OsString>,
    log_file_format_cli: Option<OsString>,
    log_file_template: Option<OutFormat>,
}

/// Parses log file path and format settings.
fn parse_log_settings<Err>(
    stderr: &mut MessageSink<Err>,
    log_file: Option<&OsString>,
    log_file_format: Option<&OsString>,
) -> Result<LogFileResult, i32>
where
    Err: Write,
{
    match log_file {
        Some(path) => {
            let (format_string, format_cli) = if let Some(spec) = log_file_format {
                (spec.clone(), Some(spec.clone()))
            } else {
                (OsString::from("%i %n%L"), None)
            };

            match parse_out_format(format_string.as_os_str()) {
                Ok(template) => Ok(LogFileResult {
                    log_file_path: Some(path.clone()),
                    log_file_format_cli: format_cli,
                    log_file_template: Some(template),
                }),
                Err(message) => Err(fail_with_message(message, stderr)),
            }
        }
        None => Ok(LogFileResult {
            log_file_path: None,
            log_file_format_cli: None,
            log_file_template: None,
        }),
    }
}

/// Parses out format template from CLI arguments.
fn parse_out_format_template<Err>(
    stderr: &mut MessageSink<Err>,
    out_format: Option<&OsString>,
    itemize_changes: bool,
) -> Result<Option<OutFormat>, i32>
where
    Err: Write,
{
    let template = match out_format {
        Some(value) => match parse_out_format(value.as_os_str()) {
            Ok(template) => Some(template),
            Err(message) => return Err(fail_with_message(message, stderr)),
        },
        None => None,
    };

    if itemize_changes && template.is_none() {
        Ok(Some(
            parse_out_format(OsString::from(ITEMIZE_CHANGES_FORMAT).as_os_str())
                .expect("default itemize-changes format parses"),
        ))
    } else {
        Ok(template)
    }
}

/// Parses advanced execution settings derived from CLI flags.
pub(crate) fn derive_settings<Out, Err>(
    stdout: &mut Out,
    stderr: &mut MessageSink<Err>,
    inputs: SettingsInputs<'_>,
) -> SettingsOutcome
where
    Out: Write,
    Err: Write,
{
    // Parse output format template
    let out_format_template =
        match parse_out_format_template(stderr, inputs.out_format, inputs.itemize_changes) {
            Ok(template) => template,
            Err(code) => return SettingsOutcome::Exit(code),
        };

    // Parse info flags for display settings
    let info_result = match parse_info_settings(
        stdout,
        stderr,
        inputs.info,
        inputs.initial_progress,
        inputs.initial_stats,
        inputs.initial_name_level,
        inputs.initial_name_overridden,
    ) {
        Ok(result) => result,
        Err(code) => return SettingsOutcome::Exit(code),
    };

    // Parse debug flags
    let debug_flags_list = match parse_debug_settings(stdout, stderr, inputs.debug) {
        Ok(flags) => flags,
        Err(code) => return SettingsOutcome::Exit(code),
    };

    // Parse size and bandwidth limits
    let limits = match parse_size_limits(
        stderr,
        SizeLimitsInputs {
            bwlimit: inputs.bwlimit,
            max_delete: inputs.max_delete,
            min_size: inputs.min_size,
            max_size: inputs.max_size,
            block_size: inputs.block_size,
            max_alloc: inputs.max_alloc,
            modify_window: inputs.modify_window,
        },
    ) {
        Ok(limits) => limits,
        Err(code) => return SettingsOutcome::Exit(code),
    };

    // Parse compression settings
    let compression = match parse_compression_settings(
        stderr,
        inputs.compress_flag,
        inputs.no_compress,
        inputs.compress_level,
        inputs.compress_choice,
        inputs.skip_compress,
    ) {
        Ok(result) => result,
        Err(code) => return SettingsOutcome::Exit(code),
    };

    // Parse log file settings
    let log = match parse_log_settings(stderr, inputs.log_file, inputs.log_file_format) {
        Ok(result) => result,
        Err(code) => return SettingsOutcome::Exit(code),
    };

    SettingsOutcome::Proceed(Box::new(DerivedSettings {
        out_format_template,
        progress_setting: info_result.progress_setting,
        progress_mode: info_result.progress_setting.resolved(),
        stats: info_result.stats,
        name_level: info_result.name_level,
        name_overridden: info_result.name_overridden,
        debug_flags_list,
        bandwidth_limit: limits.bandwidth_limit,
        max_delete_limit: limits.max_delete_limit,
        min_size_limit: limits.min_size_limit,
        max_size_limit: limits.max_size_limit,
        block_size_override: limits.block_size_override,
        max_alloc_limit: limits.max_alloc_limit,
        modify_window_setting: limits.modify_window_setting,
        compress: compression.compress,
        compress_disabled: compression.compress_disabled,
        compression_level_override: compression.compression_level_override,
        compress_level_cli: compression.compress_level_cli,
        skip_compress_list: compression.skip_compress_list,
        compression_setting: compression.compression_setting,
        compress_choice_cli: compression.compress_choice_cli,
        compression_algorithm: compression.compression_algorithm,
        log_file_path: log.log_file_path,
        log_file_format_cli: log.log_file_format_cli,
        log_file_template: log.log_file_template,
    }))
}
