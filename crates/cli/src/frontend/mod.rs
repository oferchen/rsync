//! # Overview
//!
//! `rsync_cli` implements the thin command-line front-end for the Rust `rsync`
//! workspace. The crate is intentionally small: it recognises the subset of
//! command-line switches that are currently supported (`--help`/`-h`,
//! `--version`/`-V`, `--daemon`, `--server`, `--dry-run`/`-n`, `--list-only`,
//! `--delete`/`--delete-excluded`, `--filter` (supporting `+`/`-` actions, the
//! `!` clear directive, and `merge FILE` directives), `--files-from`, `--from0`,
//! `--compare-dest`, `--copy-dest`, `--link-dest`, `--bwlimit`,
//! `--append`/`--append-verify`, `--remote-option`, `--connect-program`, and `--sparse`) and delegates local copy operations to
//! [`rsync_core::client::run_client`]. Daemon invocations are forwarded to
//! [`rsync_daemon::run`], while `--server` sessions immediately spawn the
//! system `rsync` binary (controlled by the `OC_RSYNC_FALLBACK` environment
//! variable) so remote-shell transports keep functioning while the native
//! server implementation is completed. Higher layers will eventually extend the
//! parser to cover the full upstream surface (remote modules, incremental
//! recursion, filters, etc.), but providing these entry points today allows
//! downstream tooling to depend on a stable binary path (`oc-rsync`, or `rsync`
//! via symlink) while development continues.
//!
//! # Design
//!
//! The crate exposes [`run`] as the primary entry point. The function accepts an
//! iterator of arguments together with handles for standard output and error,
//! mirroring the approach used by upstream rsync. Internally a
//! [`clap`](https://docs.rs/clap/) command definition performs a light-weight
//! parse that recognises `--help`, `--version`, `--dry-run`, `--delete`,
//! `--delete-excluded`, `--compare-dest`, `--copy-dest`, `--link-dest`,
//! `--filter`, `--files-from`, `--from0`, and `--bwlimit` flags while treating all other
//! tokens as transfer arguments. When a transfer is requested, the function
//! delegates to [`rsync_core::client::run_client`], which currently implements a
//! deterministic local copy pipeline with optional bandwidth pacing.
//!
//! # Invariants
//!
//! - `run` never panics; unexpected I/O failures surface as non-zero exit codes.
//! - Version output is delegated to [`rsync_core::version::VersionInfoReport`]
//!   so the CLI remains byte-identical with the canonical banner used by other
//!   workspace components.
//! - Help output is rendered by a dedicated helper using a static snapshot that
//!   documents the currently supported subset. The helper substitutes the
//!   invoked program name so wrappers like `oc-rsync` display branded banners
//!   while the full upstream-compatible renderer is implemented.
//! - Transfer attempts are forwarded to [`rsync_core::client::run_client`] so
//!   diagnostics and success cases remain centralised while higher-fidelity
//!   engines are developed.
//!
//! # Errors
//!
//! The parser returns a diagnostic message with exit code `1` when argument
//! processing fails. Transfer attempts surface their exit codes from
//! [`rsync_core::client::run_client`], preserving the structured diagnostics
//! emitted by the core crate.
//!
//! # Examples
//!
//! ```
//! use rsync_cli::run;
//!
//! let mut stdout = Vec::new();
//! let mut stderr = Vec::new();
//! let exit_code = run(
//!     [
//!         rsync_core::branding::client_program_name(),
//!         "--version",
//!     ],
//!     &mut stdout,
//!     &mut stderr,
//! );
//!
//! assert_eq!(exit_code, 0);
//! assert!(!stdout.is_empty());
//! assert!(stderr.is_empty());
//! ```
//!
//! # See also
//!
//! - [`rsync_core::version`] for the underlying banner rendering helpers.
//! - `src/bin/oc-rsync.rs` for the binary that wires [`run`] into `main`.

use std::collections::HashSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::ErrorKind;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::num::{IntErrorKind, NonZeroU8, NonZeroU64};
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::platform::{gid_t, uid_t};
mod command_builder;

use args::{BandwidthArgument, ParsedArgs, parse_args};
use rsync_compress::zlib::CompressionLevel;
#[cfg(test)]
use rsync_core::client::DirMergeEnforcedKind;
use rsync_core::{
    bandwidth::BandwidthParseError,
    branding::Brand,
    client::{
        BandwidthLimit, BindAddress, ClientConfig, ClientOutcome, ClientProgressObserver,
        CompressionSetting, DeleteMode, DirMergeOptions, FilterRuleKind, FilterRuleSpec,
        HumanReadableMode, ModuleListOptions, ModuleListRequest, RemoteFallbackArgs,
        RemoteFallbackContext, TransferTimeout, parse_skip_compress_list, run_client_or_fallback,
        run_module_list_with_password_and_options, skip_compress_from_env,
    },
    message::{Message, Role},
    rsync_error,
    version::VersionInfoReport,
};
use rsync_logging::MessageSink;
use rsync_meta::ChmodModifiers;
use rsync_protocol::{ParseProtocolVersionErrorKind, ProtocolVersion};
mod args;
mod defaults;
mod filter_rules;
mod help;
mod out_format;
pub(crate) mod password;
mod program;
mod progress;
mod server;

#[cfg(test)]
mod tests;

pub(crate) use defaults::LIST_TIMESTAMP_FORMAT;
use defaults::{ITEMIZE_CHANGES_FORMAT, SUPPORTED_OPTIONS_LIST};
use filter_rules::{
    FilterDirective, append_cvs_exclude_rules, append_filter_rules_from_files,
    apply_merge_directive, merge_directive_options, os_string_to_pattern, parse_filter_directive,
};
use help::help_text;
pub(crate) use out_format::{OutFormat, OutFormatContext, emit_out_format, parse_out_format};
use password::{load_optional_password, load_password_file};
use program::{ProgramName, detect_program_name};
pub(crate) use progress::*;

#[cfg(test)]
pub(crate) use filter_rules::MergeDirective;

#[cfg(test)]
pub(crate) fn load_filter_file_patterns(path: &Path) -> Result<Vec<String>, Message> {
    filter_rules::load_filter_file_patterns(path)
}

#[cfg(test)]
pub(crate) fn set_filter_stdin_input(data: Vec<u8>) {
    filter_rules::set_filter_stdin_input(data);
}

#[cfg(test)]
pub(crate) fn parse_merge_modifiers(
    modifiers: &str,
    directive: &str,
    allow_extended: bool,
) -> Result<(DirMergeOptions, bool), Message> {
    filter_rules::parse_merge_modifiers(modifiers, directive, allow_extended)
}

#[cfg(test)]
pub(crate) fn process_merge_directive(
    directive: &str,
    options: &DirMergeOptions,
    base_dir: &Path,
    display: &str,
    destination: &mut Vec<FilterRuleSpec>,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), Message> {
    filter_rules::process_merge_directive(
        directive,
        options,
        base_dir,
        display,
        destination,
        visited,
    )
}

/// Maximum exit code representable by a Unix process.
const MAX_EXIT_CODE: i32 = u8::MAX as i32;

/// Renders the help text describing the currently supported options.
fn render_help(program_name: ProgramName) -> String {
    help_text(program_name)
}

/// Writes a [`Message`] to the supplied sink, appending a newline.
fn write_message<W: Write>(message: &Message, sink: &mut MessageSink<W>) -> io::Result<()> {
    sink.write(message)
}

/// Runs the CLI using the provided argument iterator and output handles.
///
/// The function returns the process exit code that should be used by the caller.
/// On success, `0` is returned. All diagnostics are rendered using the central
/// [`rsync_core::message`] utilities to preserve formatting and trailers.
#[allow(clippy::module_name_repetitions)]
pub fn run<I, S, Out, Err>(arguments: I, stdout: &mut Out, stderr: &mut Err) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    Out: Write,
    Err: Write,
{
    let mut args: Vec<OsString> = arguments.into_iter().map(Into::into).collect();
    if args.is_empty() {
        args.push(OsString::from(Brand::Upstream.client_program_name()));
    }

    if server::server_mode_requested(&args) {
        return server::run_server_mode(&args, stdout, stderr);
    }

    if let Some(daemon_args) = server::daemon_mode_arguments(&args) {
        return server::run_daemon_mode(daemon_args, stdout, stderr);
    }

    let mut stderr_sink = MessageSink::new(stderr);
    match parse_args(args) {
        Ok(parsed) => execute(parsed, stdout, &mut stderr_sink),
        Err(error) => {
            let mut message = rsync_error!(1, "{}", error);
            message = message.with_role(Role::Client);
            if write_message(&message, &mut stderr_sink).is_err() {
                let _ = writeln!(stderr_sink.writer_mut(), "{}", error);
            }
            1
        }
    }
}

fn with_output_writer<'a, Out, Err, R>(
    stdout: &'a mut Out,
    stderr: &'a mut MessageSink<Err>,
    use_stderr: bool,
    f: impl FnOnce(&'a mut dyn Write) -> R,
) -> R
where
    Out: Write + 'a,
    Err: Write + 'a,
{
    if use_stderr {
        let writer: &mut Err = stderr.writer_mut();
        f(writer)
    } else {
        f(stdout)
    }
}

