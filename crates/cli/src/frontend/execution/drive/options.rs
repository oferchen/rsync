#![deny(unsafe_code)]

use std::ffi::OsString;
use std::io::Write;
use std::num::NonZeroU32;

use oc_rsync_compress::algorithm::CompressionAlgorithm;
use oc_rsync_compress::zlib::CompressionLevel;
use oc_rsync_core::client::{
    BandwidthLimit, CompressionSetting, SkipCompressList, force_no_compress_from_env,
    parse_skip_compress_list, skip_compress_from_env,
};
use oc_rsync_logging::MessageSink;

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
    pub(crate) fallback_out_format: Option<OsString>,
    pub(crate) initial_progress: ProgressSetting,
    pub(crate) initial_stats: bool,
    pub(crate) initial_name_level: NameOutputLevel,
    pub(crate) initial_name_overridden: bool,
    pub(crate) bwlimit: &'a Option<BandwidthArgument>,
    pub(crate) max_delete: &'a Option<OsString>,
    pub(crate) min_size: &'a Option<OsString>,
    pub(crate) max_size: &'a Option<OsString>,
    pub(crate) block_size: &'a Option<OsString>,
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
pub(crate) struct DerivedSettings {
    pub(crate) out_format_template: Option<OutFormat>,
    pub(crate) fallback_out_format: Option<OsString>,
    pub(crate) progress_setting: ProgressSetting,
    pub(crate) progress_mode: Option<ProgressMode>,
    pub(crate) stats: bool,
    pub(crate) name_level: NameOutputLevel,
    pub(crate) name_overridden: bool,
    pub(crate) debug_flags_list: Vec<OsString>,
    pub(crate) bandwidth_limit: Option<BandwidthLimit>,
    pub(crate) fallback_bwlimit: Option<OsString>,
    pub(crate) max_delete_limit: Option<u64>,
    pub(crate) min_size_limit: Option<u64>,
    pub(crate) max_size_limit: Option<u64>,
    pub(crate) block_size_override: Option<NonZeroU32>,
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
    let mut out_format_template = match inputs.out_format {
        Some(value) => match parse_out_format(value.as_os_str()) {
            Ok(template) => Some(template),
            Err(message) => return SettingsOutcome::Exit(fail_with_message(message, stderr)),
        },
        None => None,
    };

    let mut fallback_out_format = inputs.fallback_out_format.clone();

    if inputs.itemize_changes {
        if fallback_out_format.is_none() {
            fallback_out_format = Some(OsString::from(ITEMIZE_CHANGES_FORMAT));
        }
        if out_format_template.is_none() {
            out_format_template = Some(
                parse_out_format(OsString::from(ITEMIZE_CHANGES_FORMAT).as_os_str())
                    .expect("default itemize-changes format parses"),
            );
        }
    }

    let mut progress_setting = inputs.initial_progress;
    let mut stats = inputs.initial_stats;
    let mut name_level = inputs.initial_name_level;
    let mut name_overridden = inputs.initial_name_overridden;
    let mut debug_flags_list = Vec::new();

    if !inputs.info.is_empty() {
        match parse_info_flags(inputs.info) {
            Ok(settings) => {
                if settings.help_requested {
                    if stdout.write_all(INFO_HELP_TEXT.as_bytes()).is_err() {
                        let _ = write!(stderr.writer_mut(), "{INFO_HELP_TEXT}");
                        return SettingsOutcome::Exit(1);
                    }
                    return SettingsOutcome::Exit(0);
                }

                match settings.progress {
                    ProgressSetting::Unspecified => {}
                    value => progress_setting = value,
                }
                if let Some(value) = settings.stats {
                    stats = value;
                }
                if let Some(level) = settings.name {
                    name_level = level;
                    name_overridden = true;
                }
            }
            Err(message) => {
                return SettingsOutcome::Exit(fail_with_message(message, stderr));
            }
        }
    }

    if !inputs.debug.is_empty() {
        match parse_debug_flags(inputs.debug) {
            Ok(settings) => {
                if settings.help_requested {
                    if stdout.write_all(DEBUG_HELP_TEXT.as_bytes()).is_err() {
                        let _ = write!(stderr.writer_mut(), "{DEBUG_HELP_TEXT}");
                        return SettingsOutcome::Exit(1);
                    }
                    return SettingsOutcome::Exit(0);
                }

                debug_flags_list = settings.flags;
            }
            Err(message) => {
                return SettingsOutcome::Exit(fail_with_message(message, stderr));
            }
        }
    }

    let progress_mode = progress_setting.resolved();

    let bandwidth_limit = match inputs.bwlimit.as_ref() {
        Some(BandwidthArgument::Limit(value)) => match parse_bandwidth_limit(value.as_os_str()) {
            Ok(limit) => limit,
            Err(message) => return SettingsOutcome::Exit(fail_with_message(message, stderr)),
        },
        Some(BandwidthArgument::Disabled) | None => None,
    };

    let fallback_bwlimit = match (bandwidth_limit.as_ref(), inputs.bwlimit.as_ref()) {
        (Some(limit), _) => Some(limit.fallback_argument()),
        (None, Some(BandwidthArgument::Limit(_))) => {
            Some(BandwidthLimit::fallback_unlimited_argument())
        }
        (None, Some(BandwidthArgument::Disabled)) => {
            Some(BandwidthLimit::fallback_unlimited_argument())
        }
        (None, None) => None,
    };