fn execute<Out, Err>(parsed: ParsedArgs, stdout: &mut Out, stderr: &mut MessageSink<Err>) -> i32
where
    Out: Write,
    Err: Write,
{
    let ParsedArgs {
        program_name,
        show_help,
        show_version,
        human_readable,
        dry_run,
        list_only,
        remote_shell,
        connect_program,
        daemon_port,
        remote_options,
        rsync_path,
        protect_args,
        address_mode,
        bind_address: bind_address_raw,
        archive,
        delete_mode,
        delete_excluded,
        backup,
        backup_dir,
        backup_suffix,
        checksum,
        checksum_choice,
        checksum_choice_arg,
        checksum_seed,
        size_only,
        ignore_existing,
        ignore_missing_args,
        update,
        remainder: raw_remainder,
        bwlimit,
        max_delete,
        min_size,
        max_size,
        modify_window,
        compress: compress_flag,
        no_compress,
        compress_level,
        skip_compress,
        owner,
        group,
        chown,
        chmod,
        perms,
        super_mode,
        times,
        omit_dir_times,
        omit_link_times,
        acls,
        excludes,
        includes,
        compare_destinations,
        copy_destinations,
        link_destinations,
        exclude_from,
        include_from,
        filters,
        cvs_exclude,
        rsync_filter_shortcuts,
        files_from,
        from0,
        info,
        debug,
        numeric_ids,
        hard_links,
        sparse,
        copy_links,
        copy_dirlinks,
        copy_unsafe_links,
        keep_dirlinks,
        safe_links,
        devices,
        specials,
        relative,
        one_file_system,
        implied_dirs,
        mkpath,
        prune_empty_dirs,
        verbosity,
        progress: initial_progress,
        name_level: initial_name_level,
        name_overridden: initial_name_overridden,
        stats,
        partial,
        preallocate,
        delay_updates,
        partial_dir,
        temp_dir,
        link_dests,
        remove_source_files,
        inplace,
        append,
        append_verify,
        msgs_to_stderr,
        itemize_changes,
        whole_file,
        xattrs,
        no_motd,
        password_file,
        protocol,
        timeout,
        contimeout,
        out_format,
    } = parsed;

    let password_file = password_file.map(PathBuf::from);
    let human_readable_setting = human_readable;
    let human_readable_mode = human_readable_setting.unwrap_or(HumanReadableMode::Disabled);
    let human_readable_enabled = human_readable_mode.is_enabled();

    if password_file
        .as_deref()
        .is_some_and(|path| path == Path::new("-"))
        && files_from
            .iter()
            .any(|entry| entry.as_os_str() == OsStr::new("-"))
    {
        let message = rsync_error!(
            1,
            "--password-file=- cannot be combined with --files-from=- because both read from standard input"
        )
        .with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let fallback = message.to_string();
            let _ = writeln!(stderr.writer_mut(), "{fallback}");
        }
        return 1;
    }
    let desired_protocol = match protocol {
        Some(value) => match parse_protocol_version_arg(value.as_os_str()) {
            Ok(version) => Some(version),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        },
        None => None,
    };

    let timeout_setting = match timeout {
        Some(value) => match parse_timeout_argument(value.as_os_str()) {
            Ok(setting) => setting,
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        },
        None => TransferTimeout::Default,
    };

    let connect_timeout_setting = match contimeout {
        Some(value) => match parse_timeout_argument(value.as_os_str()) {
            Ok(setting) => setting,
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        },
        None => TransferTimeout::Default,
    };

    let mut out_format_template = match out_format.as_ref() {
        Some(value) => match parse_out_format(value.as_os_str()) {
            Ok(template) => Some(template),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        },
        None => None,
    };

    let mut fallback_out_format = out_format.clone();

    if itemize_changes {
        if fallback_out_format.is_none() {
            fallback_out_format = Some(OsString::from(ITEMIZE_CHANGES_FORMAT));
        }
        if out_format_template.is_none() {
            out_format_template = Some(
                parse_out_format(OsStr::new(ITEMIZE_CHANGES_FORMAT))
                    .expect("default itemize-changes format parses"),
            );
        }
    }

    if show_help {
        let help = render_help(program_name);
        if stdout.write_all(help.as_bytes()).is_err() {
            let _ = writeln!(stdout, "{help}");
            return 1;
        }
        return 0;
    }

    if show_version {
        let report = VersionInfoReport::for_client_brand(program_name.brand());
        let banner = report.human_readable();
        if stdout.write_all(banner.as_bytes()).is_err() {
            return 1;
        }
        return 0;
    }

    let bind_address = match bind_address_raw {
        Some(value) => match parse_bind_address_argument(&value) {
            Ok(parsed) => Some(parsed),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        },
        None => None,
    };

    let remainder = match extract_operands(raw_remainder) {
        Ok(operands) => operands,
        Err(unsupported) => {
            let message = unsupported.to_message();
            let fallback = unsupported.fallback_text();
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(stderr.writer_mut(), "{fallback}");
            }
            return 1;
        }
    };

    let mut compress = compress_flag;
    let mut progress_setting = initial_progress;
    let mut stats = stats;
    let mut name_level = initial_name_level;
    let mut name_overridden = initial_name_overridden;

    let mut debug_flags_list = Vec::new();

    if !info.is_empty() {
        match parse_info_flags(&info) {
            Ok(settings) => {
                if settings.help_requested {
                    if stdout.write_all(INFO_HELP_TEXT.as_bytes()).is_err() {
                        let _ = write!(stderr.writer_mut(), "{INFO_HELP_TEXT}");
                        return 1;
                    }
                    return 0;
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
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        }
    }

    if !debug.is_empty() {
        match parse_debug_flags(&debug) {
            Ok(settings) => {
                if settings.help_requested {
                    if stdout.write_all(DEBUG_HELP_TEXT.as_bytes()).is_err() {
                        let _ = write!(stderr.writer_mut(), "{DEBUG_HELP_TEXT}");
                        return 1;
                    }
                    return 0;
                }

                debug_flags_list = settings.flags;
            }
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        }
    }

    let progress_mode = progress_setting.resolved();

    let bandwidth_limit = match bwlimit.as_ref() {
        Some(BandwidthArgument::Limit(value)) => match parse_bandwidth_limit(value.as_os_str()) {
            Ok(limit) => limit,
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        },
        Some(BandwidthArgument::Disabled) | None => None,
    };

    let fallback_bwlimit = match (bandwidth_limit.as_ref(), bwlimit.as_ref()) {
        (Some(limit), _) => Some(limit.fallback_argument()),
        (None, Some(BandwidthArgument::Limit(_))) => {
            Some(BandwidthLimit::fallback_unlimited_argument())
        }
        (None, Some(BandwidthArgument::Disabled)) => {
            Some(BandwidthLimit::fallback_unlimited_argument())
        }
        (None, None) => None,
    };

    let max_delete_limit = match max_delete {
        Some(ref value) => match parse_max_delete_argument(value.as_os_str()) {
            Ok(limit) => Some(limit),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        },
        None => None,
    };

    let min_size_limit = match min_size.as_ref() {
        Some(value) => match parse_size_limit_argument(value.as_os_str(), "--min-size") {
            Ok(limit) => Some(limit),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        },
        None => None,
    };

    let max_size_limit = match max_size.as_ref() {
        Some(value) => match parse_size_limit_argument(value.as_os_str(), "--max-size") {
            Ok(limit) => Some(limit),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        },
        None => None,
    };

    let modify_window_setting = match modify_window.as_ref() {
        Some(value) => match parse_modify_window_argument(value.as_os_str()) {
            Ok(window) => Some(window),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        },
        None => None,
    };

    let compress_level_setting = match compress_level {
        Some(ref value) => match parse_compress_level(value.as_os_str()) {
            Ok(setting) => Some(setting),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        },
        None => None,
    };

    let mut compression_level_override = None;
    if let Some(setting) = compress_level_setting {
        match setting {
            CompressLevelArg::Disable => {
                compress = false;
            }
            CompressLevelArg::Level(level) => {
                if !no_compress {
                    compress = true;
                    compression_level_override = Some(CompressionLevel::precise(level));
                }
            }
        }
    }

    let compress_disabled =
        no_compress || matches!(compress_level_setting, Some(CompressLevelArg::Disable));
    let compress_level_cli = match (compress_level_setting, compress_disabled) {
        (Some(CompressLevelArg::Level(level)), false) => {
            Some(OsString::from(level.get().to_string()))
        }
        (Some(CompressLevelArg::Disable), _) => Some(OsString::from("0")),
        _ => None,
    };

    let skip_compress_list = if let Some(value) = skip_compress.as_ref() {
        match parse_skip_compress_list(value.as_os_str()) {
            Ok(list) => Some(list),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        }
    } else {
        match skip_compress_from_env("RSYNC_SKIP_COMPRESS") {
            Ok(value) => value,
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        }
    };

    let mut compression_setting = CompressionSetting::default();
    if let Some(ref value) = compress_level {
        match parse_compress_level_argument(value.as_os_str()) {
            Ok(setting) => {
                compression_setting = setting;
                compress = !setting.is_disabled();
            }
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{}", fallback);
                }
                return 1;
            }
        }
    }

    let numeric_ids_option = numeric_ids;
    let whole_file_option = whole_file;

    #[allow(unused_variables)]
    let preserve_acls = acls.unwrap_or(false);

    #[cfg(not(feature = "acl"))]
    if preserve_acls {
        let message =
            rsync_error!(1, "POSIX ACLs are not supported on this client").with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(
                stderr.writer_mut(),
                "POSIX ACLs are not supported on this client"
            );
        }
        return 1;
    }

    let parsed_chown = match chown.as_ref() {
        Some(value) => match parse_chown_argument(value.as_os_str()) {
            Ok(parsed) => Some(parsed),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{}", fallback);
                }
                return 1;
            }
        },
        None => None,
    };

    let owner_override_value = parsed_chown.as_ref().and_then(|value| value.owner);
    let group_override_value = parsed_chown.as_ref().and_then(|value| value.group);
    let chown_spec = parsed_chown.as_ref().map(|value| value.spec.clone());

    #[cfg(not(feature = "xattr"))]
    if xattrs.unwrap_or(false) {
        let message = rsync_error!(1, "extended attributes are not supported on this client")
            .with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(
                stderr.writer_mut(),
                "extended attributes are not supported on this client"
            );
        }
        return 1;
    }

    let mut file_list_operands = match load_file_list_operands(&files_from, from0) {
        Ok(operands) => operands,
        Err(message) => {
            if write_message(&message, stderr).is_err() {
                let fallback = message.to_string();
                let _ = writeln!(stderr.writer_mut(), "{}", fallback);
            }
            return 1;
        }
    };

    if file_list_operands.is_empty() {
        let module_list_port = daemon_port.unwrap_or(ModuleListRequest::DEFAULT_PORT);
        match ModuleListRequest::from_operands_with_port(&remainder, module_list_port) {
            Ok(Some(request)) => {
                let request = if let Some(protocol) = desired_protocol {
                    request.with_protocol(protocol)
                } else {
                    request
                };
                let password_override = match load_optional_password(password_file.as_deref()) {
                    Ok(secret) => secret,
                    Err(message) => {
                        if write_message(&message, stderr).is_err() {
                            let fallback = message.to_string();
                            let _ = writeln!(stderr.writer_mut(), "{}", fallback);
                        }
                        return 1;
                    }
                };

                let list_options = ModuleListOptions::default()
                    .suppress_motd(no_motd)
                    .with_address_mode(address_mode)
                    .with_bind_address(bind_address.as_ref().map(|addr| addr.socket()))
                    .with_connect_program(connect_program.clone());
                return match run_module_list_with_password_and_options(
                    request,
                    list_options,
                    password_override,
                    timeout_setting,
                    connect_timeout_setting,
                ) {
                    Ok(list) => {
                        if render_module_list(stdout, stderr.writer_mut(), &list, no_motd).is_err()
                        {
                            1
                        } else {
                            0
                        }
                    }
                    Err(error) => {
                        if write_message(error.message(), stderr).is_err() {
                            let _ = writeln!(
                                stderr.writer_mut(),
                                "rsync error: daemon functionality is unavailable in this build (code {})",
                                error.exit_code()
                            );
                        }
                        error.exit_code()
                    }
                };
            }
            Ok(None) => {}
            Err(error) => {
                if write_message(error.message(), stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", error);
                }
                return error.exit_code();
            }
        }
    }

    let files_from_used = !files_from.is_empty();
    let implied_dirs_option = implied_dirs;
    let implied_dirs = implied_dirs_option.unwrap_or(true);
    let requires_remote_fallback = transfer_requires_remote(&remainder, &file_list_operands);
    let fallback_required = requires_remote_fallback;

    let append_for_fallback = if append_verify { Some(true) } else { append };
    let fallback_one_file_system = one_file_system;

    let fallback_args = if fallback_required {
        let mut fallback_info_flags = info.clone();
        let fallback_debug_flags = debug_flags_list.clone();
        if protect_args.unwrap_or(false)
            && matches!(progress_setting, ProgressSetting::Unspecified)
            && !info_flags_include_progress(&fallback_info_flags)
        {
            fallback_info_flags.push(OsString::from("progress2"));
        }
        let delete_for_fallback =
            delete_mode.is_enabled() || delete_excluded || max_delete_limit.is_some();
        let daemon_password = match password_file.as_deref() {
            Some(path) if path == Path::new("-") => match load_password_file(path) {
                Ok(bytes) => Some(bytes),
                Err(message) => {
                    if write_message(&message, stderr).is_err() {
                        let fallback = message.to_string();
                        let _ = writeln!(stderr.writer_mut(), "{fallback}");
                    }
                    return 1;
                }
            },
            _ => None,
        };
        Some(RemoteFallbackArgs {
            dry_run,
            list_only,
            remote_shell: remote_shell.clone(),
            remote_options: remote_options.clone(),
            connect_program: connect_program.clone(),
            port: daemon_port,
            bind_address: bind_address
                .as_ref()
                .map(|address| address.raw().to_os_string()),
            protect_args,
            human_readable: human_readable_setting,
            archive,
            delete: delete_for_fallback,
            delete_mode,
            delete_excluded,
            max_delete: max_delete_limit,
            min_size: min_size.clone(),
            max_size: max_size.clone(),
            checksum,
            checksum_choice: checksum_choice_arg.clone(),
            checksum_seed,
            size_only,
            ignore_existing,
            ignore_missing_args,
            update,
            modify_window: modify_window_setting,
            compress,
            compress_disabled,
            compress_level: compress_level_cli.clone(),
            skip_compress: skip_compress.clone(),
            chown: chown_spec.clone(),
            owner,
            group,
            chmod: chmod.clone(),
            perms,
            super_mode,
            times,
            omit_dir_times,
            omit_link_times,
            numeric_ids: numeric_ids_option,
            hard_links,
            copy_links,
            copy_dirlinks,
            copy_unsafe_links,
            keep_dirlinks,
            safe_links,
            sparse,
            devices,
            specials,
            relative,
            one_file_system: fallback_one_file_system,
            implied_dirs: implied_dirs_option,
            mkpath,
            prune_empty_dirs,
            verbosity,
            progress: progress_mode.is_some(),
            stats,
            partial,
            preallocate,
            delay_updates,
            partial_dir: partial_dir.clone(),
            temp_directory: temp_dir.clone(),
            backup,
            backup_dir: backup_dir.clone().map(PathBuf::from),
            backup_suffix: backup_suffix.clone(),
            link_dests: link_dests.clone(),
            remove_source_files,
            append: append_for_fallback,
            append_verify,
            inplace,
            msgs_to_stderr,
            whole_file: whole_file_option,
            bwlimit: fallback_bwlimit.clone(),
            excludes: excludes.clone(),
            includes: includes.clone(),
            exclude_from: exclude_from.clone(),
            include_from: include_from.clone(),
            filters: filters.clone(),
            rsync_filter_shortcuts,
            compare_destinations: compare_destinations.clone(),
            copy_destinations: copy_destinations.clone(),
            link_destinations: link_destinations.clone(),
            cvs_exclude,
            info_flags: fallback_info_flags,
            debug_flags: fallback_debug_flags,
            files_from_used,
            file_list_entries: file_list_operands.clone(),
            from0,
            password_file: password_file.clone(),
            daemon_password,
            protocol: desired_protocol,
            timeout: timeout_setting,
            connect_timeout: connect_timeout_setting,
            out_format: fallback_out_format.clone(),
            no_motd,
            address_mode,
            fallback_binary: None,
            rsync_path: rsync_path.clone(),
            remainder: remainder.clone(),
            #[cfg(feature = "acl")]
            acls,
            #[cfg(feature = "xattr")]
            xattrs,
            itemize_changes,
        })
    } else {
        None
    };

    let numeric_ids = numeric_ids_option.unwrap_or(false);

    if !fallback_required {
        if rsync_path.is_some() {
            let message = rsync_error!(
                1,
                "the --rsync-path option may only be used with remote connections"
            )
            .with_role(Role::Client);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "the --rsync-path option may only be used with remote connections"
                );
            }
            return 1;
        }

        if !remote_options.is_empty() {
            let message = rsync_error!(
                1,
                "the --remote-option option may only be used with remote connections"
            )
            .with_role(Role::Client);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "the --remote-option option may only be used with remote connections"
                );
            }
            return 1;
        }

        if desired_protocol.is_some() {
            let message = rsync_error!(
                1,
                "the --protocol option may only be used when accessing an rsync daemon"
            )
            .with_role(Role::Client);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "the --protocol option may only be used when accessing an rsync daemon"
                );
            }
            return 1;
        }

        if password_file.is_some() {
            let message = rsync_error!(
                1,
                "the --password-file option may only be used when accessing an rsync daemon"
            )
            .with_role(Role::Client);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "the --password-file option may only be used when accessing an rsync daemon"
                );
            }
            return 1;
        }

        if connect_program.is_some() {
            let message = rsync_error!(
                1,
                "the --connect-program option may only be used when accessing an rsync daemon"
            )
            .with_role(Role::Client);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "the --connect-program option may only be used when accessing an rsync daemon"
                );
            }
            return 1;
        }
    }

    let mut transfer_operands = Vec::with_capacity(file_list_operands.len() + remainder.len());
    transfer_operands.append(&mut file_list_operands);
    transfer_operands.extend(remainder);

    if transfer_operands.is_empty() {
        let message = rsync_error!(
            1,
            "missing source operands: supply at least one source and a destination"
        )
        .with_role(Role::Client);
        if write_message(&message, stderr).is_err() {
            let fallback = message.to_string();
            let _ = writeln!(stderr.writer_mut(), "{fallback}");
        }

        let usage = render_help(program_name);
        if writeln!(stderr.writer_mut(), "{usage}").is_err() {
            let _ = writeln!(stdout, "{usage}");
        }
        return 1;
    }

    let preserve_owner = if parsed_chown
        .as_ref()
        .and_then(|value| value.owner)
        .is_some()
    {
        true
    } else if let Some(value) = owner {
        value
    } else if super_mode == Some(true) {
        true
    } else {
        archive
    };
    let preserve_group = if parsed_chown
        .as_ref()
        .and_then(|value| value.group)
        .is_some()
    {
        true
    } else if let Some(value) = group {
        value
    } else if super_mode == Some(true) {
        true
    } else {
        archive
    };
    let preserve_permissions = if let Some(value) = perms {
        value
    } else if super_mode == Some(true) {
        true
    } else {
        archive
    };
    let preserve_times = times.unwrap_or(archive);
    let omit_dir_times_setting = omit_dir_times.unwrap_or(false);
    let omit_link_times_setting = omit_link_times.unwrap_or(false);
    let preserve_devices = devices.unwrap_or(archive);
    let preserve_specials = specials.unwrap_or(archive);
    let preserve_hard_links = hard_links.unwrap_or(false);
    let sparse = sparse.unwrap_or(false);
    let copy_links = copy_links.unwrap_or(false);
    let copy_unsafe_links = copy_unsafe_links.unwrap_or(false);
    let keep_dirlinks_flag = keep_dirlinks.unwrap_or(false);
    let relative = relative.unwrap_or(false);
    let one_file_system = fallback_one_file_system.unwrap_or(false);

    let mut chmod_modifiers: Option<ChmodModifiers> = None;
    for spec in &chmod {
        let spec_text = spec.to_string_lossy();
        let trimmed = spec_text.trim();
        match ChmodModifiers::parse(trimmed) {
            Ok(parsed) => {
                if let Some(existing) = &mut chmod_modifiers {
                    existing.extend(parsed);
                } else {
                    chmod_modifiers = Some(parsed);
                }
            }
            Err(error) => {
                let formatted = format!(
                    "failed to parse --chmod specification '{}': {}",
                    spec_text, error
                );
                let message = rsync_error!(1, formatted).with_role(Role::Client);
                if write_message(&message, stderr).is_err() {
                    let fallback = message.to_string();
                    let _ = writeln!(stderr.writer_mut(), "{fallback}");
                }
                return 1;
            }
        }
    }

    let mut builder = ClientConfig::builder()
        .transfer_args(transfer_operands)
        .address_mode(address_mode)
        .connect_program(connect_program.clone())
        .bind_address(bind_address.clone())
        .dry_run(dry_run)
        .list_only(list_only)
        .delete(delete_mode.is_enabled() || delete_excluded || max_delete_limit.is_some())
        .delete_excluded(delete_excluded)
        .max_delete(max_delete_limit)
        .min_file_size(min_size_limit)
        .max_file_size(max_size_limit)
        .backup(backup)
        .backup_directory(backup_dir.clone().map(PathBuf::from))
        .backup_suffix(backup_suffix.clone())
        .bandwidth_limit(bandwidth_limit)
        .compression_setting(compression_setting)
        .compress(compress)
        .compression_level(compression_level_override)
        .owner(preserve_owner)
        .owner_override(owner_override_value)
        .group(preserve_group)
        .group_override(group_override_value)
        .chmod(chmod_modifiers.clone())
        .permissions(preserve_permissions)
        .times(preserve_times)
        .modify_window(modify_window_setting)
        .omit_dir_times(omit_dir_times_setting)
        .omit_link_times(omit_link_times_setting)
        .devices(preserve_devices)
        .specials(preserve_specials)
        .checksum(checksum)
        .checksum_seed(checksum_seed)
        .size_only(size_only)
        .ignore_existing(ignore_existing)
        .ignore_missing_args(ignore_missing_args)
        .update(update)
        .numeric_ids(numeric_ids)
        .hard_links(preserve_hard_links)
        .sparse(sparse)
        .copy_links(copy_links)
        .copy_dirlinks(copy_dirlinks)
        .copy_unsafe_links(copy_unsafe_links)
        .keep_dirlinks(keep_dirlinks_flag)
        .safe_links(safe_links)
        .relative_paths(relative)
        .one_file_system(one_file_system)
        .implied_dirs(implied_dirs)
        .human_readable(human_readable_enabled)
        .mkpath(mkpath)
        .prune_empty_dirs(prune_empty_dirs.unwrap_or(false))
        .verbosity(verbosity)
        .progress(progress_mode.is_some())
        .stats(stats)
        .debug_flags(debug_flags_list.clone())
        .partial(partial)
        .preallocate(preallocate)
        .delay_updates(delay_updates)
        .partial_directory(partial_dir.clone())
        .temp_directory(temp_dir.clone())
        .delay_updates(delay_updates)
        .extend_link_dests(link_dests.clone())
        .remove_source_files(remove_source_files)
        .inplace(inplace.unwrap_or(false))
        .append(append.unwrap_or(false))
        .append_verify(append_verify)
        .whole_file(whole_file_option.unwrap_or(true))
        .timeout(timeout_setting)
        .connect_timeout(connect_timeout_setting);

    if let Some(choice) = checksum_choice {
        builder = builder.checksum_choice(choice);
    }

    for path in &compare_destinations {
        builder = builder.compare_destination(PathBuf::from(path));
    }

    for path in &copy_destinations {
        builder = builder.copy_destination(PathBuf::from(path));
    }

    for path in &link_destinations {
        builder = builder.link_destination(PathBuf::from(path));
    }
    #[cfg(feature = "acl")]
    {
        builder = builder.acls(preserve_acls);
    }
    #[cfg(feature = "xattr")]
    {
        builder = builder.xattrs(xattrs.unwrap_or(false));
    }

    if let Some(list) = skip_compress_list {
        builder = builder.skip_compress(list);
    }

    builder = match delete_mode {
        DeleteMode::Before => builder.delete_before(true),
        DeleteMode::After => builder.delete_after(true),
        DeleteMode::Delay => builder.delete_delay(true),
        DeleteMode::During | DeleteMode::Disabled => builder,
    };

    let force_event_collection = itemize_changes
        || out_format_template.is_some()
        || !matches!(name_level, NameOutputLevel::Disabled);
    builder = builder.force_event_collection(force_event_collection);

    let mut filter_rules = Vec::new();
    if let Err(message) =
        append_filter_rules_from_files(&mut filter_rules, &exclude_from, FilterRuleKind::Exclude)
    {
        if write_message(&message, stderr).is_err() {
            let fallback = message.to_string();
            let _ = writeln!(stderr.writer_mut(), "{}", fallback);
        }
        return 1;
    }
    filter_rules.extend(
        excludes
            .into_iter()
            .map(|pattern| FilterRuleSpec::exclude(os_string_to_pattern(pattern))),
    );
    if let Err(message) =
        append_filter_rules_from_files(&mut filter_rules, &include_from, FilterRuleKind::Include)
    {
        if write_message(&message, stderr).is_err() {
            let fallback = message.to_string();
            let _ = writeln!(stderr.writer_mut(), "{}", fallback);
        }
        return 1;
    }
    filter_rules.extend(
        includes
            .into_iter()
            .map(|pattern| FilterRuleSpec::include(os_string_to_pattern(pattern))),
    );
    if cvs_exclude {
        if let Err(message) = append_cvs_exclude_rules(&mut filter_rules) {
            if write_message(&message, stderr).is_err() {
                let fallback = message.to_string();
                let _ = writeln!(stderr.writer_mut(), "{}", fallback);
            }
            return 1;
        }
    }

    let mut merge_stack = HashSet::new();
    let merge_base = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for filter in &filters {
        match parse_filter_directive(filter.as_os_str()) {
            Ok(FilterDirective::Rule(spec)) => filter_rules.push(spec),
            Ok(FilterDirective::Merge(directive)) => {
                let effective_options =
                    merge_directive_options(&DirMergeOptions::default(), &directive);
                let directive = directive.with_options(effective_options);
                if let Err(message) = apply_merge_directive(
                    directive,
                    merge_base.as_path(),
                    &mut filter_rules,
                    &mut merge_stack,
                ) {
                    if write_message(&message, stderr).is_err() {
                        let _ = writeln!(stderr.writer_mut(), "{}", message);
                    }
                    return 1;
                }
            }
            Ok(FilterDirective::Clear) => filter_rules.clear(),
            Err(message) => {
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(stderr.writer_mut(), "{}", message);
                }
                return 1;
            }
        }
    }
    if !filter_rules.is_empty() {
        builder = builder.extend_filter_rules(filter_rules);
    }

    let config = builder.build();

    if let Some(args) = fallback_args {
        let outcome = {
            let mut stderr_writer = stderr.writer_mut();
            run_client_or_fallback(
                config,
                None,
                Some(RemoteFallbackContext::new(stdout, &mut stderr_writer, args)),
            )
        };

        return match outcome {
            Ok(ClientOutcome::Fallback(summary)) => summary.exit_code(),
            Ok(ClientOutcome::Local(_)) => {
                unreachable!("local outcome returned without fallback context")
            }
            Err(error) => {
                if write_message(error.message(), stderr).is_err() {
                    let fallback = error.message().to_string();
                    let _ = writeln!(stderr.writer_mut(), "{}", fallback);
                }
                error.exit_code()
            }
        };
    }

    let mut live_progress = if let Some(mode) = progress_mode {
        Some(with_output_writer(
            stdout,
            stderr,
            msgs_to_stderr,
            |writer| LiveProgress::new(writer, mode, human_readable_mode),
        ))
    } else {
        None
    };

    let result = {
        let observer = live_progress
            .as_mut()
            .map(|observer| observer as &mut dyn ClientProgressObserver);
        run_client_or_fallback::<io::Sink, io::Sink>(config, observer, None)
    };

    match result {
        Ok(ClientOutcome::Local(summary)) => {
            let summary = *summary;
            let progress_rendered_live = live_progress.as_ref().is_some_and(LiveProgress::rendered);

            if let Some(observer) = live_progress {
                if let Err(error) = observer.finish() {
                    let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                        writeln!(writer, "warning: failed to render progress output: {error}")
                    });
                }
            }

            let out_format_context = OutFormatContext::default();
            if let Err(error) = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                emit_transfer_summary(
                    &summary,
                    verbosity,
                    progress_mode,
                    stats,
                    progress_rendered_live,
                    list_only,
                    out_format_template.as_ref(),
                    &out_format_context,
                    name_level,
                    name_overridden,
                    human_readable_mode,
                    writer,
                )
            }) {
                let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                    writeln!(
                        writer,
                        "warning: failed to render transfer summary: {error}"
                    )
                });
            }
            0
        }
        Ok(ClientOutcome::Fallback(_)) => {
            unreachable!("fallback outcome returned without fallback args")
        }
        Err(error) => {
            if let Some(observer) = live_progress {
                if let Err(err) = observer.finish() {
                    let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                        writeln!(writer, "warning: failed to render progress output: {err}")
                    });
                }
            }

            if write_message(error.message(), stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "rsync error: client functionality is unavailable in this build (code 1)",
                );
            }
            error.exit_code()
        }
    }
}

/// Converts a numeric exit code into an [`std::process::ExitCode`].
#[must_use]
pub fn exit_code_from(status: i32) -> std::process::ExitCode {
    let clamped = status.clamp(0, MAX_EXIT_CODE);
    std::process::ExitCode::from(clamped as u8)
}

fn parse_protocol_version_arg(value: &OsStr) -> Result<ProtocolVersion, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    match ProtocolVersion::from_str(text.as_ref()) {
        Ok(version) => Ok(version),
        Err(error) => {
            let supported = supported_protocols_list();
            let mut detail = match error.kind() {
                ParseProtocolVersionErrorKind::Empty => {
                    "protocol value must not be empty".to_string()
                }
                ParseProtocolVersionErrorKind::InvalidDigit => {
                    "protocol version must be an unsigned integer".to_string()
                }
                ParseProtocolVersionErrorKind::Negative => {
                    "protocol version cannot be negative".to_string()
                }
                ParseProtocolVersionErrorKind::Overflow => {
                    "protocol version value exceeds 255".to_string()
                }
                ParseProtocolVersionErrorKind::UnsupportedRange(value) => {
                    let (oldest, newest) = ProtocolVersion::supported_range_bounds();
                    format!(
                        "protocol version {} is outside the supported range {}-{}",
                        value, oldest, newest
                    )
                }
            };
            if !detail.is_empty() {
                detail.push_str("; ");
            }
            detail.push_str(&format!("supported protocols are {}", supported));

            Err(rsync_error!(
                1,
                format!("invalid protocol version '{}': {}", display, detail)
            )
            .with_role(Role::Client))
        }
    }
}