    let max_delete_limit = match inputs.max_delete {
        Some(value) => match parse_max_delete_argument(value.as_os_str()) {
            Ok(limit) => Some(limit),
            Err(message) => return SettingsOutcome::Exit(fail_with_message(message, stderr)),
        },
        None => None,
    };

    let min_size_limit = match inputs.min_size.as_ref() {
        Some(value) => match parse_size_limit_argument(value.as_os_str(), "--min-size") {
            Ok(limit) => Some(limit),
            Err(message) => return SettingsOutcome::Exit(fail_with_message(message, stderr)),
        },
        None => None,
    };

    let max_size_limit = match inputs.max_size.as_ref() {
        Some(value) => match parse_size_limit_argument(value.as_os_str(), "--max-size") {
            Ok(limit) => Some(limit),
            Err(message) => return SettingsOutcome::Exit(fail_with_message(message, stderr)),
        },
        None => None,
    };

    let block_size_override = match inputs.block_size.as_ref() {
        Some(value) => match parse_block_size_argument(value.as_os_str()) {
            Ok(size) => Some(size),
            Err(message) => return SettingsOutcome::Exit(fail_with_message(message, stderr)),
        },
        None => None,
    };

    let modify_window_setting = match inputs.modify_window.as_ref() {
        Some(value) => match parse_modify_window_argument(value.as_os_str()) {
            Ok(window) => Some(window),
            Err(message) => return SettingsOutcome::Exit(fail_with_message(message, stderr)),
        },
        None => None,
    };

    let mut compress = inputs.compress_flag;
    let mut compression_level_override = None;
    let mut compression_algorithm = None;
    let mut compress_choice_cli = inputs.compress_choice.clone();
    let mut compress_choice_disabled = false;
    let mut compress_level_setting = match inputs.compress_level {
        Some(value) => match parse_compress_level(value.as_os_str()) {
            Ok(setting) => Some(setting),
            Err(message) => return SettingsOutcome::Exit(fail_with_message(message, stderr)),
        },
        None => None,
    };

    if let Some(choice) = inputs.compress_choice.as_ref() {
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
                if !inputs.no_compress {
                    compress = true;
                }
            }
            Err(message) => return SettingsOutcome::Exit(fail_with_message(message, stderr)),
        }
    }

    if let Some(ref setting) = compress_level_setting {
        match setting {
            CompressLevelArg::Disable => {
                compress = false;
            }
            CompressLevelArg::Level(level) => {
                if !inputs.no_compress {
                    compress = true;
                    compression_level_override = Some(CompressionLevel::precise(*level));
                }
            }
        }
    }

    let mut compress_disabled = inputs.no_compress
        || compress_choice_disabled
        || matches!(compress_level_setting, Some(CompressLevelArg::Disable));

    let force_no_compress = match force_no_compress_from_env("OC_RSYNC_FORCE_NO_COMPRESS") {
        Ok(value) => value,
        Err(message) => return SettingsOutcome::Exit(fail_with_message(message, stderr)),
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

    let skip_compress_list = if let Some(value) = inputs.skip_compress.as_ref() {
        match parse_skip_compress_list(value.as_os_str()) {
            Ok(list) => Some(list),
            Err(message) => return SettingsOutcome::Exit(fail_with_message(message, stderr)),
        }
    } else {
        match skip_compress_from_env("RSYNC_SKIP_COMPRESS") {
            Ok(value) => value,
            Err(message) => return SettingsOutcome::Exit(fail_with_message(message, stderr)),
        }
    };

    let compression_setting = match compress_level_setting {
        Some(CompressLevelArg::Disable) => CompressionSetting::disabled(),
        Some(CompressLevelArg::Level(level)) => {
            CompressionSetting::level(CompressionLevel::precise(level))
        }
        None => {
            if let Some(value) = inputs.compress_level {
                match parse_compress_level_argument(value.as_os_str()) {
                    Ok(setting) => {
                        compress = !setting.is_disabled();
                        setting
                    }
                    Err(message) => {
                        return SettingsOutcome::Exit(fail_with_message(message, stderr));
                    }
                }
            } else {
                CompressionSetting::default()
            }
        }
    };

    let (log_file_path, log_file_format_cli, log_file_template) = match inputs.log_file {
        Some(path) => {
            let (format_string, format_cli) = if let Some(spec) = inputs.log_file_format {
                (spec.clone(), Some(spec.clone()))
            } else {
                (OsString::from("%i %n%L"), None)
            };

            match parse_out_format(format_string.as_os_str()) {
                Ok(template) => (Some(path.clone()), format_cli, Some(template)),
                Err(message) => {
                    return SettingsOutcome::Exit(fail_with_message(message, stderr));
                }
            }
        }
        None => (None, None, None),
    };

    SettingsOutcome::Proceed(Box::new(DerivedSettings {
        out_format_template,
        fallback_out_format,
        progress_setting,
        progress_mode,
        stats,
        name_level,
        name_overridden,
        debug_flags_list,
        bandwidth_limit,
        fallback_bwlimit,
        max_delete_limit,
        min_size_limit,
        max_size_limit,
        block_size_override,
        modify_window_setting,
        compress,
        compress_disabled,
        compression_level_override,
        compress_level_cli,
        skip_compress_list,
        compression_setting,
        compress_choice_cli,
        compression_algorithm,
        log_file_path,
        log_file_format_cli,
        log_file_template,
    }))
}