fn supported_protocols_list() -> &'static str {
    ProtocolVersion::supported_protocol_numbers_display()
}

fn parse_timeout_argument(value: &OsStr) -> Result<TransferTimeout, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(rsync_error!(1, "timeout value must not be empty").with_role(Role::Client));
    }

    if trimmed.starts_with('-') {
        return Err(rsync_error!(
            1,
            format!(
                "invalid timeout '{}': timeout must be non-negative",
                display
            )
        )
        .with_role(Role::Client));
    }

    let normalized = trimmed.strip_prefix('+').unwrap_or(trimmed);

    match normalized.parse::<u64>() {
        Ok(0) => Ok(TransferTimeout::Disabled),
        Ok(value) => Ok(TransferTimeout::Seconds(
            NonZeroU64::new(value).expect("non-zero ensured"),
        )),
        Err(error) => {
            let detail = match error.kind() {
                IntErrorKind::InvalidDigit => "timeout must be an unsigned integer",
                IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => {
                    "timeout value exceeds the supported range"
                }
                IntErrorKind::Empty => "timeout value must not be empty",
                _ => "timeout value is invalid",
            };
            Err(
                rsync_error!(1, format!("invalid timeout '{}': {}", display, detail))
                    .with_role(Role::Client),
            )
        }
    }
}

pub(crate) fn parse_human_readable_level(value: &OsStr) -> Result<HumanReadableMode, clap::Error> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::InvalidValue,
            "human-readable level must not be empty",
        ));
    }

    match trimmed {
        "0" => Ok(HumanReadableMode::Disabled),
        "1" => Ok(HumanReadableMode::Enabled),
        "2" => Ok(HumanReadableMode::Combined),
        _ => Err(clap::Error::raw(
            clap::error::ErrorKind::InvalidValue,
            format!(
                "invalid human-readable level '{}': expected 0, 1, or 2",
                display
            ),
        )),
    }
}

fn parse_max_delete_argument(value: &OsStr) -> Result<u64, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(rsync_error!(1, "--max-delete value must not be empty").with_role(Role::Client));
    }

    if trimmed.starts_with('-') {
        return Err(rsync_error!(
            1,
            format!(
                "invalid --max-delete '{}': deletion limit must be non-negative",
                display
            )
        )
        .with_role(Role::Client));
    }

    let normalized = trimmed.strip_prefix('+').unwrap_or(trimmed);

    match normalized.parse::<u64>() {
        Ok(value) => Ok(value),
        Err(error) => {
            let detail = match error.kind() {
                IntErrorKind::InvalidDigit => "deletion limit must be an unsigned integer",
                IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => {
                    "deletion limit exceeds the supported range"
                }
                IntErrorKind::Empty => "--max-delete value must not be empty",
                _ => "deletion limit is invalid",
            };
            Err(
                rsync_error!(1, format!("invalid --max-delete '{}': {}", display, detail))
                    .with_role(Role::Client),
            )
        }
    }
}

pub(crate) fn parse_checksum_seed_argument(value: &OsStr) -> Result<u32, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(
            rsync_error!(1, "--checksum-seed value must not be empty").with_role(Role::Client)
        );
    }

    if trimmed.starts_with('-') {
        return Err(rsync_error!(
            1,
            format!(
                "invalid --checksum-seed value '{}': must be non-negative",
                display
            )
        )
        .with_role(Role::Client));
    }

    let normalized = trimmed.strip_prefix('+').unwrap_or(trimmed);

    normalized.parse::<u32>().map_err(|_| {
        rsync_error!(
            1,
            format!(
                "invalid --checksum-seed value '{}': must be between 0 and {}",
                display,
                u32::MAX
            )
        )
        .with_role(Role::Client)
    })
}

fn parse_modify_window_argument(value: &OsStr) -> Result<u64, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(
            rsync_error!(1, "--modify-window value must not be empty").with_role(Role::Client)
        );
    }

    if trimmed.starts_with('-') {
        return Err(rsync_error!(
            1,
            format!(
                "invalid --modify-window '{}': window must be non-negative",
                display
            )
        )
        .with_role(Role::Client));
    }

    let normalized = trimmed.strip_prefix('+').unwrap_or(trimmed);

    match normalized.parse::<u64>() {
        Ok(value) => Ok(value),
        Err(error) => {
            let detail = match error.kind() {
                IntErrorKind::InvalidDigit => "window must be an unsigned integer",
                IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => {
                    "window exceeds the supported range"
                }
                IntErrorKind::Empty => "--modify-window value must not be empty",
                _ => "window is invalid",
            };
            Err(rsync_error!(
                1,
                format!("invalid --modify-window '{}': {}", display, detail)
            )
            .with_role(Role::Client))
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SizeParseError {
    Empty,
    Negative,
    Invalid,
    TooLarge,
}

fn parse_size_limit_argument(value: &OsStr, flag: &str) -> Result<u64, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    match parse_size_spec(trimmed) {
        Ok(limit) => Ok(limit),
        Err(SizeParseError::Empty) => {
            Err(rsync_error!(1, format!("{flag} value must not be empty")).with_role(Role::Client))
        }
        Err(SizeParseError::Negative) => Err(rsync_error!(
            1,
            format!("invalid {flag} '{display}': size must be non-negative")
        )
        .with_role(Role::Client)),
        Err(SizeParseError::Invalid) => Err(rsync_error!(
            1,
            format!(
                "invalid {flag} '{display}': expected a size with an optional K/M/G/T/P/E suffix"
            )
        )
        .with_role(Role::Client)),
        Err(SizeParseError::TooLarge) => Err(rsync_error!(
            1,
            format!("invalid {flag} '{display}': size exceeds the supported range")
        )
        .with_role(Role::Client)),
    }
}

fn parse_size_spec(text: &str) -> Result<u64, SizeParseError> {
    if text.is_empty() {
        return Err(SizeParseError::Empty);
    }

    let mut unsigned = text;
    let mut negative = false;

    if let Some(first) = unsigned.chars().next() {
        match first {
            '+' => {
                unsigned = &unsigned[first.len_utf8()..];
            }
            '-' => {
                negative = true;
                unsigned = &unsigned[first.len_utf8()..];
            }
            _ => {}
        }
    }

    if unsigned.is_empty() {
        return Err(SizeParseError::Empty);
    }

    if negative {
        return Err(SizeParseError::Negative);
    }

    let mut digits_seen = false;
    let mut decimal_seen = false;
    let mut numeric_end = unsigned.len();

    for (index, ch) in unsigned.char_indices() {
        if ch.is_ascii_digit() {
            digits_seen = true;
            continue;
        }

        if (ch == '.' || ch == ',') && !decimal_seen {
            decimal_seen = true;
            continue;
        }

        numeric_end = index;
        break;
    }

    let numeric_part = &unsigned[..numeric_end];
    let remainder = &unsigned[numeric_end..];

    if !digits_seen || numeric_part == "." || numeric_part == "," {
        return Err(SizeParseError::Invalid);
    }

    let (integer_part, fractional_part, denominator) =
        parse_decimal_components_for_size(numeric_part)?;

    let (exponent, mut remainder_after_suffix) = if remainder.is_empty() {
        (0u32, remainder)
    } else {
        let mut chars = remainder.chars();
        let ch = chars.next().unwrap();
        (
            match ch.to_ascii_lowercase() {
                'b' => 0,
                'k' => 1,
                'm' => 2,
                'g' => 3,
                't' => 4,
                'p' => 5,
                'e' => 6,
                _ => return Err(SizeParseError::Invalid),
            },
            chars.as_str(),
        )
    };

    let mut base = 1024u32;

    if !remainder_after_suffix.is_empty() {
        let bytes = remainder_after_suffix.as_bytes();
        match bytes[0] {
            b'b' | b'B' => {
                base = 1000;
                remainder_after_suffix = &remainder_after_suffix[1..];
            }
            b'i' | b'I' => {
                if bytes.len() < 2 {
                    return Err(SizeParseError::Invalid);
                }
                if matches!(bytes[1], b'b' | b'B') {
                    remainder_after_suffix = &remainder_after_suffix[2..];
                } else {
                    return Err(SizeParseError::Invalid);
                }
            }
            _ => {}
        }
    }

    if !remainder_after_suffix.is_empty() {
        return Err(SizeParseError::Invalid);
    }

    let scale = pow_u128_for_size(base, exponent)?;

    let numerator = integer_part
        .checked_mul(denominator)
        .and_then(|value| value.checked_add(fractional_part))
        .ok_or(SizeParseError::TooLarge)?;
    let product = numerator
        .checked_mul(scale)
        .ok_or(SizeParseError::TooLarge)?;

    let value = product / denominator;
    if value > u64::MAX as u128 {
        return Err(SizeParseError::TooLarge);
    }

    Ok(value as u64)
}

fn parse_decimal_components_for_size(text: &str) -> Result<(u128, u128, u128), SizeParseError> {
    let mut integer = 0u128;
    let mut fraction = 0u128;
    let mut denominator = 1u128;
    let mut saw_decimal = false;

    for ch in text.chars() {
        match ch {
            '0'..='9' => {
                let digit = u128::from(ch as u8 - b'0');
                if saw_decimal {
                    denominator = denominator
                        .checked_mul(10)
                        .ok_or(SizeParseError::TooLarge)?;
                    fraction = fraction
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or(SizeParseError::TooLarge)?;
                } else {
                    integer = integer
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or(SizeParseError::TooLarge)?;
                }
            }
            '.' | ',' => {
                if saw_decimal {
                    return Err(SizeParseError::Invalid);
                }
                saw_decimal = true;
            }
            _ => return Err(SizeParseError::Invalid),
        }
    }

    Ok((integer, fraction, denominator))
}

fn pow_u128_for_size(base: u32, exponent: u32) -> Result<u128, SizeParseError> {
    u128::from(base)
        .checked_pow(exponent)
        .ok_or(SizeParseError::TooLarge)
}

impl Default for ProgressSetting {
    fn default() -> Self {
        Self::Unspecified
    }
}

#[derive(Default)]
struct InfoFlagSettings {
    progress: ProgressSetting,
    stats: Option<bool>,
    name: Option<NameOutputLevel>,
    help_requested: bool,
}

impl InfoFlagSettings {
    fn enable_all(&mut self) {
        self.progress = ProgressSetting::PerFile;
        self.stats = Some(true);
        self.name = Some(NameOutputLevel::UpdatedAndUnchanged);
    }

    fn disable_all(&mut self) {
        self.progress = ProgressSetting::Disabled;
        self.stats = Some(false);
        self.name = Some(NameOutputLevel::Disabled);
    }

    fn apply(&mut self, token: &str, display: &str) -> Result<(), Message> {
        let lower = token.to_ascii_lowercase();
        match lower.as_str() {
            "help" => {
                self.help_requested = true;
                Ok(())
            }
            "all" | "1" => {
                self.enable_all();
                Ok(())
            }
            "none" | "0" => {
                self.disable_all();
                Ok(())
            }
            "progress" | "progress1" => {
                self.progress = ProgressSetting::PerFile;
                Ok(())
            }
            "progress2" => {
                self.progress = ProgressSetting::Overall;
                Ok(())
            }
            "progress0" | "noprogress" | "-progress" => {
                self.progress = ProgressSetting::Disabled;
                Ok(())
            }
            "stats" | "stats1" => {
                self.stats = Some(true);
                Ok(())
            }
            "stats0" | "nostats" | "-stats" => {
                self.stats = Some(false);
                Ok(())
            }
            _ if lower.starts_with("name") => {
                let level = &lower[4..];
                let parsed = if level.is_empty() || level == "1" {
                    Some(NameOutputLevel::UpdatedOnly)
                } else if level == "0" {
                    Some(NameOutputLevel::Disabled)
                } else if level.chars().all(|ch| ch.is_ascii_digit()) {
                    Some(NameOutputLevel::UpdatedAndUnchanged)
                } else {
                    None
                };

                match parsed {
                    Some(level) => {
                        self.name = Some(level);
                        Ok(())
                    }
                    None => Err(info_flag_error(display)),
                }
            }
            _ => Err(info_flag_error(display)),
        }
    }
}

fn info_flag_error(display: &str) -> Message {
    rsync_error!(
        1,
        format!(
            "invalid --info flag '{display}': supported flags are help, all, none, 0, 1, name, name0, name1, name2, progress, progress0, progress1, progress2, stats, stats0, and stats1"
        )
    )
    .with_role(Role::Client)
}

fn parse_info_flags(values: &[OsString]) -> Result<InfoFlagSettings, Message> {
    let mut settings = InfoFlagSettings::default();
    for value in values {
        let text = value.to_string_lossy();
        let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());

        if trimmed.is_empty() {
            return Err(rsync_error!(1, "--info flag must not be empty").with_role(Role::Client));
        }

        for token in trimmed.split(',') {
            let token = token.trim_matches(|ch: char| ch.is_ascii_whitespace());
            if token.is_empty() {
                return Err(
                    rsync_error!(1, "--info flag must not be empty").with_role(Role::Client)
                );
            }

            settings.apply(token, token)?;
        }
    }

    Ok(settings)
}

struct DebugFlagSettings {
    flags: Vec<OsString>,
    help_requested: bool,
}

impl DebugFlagSettings {
    fn push_flag(&mut self, flag: &str) {
        self.flags.push(OsString::from(flag));
    }
}

fn parse_debug_flags(values: &[OsString]) -> Result<DebugFlagSettings, Message> {
    let mut settings = DebugFlagSettings {
        flags: Vec::new(),
        help_requested: false,
    };

    for value in values {
        let text = value.to_string_lossy();
        let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());

        if trimmed.is_empty() {
            return Err(debug_flag_empty_error());
        }

        for token in trimmed.split(',') {
            let token = token.trim_matches(|ch: char| ch.is_ascii_whitespace());
            if token.is_empty() {
                return Err(debug_flag_empty_error());
            }

            if token.eq_ignore_ascii_case("help") {
                settings.help_requested = true;
            } else {
                settings.push_flag(token);
            }
        }
    }

    Ok(settings)
}

fn debug_flag_empty_error() -> Message {
    rsync_error!(1, "--debug flag must not be empty").with_role(Role::Client)
}

fn info_flags_include_progress(flags: &[OsString]) -> bool {
    flags.iter().any(|value| {
        value
            .to_string_lossy()
            .split(',')
            .map(|token| token.trim())
            .filter(|token| !token.is_empty())
            .any(|token| {
                let normalized = token.to_ascii_lowercase();
                let without_dash = normalized.strip_prefix('-').unwrap_or(&normalized);
                let stripped = without_dash
                    .strip_prefix("no-")
                    .or_else(|| without_dash.strip_prefix("no"))
                    .unwrap_or(without_dash);
                stripped.starts_with("progress")
            })
    })
}

const INFO_HELP_TEXT: &str = "The following --info flags are supported:\n\
    all         Enable all informational output currently implemented.\n\
    none        Disable all informational output handled by this build.\n\
    name        Mention updated file and directory names.\n\
    name2       Mention updated and unchanged file and directory names.\n\
    name0       Disable file and directory name output.\n\
    progress    Enable per-file progress updates.\n\
    progress2   Enable overall transfer progress.\n\
    progress0   Disable progress reporting.\n\
    stats       Enable transfer statistics.\n\
    stats0      Disable transfer statistics.\n\
Flags may also be written with 'no' prefixes (for example, --info=noprogress).\n";

const DEBUG_HELP_TEXT: &str = "The following --debug flags are supported:\n\
    all         Enable all diagnostic categories currently implemented.\n\
    none        Disable diagnostic output.\n\
    checksum    Trace checksum calculations and verification.\n\
    deltas      Trace delta-transfer generation and token handling.\n\
    events      Trace file-list discovery and generator events.\n\
    fs          Trace filesystem metadata operations.\n\
    io          Trace I/O buffering and transport exchanges.\n\
    socket      Trace socket setup, negotiation, and pacing decisions.\n\
Flags may be prefixed with 'no' or '-' to disable a category. Multiple flags\n\
may be combined by separating them with commas.\n";

#[derive(Debug)]
struct UnsupportedOption {
    option: OsString,
}

impl UnsupportedOption {
    fn new(option: OsString) -> Self {
        Self { option }
    }

    fn to_message(&self) -> Message {
        let option = self.option.to_string_lossy();
        let text = format!(
            "unsupported option '{}': this build currently supports only {}",
            option, SUPPORTED_OPTIONS_LIST
        );
        rsync_error!(1, text).with_role(Role::Client)
    }

    fn fallback_text(&self) -> String {
        format!(
            "unsupported option '{}': this build currently supports only {}",
            self.option.to_string_lossy(),
            SUPPORTED_OPTIONS_LIST
        )
    }
}

fn is_option(argument: &OsStr) -> bool {
    let text = argument.to_string_lossy();
    let mut chars = text.chars();
    matches!(chars.next(), Some('-')) && chars.next().is_some()
}

fn extract_operands(arguments: Vec<OsString>) -> Result<Vec<OsString>, UnsupportedOption> {
    let mut operands = Vec::new();
    let mut accept_everything = false;

    for argument in arguments {
        if !accept_everything {
            if argument == "--" {
                accept_everything = true;
                continue;
            }

            if is_option(argument.as_os_str()) {
                return Err(UnsupportedOption::new(argument));
            }
        }

        operands.push(argument);
    }

    Ok(operands)
}

fn parse_bind_address_argument(value: &OsStr) -> Result<BindAddress, Message> {
    if value.is_empty() {
        return Err(rsync_error!(1, "--address requires a non-empty value").with_role(Role::Client));
    }

    let text = value.to_string_lossy();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(rsync_error!(1, "--address requires a non-empty value").with_role(Role::Client));
    }

    match resolve_bind_address(trimmed) {
        Ok(socket) => Ok(BindAddress::new(value.to_os_string(), socket)),
        Err(error) => {
            let formatted = format!("failed to resolve --address value '{}': {}", trimmed, error);
            Err(rsync_error!(1, formatted).with_role(Role::Client))
        }
    }
}

fn resolve_bind_address(text: &str) -> io::Result<SocketAddr> {
    if let Ok(ip) = text.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, 0));
    }

    let candidate = if text.starts_with('[') {
        format!("{text}:0")
    } else if text.contains(':') {
        format!("[{text}]:0")
    } else {
        format!("{text}:0")
    };

    let mut resolved = candidate.to_socket_addrs()?;
    resolved
        .next()
        .map(|addr| SocketAddr::new(addr.ip(), 0))
        .ok_or_else(|| {
            io::Error::new(
                ErrorKind::AddrNotAvailable,
                "address resolution returned no results",
            )
        })
}

/// Loads operands referenced by `--files-from` arguments.
///
/// When `zero_terminated` is `false`, the reader treats lines beginning with `#`
/// or `;` as comments, matching upstream rsync. Supplying `--from0` disables the
/// comment semantics so entries can legitimately start with those bytes.
fn load_file_list_operands(
    files: &[OsString],
    zero_terminated: bool,
) -> Result<Vec<OsString>, Message> {
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    let mut stdin_handle: Option<io::Stdin> = None;

    for path in files {
        if path.as_os_str() == OsStr::new("-") {
            let stdin = stdin_handle.get_or_insert_with(io::stdin);
            let mut reader = stdin.lock();
            read_file_list_from_reader(&mut reader, zero_terminated, &mut entries).map_err(
                |error| {
                    rsync_error!(
                        1,
                        format!("failed to read file list from standard input: {error}")
                    )
                    .with_role(Role::Client)
                },
            )?;
            continue;
        }

        let path_buf = PathBuf::from(path);
        let display = path_buf.display().to_string();
        let file = File::open(&path_buf).map_err(|error| {
            rsync_error!(
                1,
                format!("failed to read file list '{}': {}", display, error)
            )
            .with_role(Role::Client)
        })?;
        let mut reader = BufReader::new(file);
        read_file_list_from_reader(&mut reader, zero_terminated, &mut entries).map_err(
            |error| {
                rsync_error!(
                    1,
                    format!("failed to read file list '{}': {}", display, error)
                )
                .with_role(Role::Client)
            },
        )?;
    }

    Ok(entries)
}

fn read_file_list_from_reader<R: BufRead>(
    reader: &mut R,
    zero_terminated: bool,
    entries: &mut Vec<OsString>,
) -> io::Result<()> {
    if zero_terminated {
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            let read = reader.read_until(b'\0', &mut buffer)?;
            if read == 0 {
                break;
            }

            if buffer.last() == Some(&b'\0') {
                buffer.pop();
            }

            push_file_list_entry(&buffer, entries);
        }
        return Ok(());
    }

    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let bytes_read = reader.read_until(b'\n', &mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        if buffer.last() == Some(&b'\n') {
            buffer.pop();
        }
        if buffer.last() == Some(&b'\r') {
            buffer.pop();
        }

        if buffer
            .first()
            .is_some_and(|byte| matches!(byte, b'#' | b';'))
        {
            continue;
        }

        push_file_list_entry(&buffer, entries);
    }

    Ok(())
}

fn transfer_requires_remote(remainder: &[OsString], file_list_operands: &[OsString]) -> bool {
    remainder
        .iter()
        .chain(file_list_operands.iter())
        .any(|operand| operand_is_remote(operand.as_os_str()))
}

#[cfg(windows)]
fn operand_has_windows_prefix(path: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    const COLON: u16 = b':' as u16;
    const QUESTION: u16 = b'?' as u16;
    const DOT: u16 = b'.' as u16;
    const SLASH: u16 = b'/' as u16;
    const BACKSLASH: u16 = b'\\' as u16;

    fn is_ascii_alpha(unit: u16) -> bool {
        (unit >= b'a' as u16 && unit <= b'z' as u16) || (unit >= b'A' as u16 && unit <= b'Z' as u16)
    }

    fn is_separator(unit: u16) -> bool {
        unit == SLASH || unit == BACKSLASH
    }

    let units: Vec<u16> = path.encode_wide().collect();
    if units.is_empty() {
        return false;
    }

    if units.len() >= 4
        && is_separator(units[0])
        && is_separator(units[1])
        && (units[2] == QUESTION || units[2] == DOT)
        && is_separator(units[3])
    {
        return true;
    }

    if units.len() >= 2 && is_separator(units[0]) && is_separator(units[1]) {
        return true;
    }

    if units.len() >= 2 && is_ascii_alpha(units[0]) && units[1] == COLON {
        return true;
    }

    false
}

fn operand_is_remote(path: &OsStr) -> bool {
    let text = path.to_string_lossy();

    if text.starts_with("rsync://") {
        return true;
    }

    if text.contains("::") {
        return true;
    }

    if let Some(colon_index) = text.find(':') {
        #[cfg(windows)]
        if operand_has_windows_prefix(path) {
            return false;
        }

        let after = &text[colon_index + 1..];
        if after.starts_with(':') {
            return true;
        }

        #[cfg(windows)]
        {
            use std::path::{Component, Path};

            if Path::new(path)
                .components()
                .next()
                .is_some_and(|component| matches!(component, Component::Prefix(_)))
            {
                return false;
            }
        }

        let before = &text[..colon_index];
        if before.contains('/') || before.contains('\\') {
            return false;
        }

        if colon_index == 1 && before.chars().all(|ch| ch.is_ascii_alphabetic()) {
            return false;
        }

        return true;
    }

    false
}

#[cfg(all(test, windows))]
mod windows_operand_detection {
    use super::operand_is_remote;
    use std::ffi::OsStr;

    #[test]
    fn drive_letter_paths_are_local() {
        assert!(!operand_is_remote(OsStr::new(r"C:\\tmp\\file.txt")));
        assert!(!operand_is_remote(OsStr::new(r"c:relative\\path")));
    }

    #[test]
    fn extended_prefixes_are_local() {
        assert!(!operand_is_remote(OsStr::new(r"\\\\?\\C:\\tmp\\file.txt")));
        assert!(!operand_is_remote(OsStr::new(
            r"\\\\?\\UNC\\server\\share\\file.txt"
        )));
        assert!(!operand_is_remote(OsStr::new(r"\\\\.\\pipe\\rsync")));
    }

    #[test]
    fn unc_and_forward_slash_paths_are_local() {
        assert!(!operand_is_remote(OsStr::new(
            r"\\\\server\\share\\file.txt"
        )));
        assert!(!operand_is_remote(OsStr::new("//server/share/file.txt")));
    }

    #[test]
    fn remote_operands_remain_remote() {
        assert!(operand_is_remote(OsStr::new("host:path")));
        assert!(operand_is_remote(OsStr::new("user@host:path")));
        assert!(operand_is_remote(OsStr::new("host::module")));
        assert!(operand_is_remote(OsStr::new("rsync://example.com/module")));
    }
}

fn push_file_list_entry(bytes: &[u8], entries: &mut Vec<OsString>) {
    if bytes.is_empty() {
        return;
    }

    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == b'\r' {
        end -= 1;
    }

    if end > 0 {
        let trimmed = &bytes[..end];

        #[cfg(unix)]
        {
            if !trimmed.is_empty() {
                entries.push(OsString::from_vec(trimmed.to_vec()));
            }
        }

        #[cfg(not(unix))]
        {
            let text = String::from_utf8_lossy(trimmed).into_owned();
            if !text.is_empty() {
                entries.push(OsString::from(text));
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompressLevelArg {
    Disable,
    Level(NonZeroU8),
}

fn parse_compress_level(argument: &OsStr) -> Result<CompressLevelArg, Message> {
    let text = argument.to_string_lossy();
    let trimmed = text.trim();

    if trimmed.is_empty() {
        return Err(rsync_error!(1, "--compress-level={} is invalid", text).with_role(Role::Client));
    }

    match trimmed.parse::<i32>() {
        Ok(0) => Ok(CompressLevelArg::Disable),
        Ok(value @ 1..=9) => Ok(CompressLevelArg::Level(
            NonZeroU8::new(value as u8).expect("range guarantees non-zero"),
        )),
        Ok(_) => Err(
            rsync_error!(1, "--compress-level={} must be between 0 and 9", trimmed)
                .with_role(Role::Client),
        ),
        Err(_) => {
            Err(rsync_error!(1, "--compress-level={} is invalid", text).with_role(Role::Client))
        }
    }
}

fn parse_bandwidth_limit(argument: &OsStr) -> Result<Option<BandwidthLimit>, Message> {
    let text = argument.to_string_lossy();
    match BandwidthLimit::parse(&text) {
        Ok(Some(limit)) => Ok(Some(limit)),
        Ok(None) => Ok(None),
        Err(BandwidthParseError::Invalid) => {
            Err(rsync_error!(1, "--bwlimit={} is invalid", text).with_role(Role::Client))
        }
        Err(BandwidthParseError::TooSmall) => Err(rsync_error!(
            1,
            "--bwlimit={} is too small (min: 512 or 0 for unlimited)",
            text
        )
        .with_role(Role::Client)),
        Err(BandwidthParseError::TooLarge) => {
            Err(rsync_error!(1, "--bwlimit={} is too large", text).with_role(Role::Client))
        }
    }
}

pub(crate) fn parse_compress_level_argument(value: &OsStr) -> Result<CompressionSetting, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(
            rsync_error!(1, "compression level value must not be empty").with_role(Role::Client)
        );
    }

    if !trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(
            rsync_error!(
                1,
                format!(
                    "invalid compression level '{trimmed}': compression level must be an integer between 0 and 9"
                )
            )
            .with_role(Role::Client),
        );
    }

    let level: u32 = trimmed.parse().map_err(|_| {
        rsync_error!(
            1,
            format!(
                "invalid compression level '{trimmed}': compression level must be an integer between 0 and 9"
            )
        )
        .with_role(Role::Client)
    })?;

    CompressionSetting::try_from_numeric(level).map_err(|error| {
        rsync_error!(
            1,
            format!(
                "invalid compression level '{trimmed}': compression level {} is outside the supported range 0-9",
                error.level()
            )
        )
        .with_role(Role::Client)
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedChown {
    spec: OsString,
    owner: Option<uid_t>,
    group: Option<gid_t>,
}

fn parse_chown_argument(value: &OsStr) -> Result<ParsedChown, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim();

    if trimmed.is_empty() {
        return Err(
            rsync_error!(1, "--chown requires a non-empty USER and/or GROUP")
                .with_role(Role::Client),
        );
    }

    let (user_part, group_part) = match trimmed.split_once(':') {
        Some((user, group)) => (user, group),
        None => (trimmed, ""),
    };

    let owner = if user_part.is_empty() {
        None
    } else {
        Some(resolve_chown_user(user_part)?)
    };
    let group = if group_part.is_empty() {
        None
    } else {
        Some(resolve_chown_group(group_part)?)
    };

    if owner.is_none() && group.is_none() {
        return Err(rsync_error!(1, "--chown requires a user and/or group").with_role(Role::Client));
    }

    Ok(ParsedChown {
        spec: OsString::from(trimmed),
        owner,
        group,
    })
}

fn resolve_chown_user(input: &str) -> Result<uid_t, Message> {
    if let Ok(id) = input.parse::<uid_t>() {
        return Ok(id);
    }

    if let Some(uid) = crate::platform::lookup_user_by_name(input) {
        return Ok(uid);
    }

    if crate::platform::supports_user_name_lookup() {
        Err(
            rsync_error!(1, "unknown user '{}' specified for --chown", input)
                .with_role(Role::Client),
        )
    } else {
        Err(rsync_error!(
            1,
            "user name '{}' specified for --chown requires a numeric ID on this platform",
            input
        )
        .with_role(Role::Client))
    }
}

fn resolve_chown_group(input: &str) -> Result<gid_t, Message> {
    if let Ok(id) = input.parse::<gid_t>() {
        return Ok(id);
    }

    if let Some(gid) = crate::platform::lookup_group_by_name(input) {
        return Ok(gid);
    }

    if crate::platform::supports_group_name_lookup() {
        Err(
            rsync_error!(1, "unknown group '{}' specified for --chown", input)
                .with_role(Role::Client),
        )
    } else {
        Err(rsync_error!(
            1,
            "group name '{}' specified for --chown requires a numeric ID on this platform",
            input
        )
        .with_role(Role::Client))
    }
}

fn render_module_list<W: Write, E: Write>(
    stdout: &mut W,
    stderr: &mut E,
    list: &rsync_core::client::ModuleList,
    suppress_motd: bool,
) -> io::Result<()> {
    for warning in list.warnings() {
        writeln!(stderr, "@WARNING: {}", warning)?;
    }

    if !suppress_motd {
        for line in list.motd_lines() {
            writeln!(stdout, "{}", line)?;
        }
    }

    for entry in list.entries() {
        if let Some(comment) = entry.comment() {
            writeln!(stdout, "{}\t{}", entry.name(), comment)?;
        } else {
            writeln!(stdout, "{}", entry.name())?;
        }
    }
    Ok(())
}
