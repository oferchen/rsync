//! Implementation details backing [`crate::run_daemon`].
//!
//! The module hosts the listener loop, authentication helpers, and
//! connection-management utilities that were previously embedded in
//! `lib.rs`, keeping the crate root lightweight while preserving existing
//! functionality.

use dns_lookup::{lookup_addr, lookup_host};
use std::borrow::Cow;
#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::HashMap;
use std::collections::{BTreeMap, HashSet};
use std::convert::TryFrom;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use std::time::Instant;

use std::process::{ChildStdin, Command as ProcessCommand, Stdio};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use fs2::FileExt;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use clap::{Arg, ArgAction, Command, builder::OsStringValueParser};
use rsync_checksums::strong::Md5;
use rsync_core::{
    bandwidth::{
        BandwidthLimitComponents, BandwidthLimiter, BandwidthParseError, LimiterChange,
        parse_bandwidth_limit,
    },
    branding::{self, Brand, manifest},
    fallback::{CLIENT_FALLBACK_ENV, DAEMON_FALLBACK_ENV, fallback_override},
    message::{Message, Role},
    rsync_error, rsync_info, rsync_warning,
};
use rsync_logging::MessageSink;
use rsync_protocol::{
    LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonMessage, MessageCode, MessageFrame, ProtocolVersion,
    format_legacy_daemon_message, parse_legacy_daemon_message,
};

use crate::{config::DaemonConfig, error::DaemonError, systemd};

mod help;

use self::help::help_text;

/// Exit code used when daemon functionality is unavailable.
const FEATURE_UNAVAILABLE_EXIT_CODE: i32 = 1;
/// Exit code returned when socket I/O fails.
const SOCKET_IO_EXIT_CODE: i32 = 10;

/// Maximum exit code representable by a Unix process.
pub(crate) const MAX_EXIT_CODE: i32 = u8::MAX as i32;

/// Default bind address when no CLI overrides are provided.
const DEFAULT_BIND_ADDRESS: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
/// Default port used for the development daemon listener.
const DEFAULT_PORT: u16 = 873;

const BRANDED_CONFIG_ENV: &str = "OC_RSYNC_CONFIG";
const LEGACY_CONFIG_ENV: &str = "RSYNCD_CONFIG";
const BRANDED_SECRETS_ENV: &str = "OC_RSYNC_SECRETS";
const LEGACY_SECRETS_ENV: &str = "RSYNCD_SECRETS";
/// Timeout applied to accepted sockets to avoid hanging handshakes.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

/// Error payload returned to clients while daemon functionality is incomplete.
const HANDSHAKE_ERROR_PAYLOAD: &str = "@ERROR: daemon functionality is unavailable in this build";
/// Error payload returned when a configured module is requested but file serving is unavailable.
const MODULE_UNAVAILABLE_PAYLOAD: &str =
    "@ERROR: module '{module}' transfers are not yet implemented in this build";
const ACCESS_DENIED_PAYLOAD: &str = "@ERROR: access denied to module '{module}' from {addr}";
/// Error payload returned when a requested module does not exist.
const UNKNOWN_MODULE_PAYLOAD: &str = "@ERROR: Unknown module '{module}'";
/// Error payload returned when a module reaches its connection cap.
const MODULE_MAX_CONNECTIONS_PAYLOAD: &str =
    "@ERROR: max connections ({limit}) reached -- try again later";
/// Error payload returned when updating the connection lock file fails.
const MODULE_LOCK_ERROR_PAYLOAD: &str =
    "@ERROR: failed to update module connection lock; please try again later";
/// Digest algorithms advertised during the legacy daemon greeting.
const LEGACY_HANDSHAKE_DIGESTS: &[&str] = &["sha512", "sha256", "sha1", "md5", "md4"];
// Deterministic help text describing the currently supported daemon surface.
//
// The snapshot adjusts the banner, usage line, and default configuration path
// to reflect the supplied [`Brand`], ensuring invocations via compatibility
// symlinks and the canonical `oc-rsyncd` binary emit brand-appropriate help
// output.

include!("daemon/module_state.rs");

type SharedLogSink = Arc<Mutex<MessageSink<std::fs::File>>>;

include!("daemon/runtime_options.rs");

fn take_option_value<'a, I>(
    argument: &'a OsString,
    iter: &mut I,
    option: &str,
) -> Result<Option<OsString>, DaemonError>
where
    I: Iterator<Item = &'a OsString>,
{
    if argument == option {
        let value = iter
            .next()
            .cloned()
            .ok_or_else(|| missing_argument_value(option))?;
        return Ok(Some(value));
    }

    let text = argument.to_string_lossy();
    if let Some(rest) = text.strip_prefix(option) {
        if let Some(value) = rest.strip_prefix('=') {
            return Ok(Some(OsString::from(value)));
        }
    }

    Ok(None)
}

fn config_argument_present(arguments: &[OsString]) -> bool {
    for argument in arguments {
        if argument == "--config" {
            return true;
        }

        let text = argument.to_string_lossy();
        if let Some(rest) = text.strip_prefix("--config") {
            if rest.starts_with('=') {
                return true;
            }
        }
    }

    false
}

fn first_existing_path<I, P>(paths: I) -> Option<OsString>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    for candidate in paths {
        let candidate = candidate.as_ref();
        if candidate.is_file() {
            return Some(candidate.as_os_str().to_os_string());
        }
    }

    None
}

fn first_existing_config_path<I, P>(paths: I) -> Option<OsString>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    first_existing_path(paths)
}

fn environment_config_override() -> Option<OsString> {
    environment_path_override(BRANDED_CONFIG_ENV)
        .or_else(|| environment_path_override(LEGACY_CONFIG_ENV))
}

fn environment_secrets_override() -> Option<(OsString, &'static str)> {
    #[cfg(test)]
    if let Some(env) = TEST_SECRETS_ENV.with(|cell| cell.borrow().clone()) {
        if let Some(path) = env.branded.clone() {
            return Some((path, BRANDED_SECRETS_ENV));
        }

        if let Some(path) = env.legacy.clone() {
            return Some((path, LEGACY_SECRETS_ENV));
        }
    }

    if let Some(path) = environment_path_override(BRANDED_SECRETS_ENV) {
        return Some((path, BRANDED_SECRETS_ENV));
    }

    environment_path_override(LEGACY_SECRETS_ENV).map(|path| (path, LEGACY_SECRETS_ENV))
}

fn environment_path_override(name: &'static str) -> Option<OsString> {
    let value = env::var_os(name)?;
    if value.is_empty() { None } else { Some(value) }
}

fn default_config_path_if_present(brand: Brand) -> Option<OsString> {
    #[cfg(test)]
    if let Some(paths) = TEST_CONFIG_CANDIDATES.with(|cell| cell.borrow().clone()) {
        return first_existing_path(paths.iter().map(PathBuf::as_path));
    }

    first_existing_config_path(brand.config_path_candidate_strs())
}

fn default_secrets_path_if_present(brand: Brand) -> Option<OsString> {
    #[cfg(test)]
    if let Some(paths) = TEST_SECRETS_CANDIDATES.with(|cell| cell.borrow().clone()) {
        return first_existing_path(paths.iter());
    }

    first_existing_path(brand.secrets_path_candidate_strs())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ConfigDirectiveOrigin {
    path: PathBuf,
    line: usize,
}

#[derive(Debug)]
struct ParsedConfigModules {
    modules: Vec<ModuleDefinition>,
    global_refuse_options: Vec<(Vec<String>, ConfigDirectiveOrigin)>,
    motd_lines: Vec<String>,
    pid_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    reverse_lookup: Option<(bool, ConfigDirectiveOrigin)>,
    lock_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    global_bandwidth_limit: Option<(BandwidthLimitComponents, ConfigDirectiveOrigin)>,
    global_secrets_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
}

fn parse_config_modules(path: &Path) -> Result<ParsedConfigModules, DaemonError> {
    let mut stack = Vec::new();
    parse_config_modules_inner(path, &mut stack)
}

fn parse_config_modules_inner(
    path: &Path,
    stack: &mut Vec<PathBuf>,
) -> Result<ParsedConfigModules, DaemonError> {
    let canonical = path
        .canonicalize()
        .map_err(|error| config_io_error("read", path, error))?;

    if stack.iter().any(|seen| seen == &canonical) {
        return Err(config_parse_error(
            path,
            0,
            format!("recursive include detected for '{}'", canonical.display()),
        ));
    }

    let contents = fs::read_to_string(&canonical)
        .map_err(|error| config_io_error("read", &canonical, error))?;
    stack.push(canonical.clone());

    let mut modules = Vec::new();
    let mut current: Option<ModuleDefinitionBuilder> = None;
    let mut global_refuse_directives = Vec::new();
    let mut global_refuse_line: Option<usize> = None;
    let mut motd_lines = Vec::new();
    let mut pid_file: Option<(PathBuf, ConfigDirectiveOrigin)> = None;
    let mut reverse_lookup: Option<(bool, ConfigDirectiveOrigin)> = None;
    let mut lock_file: Option<(PathBuf, ConfigDirectiveOrigin)> = None;
    let mut global_bwlimit: Option<(BandwidthLimitComponents, ConfigDirectiveOrigin)> = None;
    let mut global_secrets_file: Option<(PathBuf, ConfigDirectiveOrigin)> = None;

    let result = (|| -> Result<ParsedConfigModules, DaemonError> {
        for (index, raw_line) in contents.lines().enumerate() {
            let line_number = index + 1;
            let line = raw_line.trim();

            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            if line.starts_with('[') {
                let end = line.find(']').ok_or_else(|| {
                    config_parse_error(path, line_number, "unterminated module header")
                })?;
                let name = line[1..end].trim();

                if name.is_empty() {
                    return Err(config_parse_error(
                        path,
                        line_number,
                        "module name must be non-empty",
                    ));
                }

                ensure_valid_module_name(name)
                    .map_err(|msg| config_parse_error(path, line_number, msg))?;

                let trailing = line[end + 1..].trim();
                if !trailing.is_empty() && !trailing.starts_with('#') && !trailing.starts_with(';')
                {
                    return Err(config_parse_error(
                        path,
                        line_number,
                        "unexpected characters after module header",
                    ));
                }

                if let Some(builder) = current.take() {
                    let default_secrets = global_secrets_file.as_ref().map(|(p, _)| p.as_path());
                    modules.push(builder.finish(path, default_secrets)?);
                }

                current = Some(ModuleDefinitionBuilder::new(name.to_string(), line_number));
                continue;
            }

            let (key, value) = line.split_once('=').ok_or_else(|| {
                config_parse_error(path, line_number, "expected 'key = value' directive")
            })?;
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();

            if let Some(builder) = current.as_mut() {
                match key.as_str() {
                    "path" => {
                        if value.is_empty() {
                            return Err(config_parse_error(
                                path,
                                line_number,
                                "module path directive must not be empty",
                            ));
                        }
                        builder.set_path(PathBuf::from(value), path, line_number)?;
                    }
                    "comment" => {
                        let comment = if value.is_empty() {
                            None
                        } else {
                            Some(value.to_string())
                        };
                        builder.set_comment(comment, path, line_number)?;
                    }
                    "hosts allow" => {
                        let patterns = parse_host_list(value, path, line_number, "hosts allow")?;
                        builder.set_hosts_allow(patterns, path, line_number)?;
                    }
                    "hosts deny" => {
                        let patterns = parse_host_list(value, path, line_number, "hosts deny")?;
                        builder.set_hosts_deny(patterns, path, line_number)?;
                    }
                    "auth users" => {
                        let users = parse_auth_user_list(value).map_err(|error| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid 'auth users' directive: {error}"),
                            )
                        })?;
                        builder.set_auth_users(users, path, line_number)?;
                    }
                    "secrets file" => {
                        if value.is_empty() {
                            return Err(config_parse_error(
                                path,
                                line_number,
                                "'secrets file' directive must not be empty",
                            ));
                        }
                        builder.set_secrets_file(PathBuf::from(value), path, line_number)?;
                    }
                    "bwlimit" => {
                        if value.is_empty() {
                            return Err(config_parse_error(
                                path,
                                line_number,
                                "'bwlimit' directive must not be empty",
                            ));
                        }
                        let components = parse_config_bwlimit(value, path, line_number)?;
                        builder.set_bandwidth_limit(
                            components.rate(),
                            components.burst(),
                            components.burst_specified(),
                            path,
                            line_number,
                        )?;
                    }
                    "refuse options" => {
                        let options = parse_refuse_option_list(value).map_err(|error| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid 'refuse options' directive: {error}"),
                            )
                        })?;
                        builder.set_refuse_options(options, path, line_number)?;
                    }
                    "read only" => {
                        let parsed = parse_boolean_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid boolean value '{value}' for 'read only'"),
                            )
                        })?;
                        builder.set_read_only(parsed, path, line_number)?;
                    }
                    "use chroot" => {
                        let parsed = parse_boolean_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid boolean value '{value}' for 'use chroot'"),
                            )
                        })?;
                        builder.set_use_chroot(parsed, path, line_number)?;
                    }
                    "numeric ids" => {
                        let parsed = parse_boolean_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid boolean value '{value}' for 'numeric ids'"),
                            )
                        })?;
                        builder.set_numeric_ids(parsed, path, line_number)?;
                    }
                    "list" => {
                        let parsed = parse_boolean_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid boolean value '{value}' for 'list'"),
                            )
                        })?;
                        builder.set_listable(parsed, path, line_number)?;
                    }
                    "uid" => {
                        let uid = parse_numeric_identifier(value).ok_or_else(|| {
                            config_parse_error(path, line_number, format!("invalid uid '{value}'"))
                        })?;
                        builder.set_uid(uid, path, line_number)?;
                    }
                    "gid" => {
                        let gid = parse_numeric_identifier(value).ok_or_else(|| {
                            config_parse_error(path, line_number, format!("invalid gid '{value}'"))
                        })?;
                        builder.set_gid(gid, path, line_number)?;
                    }
                    "timeout" => {
                        let timeout = parse_timeout_seconds(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid timeout '{value}'"),
                            )
                        })?;
                        builder.set_timeout(timeout, path, line_number)?;
                    }
                    "max connections" => {
                        let max = parse_max_connections_directive(value).ok_or_else(|| {
                            config_parse_error(
                                path,
                                line_number,
                                format!("invalid max connections value '{value}'"),
                            )
                        })?;
                        builder.set_max_connections(max, path, line_number)?;
                    }
                    _ => {
                        // Unsupported directives are ignored for now.
                    }
                }
                continue;
            }

            match key.as_str() {
                "refuse options" => {
                    if value.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'refuse options' directive must not be empty",
                        ));
                    }
                    let options = parse_refuse_option_list(value).map_err(|error| {
                        config_parse_error(
                            path,
                            line_number,
                            format!("invalid 'refuse options' directive: {error}"),
                        )
                    })?;

                    if let Some(existing_line) = global_refuse_line {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            format!(
                                "duplicate 'refuse options' directive in global section (previously defined on line {})",
                                existing_line
                            ),
                        ));
                    }

                    global_refuse_line = Some(line_number);
                    global_refuse_directives.push((
                        options,
                        ConfigDirectiveOrigin {
                            path: canonical.clone(),
                            line: line_number,
                        },
                    ));
                }
                "include" => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'include' directive must not be empty",
                        ));
                    }

                    let include_path = resolve_config_relative_path(&canonical, trimmed);
                    let included = parse_config_modules_inner(&include_path, stack)?;

                    if !included.modules.is_empty() {
                        modules.extend(included.modules);
                    }

                    if !included.motd_lines.is_empty() {
                        motd_lines.extend(included.motd_lines);
                    }

                    if !included.global_refuse_options.is_empty() {
                        global_refuse_directives.extend(included.global_refuse_options);
                    }

                    if let Some((components, origin)) = included.global_bandwidth_limit {
                        if let Some((existing, existing_origin)) = &global_bwlimit {
                            if existing != &components {
                                return Err(config_parse_error(
                                    &origin.path,
                                    origin.line,
                                    format!(
                                        "duplicate 'bwlimit' directive in global section (previously defined on line {})",
                                        existing_origin.line
                                    ),
                                ));
                            }
                        } else {
                            global_bwlimit = Some((components, origin));
                        }
                    }

                    if let Some((secrets_path, origin)) = included.global_secrets_file {
                        if let Some((existing, existing_origin)) = &global_secrets_file {
                            if existing != &secrets_path {
                                return Err(config_parse_error(
                                    &origin.path,
                                    origin.line,
                                    format!(
                                        "duplicate 'secrets file' directive in global section (previously defined on line {})",
                                        existing_origin.line
                                    ),
                                ));
                            }
                        } else {
                            global_secrets_file = Some((secrets_path, origin));
                        }
                    }
                }
                "motd file" => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'motd file' directive must not be empty",
                        ));
                    }

                    let motd_path = resolve_config_relative_path(path, trimmed);
                    let contents = fs::read_to_string(&motd_path).map_err(|error| {
                        config_parse_error(
                            path,
                            line_number,
                            format!(
                                "failed to read motd file '{}': {}",
                                motd_path.display(),
                                error
                            ),
                        )
                    })?;

                    for raw_line in contents.lines() {
                        motd_lines.push(raw_line.trim_end_matches('\r').to_string());
                    }
                }
                "motd" => {
                    motd_lines.push(value.trim_end_matches(['\r', '\n']).to_string());
                }
                "pid file" => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'pid file' directive must not be empty",
                        ));
                    }

                    let resolved = resolve_config_relative_path(path, trimmed);
                    if let Some((existing, origin)) = &pid_file {
                        if existing != &resolved {
                            return Err(config_parse_error(
                                path,
                                line_number,
                                format!(
                                    "duplicate 'pid file' directive in global section (previously defined on line {})",
                                    origin.line
                                ),
                            ));
                        }
                    } else {
                        pid_file = Some((
                            resolved,
                            ConfigDirectiveOrigin {
                                path: canonical.clone(),
                                line: line_number,
                            },
                        ));
                    }
                }
                "reverse lookup" => {
                    let parsed = parse_boolean_directive(value).ok_or_else(|| {
                        config_parse_error(
                            path,
                            line_number,
                            format!("invalid boolean value '{}' for 'reverse lookup'", value),
                        )
                    })?;

                    let origin = ConfigDirectiveOrigin {
                        path: canonical.clone(),
                        line: line_number,
                    };

                    if let Some((existing, existing_origin)) = &reverse_lookup {
                        if *existing != parsed {
                            return Err(config_parse_error(
                                path,
                                line_number,
                                format!(
                                    "duplicate 'reverse lookup' directive in global section (previously defined on line {})",
                                    existing_origin.line
                                ),
                            ));
                        }
                    } else {
                        reverse_lookup = Some((parsed, origin));
                    }
                }
                "bwlimit" => {
                    if value.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'bwlimit' directive must not be empty",
                        ));
                    }

                    let components = parse_config_bwlimit(value, path, line_number)?;
                    let origin = ConfigDirectiveOrigin {
                        path: canonical.clone(),
                        line: line_number,
                    };

                    if let Some((existing, existing_origin)) = &global_bwlimit {
                        if existing != &components {
                            return Err(config_parse_error(
                                &origin.path,
                                origin.line,
                                format!(
                                    "duplicate 'bwlimit' directive in global section (previously defined on line {})",
                                    existing_origin.line
                                ),
                            ));
                        }
                    } else {
                        global_bwlimit = Some((components, origin));
                    }
                }
                "secrets file" => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'secrets file' directive must not be empty",
                        ));
                    }

                    let resolved = resolve_config_relative_path(path, trimmed);
                    let validated = validate_secrets_file(&resolved, path, line_number)?;
                    let origin = ConfigDirectiveOrigin {
                        path: canonical.clone(),
                        line: line_number,
                    };

                    if let Some((existing, existing_origin)) = &global_secrets_file {
                        if existing != &validated {
                            return Err(config_parse_error(
                                path,
                                line_number,
                                format!(
                                    "duplicate 'secrets file' directive in global section (previously defined on line {})",
                                    existing_origin.line
                                ),
                            ));
                        }
                    } else {
                        global_secrets_file = Some((validated, origin));
                    }
                }
                "lock file" => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return Err(config_parse_error(
                            path,
                            line_number,
                            "'lock file' directive must not be empty",
                        ));
                    }

                    let resolved = resolve_config_relative_path(path, trimmed);
                    let origin = ConfigDirectiveOrigin {
                        path: canonical.clone(),
                        line: line_number,
                    };

                    if let Some((existing, existing_origin)) = &lock_file {
                        if existing != &resolved {
                            return Err(config_parse_error(
                                path,
                                line_number,
                                format!(
                                    "duplicate 'lock file' directive in global section (previously defined on line {})",
                                    existing_origin.line
                                ),
                            ));
                        }
                    } else {
                        lock_file = Some((resolved, origin));
                    }
                }
                _ => {
                    return Err(config_parse_error(
                        path,
                        line_number,
                        "directive outside module section",
                    ));
                }
            }
        }

        if let Some(builder) = current {
            let default_secrets = global_secrets_file.as_ref().map(|(p, _)| p.as_path());
            modules.push(builder.finish(path, default_secrets)?);
        }

        Ok(ParsedConfigModules {
            modules,
            global_refuse_options: global_refuse_directives,
            motd_lines,
            pid_file,
            reverse_lookup,
            lock_file,
            global_bandwidth_limit: global_bwlimit,
            global_secrets_file,
        })
    })();

    stack.pop();
    result
}

fn resolve_config_relative_path(config_path: &Path, value: &str) -> PathBuf {
    let candidate = Path::new(value);
    if candidate.is_absolute() {
        return candidate.to_path_buf();
    }

    if let Some(parent) = config_path.parent() {
        parent.join(candidate)
    } else {
        candidate.to_path_buf()
    }
}

struct ModuleDefinitionBuilder {
    name: String,
    path: Option<PathBuf>,
    comment: Option<String>,
    hosts_allow: Option<Vec<HostPattern>>,
    hosts_deny: Option<Vec<HostPattern>>,
    auth_users: Option<Vec<String>>,
    secrets_file: Option<PathBuf>,
    declaration_line: usize,
    bandwidth_limit: Option<NonZeroU64>,
    bandwidth_limit_specified: bool,
    bandwidth_burst: Option<NonZeroU64>,
    bandwidth_burst_specified: bool,
    bandwidth_limit_set: bool,
    refuse_options: Option<Vec<String>>,
    read_only: Option<bool>,
    numeric_ids: Option<bool>,
    uid: Option<u32>,
    gid: Option<u32>,
    timeout: Option<Option<NonZeroU64>>,
    listable: Option<bool>,
    use_chroot: Option<bool>,
    max_connections: Option<Option<NonZeroU32>>,
}

impl ModuleDefinitionBuilder {
    fn new(name: String, line: usize) -> Self {
        Self {
            name,
            path: None,
            comment: None,
            hosts_allow: None,
            hosts_deny: None,
            auth_users: None,
            secrets_file: None,
            declaration_line: line,
            bandwidth_limit: None,
            bandwidth_limit_specified: false,
            bandwidth_burst: None,
            bandwidth_burst_specified: false,
            bandwidth_limit_set: false,
            refuse_options: None,
            read_only: None,
            numeric_ids: None,
            uid: None,
            gid: None,
            timeout: None,
            listable: None,
            use_chroot: None,
            max_connections: None,
        }
    }

    fn set_path(
        &mut self,
        path: PathBuf,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.path.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'path' directive in module '{}'", self.name),
            ));
        }

        self.path = Some(path);
        Ok(())
    }

    fn set_comment(
        &mut self,
        comment: Option<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.comment.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'comment' directive in module '{}'", self.name),
            ));
        }

        self.comment = comment;
        Ok(())
    }

    fn set_hosts_allow(
        &mut self,
        patterns: Vec<HostPattern>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.hosts_allow.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'hosts allow' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.hosts_allow = Some(patterns);
        Ok(())
    }

    fn set_hosts_deny(
        &mut self,
        patterns: Vec<HostPattern>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.hosts_deny.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'hosts deny' directive in module '{}'", self.name),
            ));
        }

        self.hosts_deny = Some(patterns);
        Ok(())
    }

    fn set_auth_users(
        &mut self,
        users: Vec<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.auth_users.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'auth users' directive in module '{}'", self.name),
            ));
        }

        if users.is_empty() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "'auth users' directive in module '{}' must list at least one user",
                    self.name
                ),
            ));
        }

        self.auth_users = Some(users);
        Ok(())
    }

    fn set_secrets_file(
        &mut self,
        path: PathBuf,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.secrets_file.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'secrets file' directive in module '{}'",
                    self.name
                ),
            ));
        }

        let validated = validate_secrets_file(&path, config_path, line)?;
        self.secrets_file = Some(validated);
        Ok(())
    }

    fn set_bandwidth_limit(
        &mut self,
        limit: Option<NonZeroU64>,
        burst: Option<NonZeroU64>,
        burst_specified: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.bandwidth_limit_set {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'bwlimit' directive in module '{}'", self.name),
            ));
        }

        self.bandwidth_limit = limit;
        self.bandwidth_burst = burst;
        self.bandwidth_burst_specified = burst_specified;
        self.bandwidth_limit_specified = true;
        self.bandwidth_limit_set = true;
        Ok(())
    }

    fn set_refuse_options(
        &mut self,
        options: Vec<String>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.refuse_options.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'refuse options' directive in module '{}'",
                    self.name
                ),
            ));
        }

        if options.is_empty() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "'refuse options' directive in module '{}' must list at least one option",
                    self.name
                ),
            ));
        }

        self.refuse_options = Some(options);
        Ok(())
    }

    fn set_read_only(
        &mut self,
        read_only: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.read_only.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'read only' directive in module '{}'", self.name),
            ));
        }

        self.read_only = Some(read_only);
        Ok(())
    }

    fn set_numeric_ids(
        &mut self,
        numeric_ids: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.numeric_ids.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'numeric ids' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.numeric_ids = Some(numeric_ids);
        Ok(())
    }

    fn set_listable(
        &mut self,
        listable: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.listable.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'list' directive in module '{}'", self.name),
            ));
        }

        self.listable = Some(listable);
        Ok(())
    }

    fn set_use_chroot(
        &mut self,
        use_chroot: bool,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.use_chroot.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'use chroot' directive in module '{}'", self.name),
            ));
        }

        self.use_chroot = Some(use_chroot);
        Ok(())
    }

    fn set_uid(&mut self, uid: u32, config_path: &Path, line: usize) -> Result<(), DaemonError> {
        if self.uid.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'uid' directive in module '{}'", self.name),
            ));
        }

        self.uid = Some(uid);
        Ok(())
    }

    fn set_gid(&mut self, gid: u32, config_path: &Path, line: usize) -> Result<(), DaemonError> {
        if self.gid.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'gid' directive in module '{}'", self.name),
            ));
        }

        self.gid = Some(gid);
        Ok(())
    }

    fn set_timeout(
        &mut self,
        timeout: Option<NonZeroU64>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.timeout.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!("duplicate 'timeout' directive in module '{}'", self.name),
            ));
        }

        self.timeout = Some(timeout);
        Ok(())
    }

    fn set_max_connections(
        &mut self,
        max: Option<NonZeroU32>,
        config_path: &Path,
        line: usize,
    ) -> Result<(), DaemonError> {
        if self.max_connections.is_some() {
            return Err(config_parse_error(
                config_path,
                line,
                format!(
                    "duplicate 'max connections' directive in module '{}'",
                    self.name
                ),
            ));
        }

        self.max_connections = Some(max);
        Ok(())
    }

    fn finish(
        self,
        config_path: &Path,
        default_secrets: Option<&Path>,
    ) -> Result<ModuleDefinition, DaemonError> {
        let path = self.path.ok_or_else(|| {
            config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "module '{}' is missing required 'path' directive",
                    self.name
                ),
            )
        })?;

        let use_chroot = self.use_chroot.unwrap_or(true);

        if use_chroot && !path.is_absolute() {
            return Err(config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "module '{}' requires an absolute path when 'use chroot' is enabled",
                    self.name
                ),
            ));
        }

        if self.auth_users.as_ref().is_some_and(Vec::is_empty) {
            return Err(config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "'auth users' directive in module '{}' must list at least one user",
                    self.name
                ),
            ));
        }

        let auth_users = self.auth_users.unwrap_or_default();
        let secrets_file = if auth_users.is_empty() {
            self.secrets_file
        } else if let Some(path) = self.secrets_file {
            Some(path)
        } else if let Some(default) = default_secrets {
            Some(default.to_path_buf())
        } else {
            return Err(config_parse_error(
                config_path,
                self.declaration_line,
                format!(
                    "module '{}' specifies 'auth users' but is missing the required 'secrets file' directive",
                    self.name
                ),
            ));
        };

        Ok(ModuleDefinition {
            name: self.name,
            path,
            comment: self.comment,
            hosts_allow: self.hosts_allow.unwrap_or_default(),
            hosts_deny: self.hosts_deny.unwrap_or_default(),
            auth_users,
            secrets_file,
            bandwidth_limit: self.bandwidth_limit,
            bandwidth_limit_specified: self.bandwidth_limit_specified,
            bandwidth_burst: self.bandwidth_burst,
            bandwidth_burst_specified: self.bandwidth_burst_specified,
            bandwidth_limit_configured: self.bandwidth_limit_set,
            refuse_options: self.refuse_options.unwrap_or_default(),
            read_only: self.read_only.unwrap_or(true),
            numeric_ids: self.numeric_ids.unwrap_or(false),
            uid: self.uid,
            gid: self.gid,
            timeout: self.timeout.unwrap_or(None),
            listable: self.listable.unwrap_or(true),
            use_chroot,
            max_connections: self.max_connections.unwrap_or(None),
        })
    }
}

fn parse_auth_user_list(value: &str) -> Result<Vec<String>, String> {
    let mut users = Vec::new();
    let mut seen = HashSet::new();

    for segment in value.split(',') {
        for token in segment.split_whitespace() {
            let trimmed = token.trim();
            if trimmed.is_empty() {
                continue;
            }

            if seen.insert(trimmed.to_ascii_lowercase()) {
                users.push(trimmed.to_string());
            }
        }
    }

    if users.is_empty() {
        return Err("must specify at least one username".to_string());
    }

    Ok(users)
}

fn parse_refuse_option_list(value: &str) -> Result<Vec<String>, String> {
    let mut options = Vec::new();
    let mut seen = HashSet::new();

    for segment in value.split(',') {
        for token in segment.split_whitespace() {
            let trimmed = token.trim();
            if trimmed.is_empty() {
                continue;
            }

            let canonical = trimmed.to_ascii_lowercase();
            if seen.insert(canonical.clone()) {
                options.push(canonical);
            }
        }
    }

    if options.is_empty() {
        return Err("must specify at least one option".to_string());
    }

    Ok(options)
}

fn validate_secrets_file(
    path: &Path,
    config_path: &Path,
    line: usize,
) -> Result<PathBuf, DaemonError> {
    let metadata = fs::metadata(path).map_err(|error| {
        config_parse_error(
            config_path,
            line,
            format!(
                "failed to access secrets file '{}': {}",
                path.display(),
                error
            ),
        )
    })?;

    if let Err(detail) = ensure_secrets_file(path, &metadata) {
        return Err(config_parse_error(config_path, line, detail));
    }

    Ok(path.to_path_buf())
}

fn validate_secrets_file_from_env(
    path: &Path,
    env: &'static str,
) -> Result<Option<PathBuf>, DaemonError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }

            return Err(secrets_env_error(
                env,
                path,
                format!("could not be accessed: {error}"),
            ));
        }
    };

    if let Err(detail) = ensure_secrets_file(path, &metadata) {
        return Err(secrets_env_error(env, path, detail));
    }

    Ok(Some(path.to_path_buf()))
}

fn ensure_secrets_file(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if !metadata.is_file() {
        return Err(format!(
            "secrets file '{}' must be a regular file",
            path.display()
        ));
    }

    #[cfg(unix)]
    {
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "secrets file '{}' must not be accessible to group or others (expected permissions 0600)",
                path.display()
            ));
        }
    }

    Ok(())
}

fn parse_boolean_directive(value: &str) -> Option<bool> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_numeric_identifier(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    trimmed.parse().ok()
}

fn parse_timeout_seconds(value: &str) -> Option<Option<NonZeroU64>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let seconds: u64 = trimmed.parse().ok()?;
    if seconds == 0 {
        Some(None)
    } else {
        Some(NonZeroU64::new(seconds))
    }
}

fn parse_max_connections_directive(value: &str) -> Option<Option<NonZeroU32>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed == "0" {
        return Some(None);
    }

    trimmed.parse::<NonZeroU32>().ok().map(Some)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum HostPattern {
    Any,
    Ipv4 { network: Ipv4Addr, prefix: u8 },
    Ipv6 { network: Ipv6Addr, prefix: u8 },
    Hostname(HostnamePattern),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AddressFamily {
    Ipv4,
    Ipv6,
}

impl AddressFamily {
    fn from_ip(addr: IpAddr) -> Self {
        match addr {
            IpAddr::V4(_) => Self::Ipv4,
            IpAddr::V6(_) => Self::Ipv6,
        }
    }

    fn matches(self, addr: IpAddr) -> bool {
        matches!(
            (self, addr),
            (Self::Ipv4, IpAddr::V4(_)) | (Self::Ipv6, IpAddr::V6(_))
        )
    }
}

impl HostPattern {
    fn parse(token: &str) -> Result<Self, String> {
        let token = token.trim();
        if token.is_empty() {
            return Err("host pattern must be non-empty".to_string());
        }

        if token == "*" || token.eq_ignore_ascii_case("all") {
            return Ok(Self::Any);
        }

        let (address_str, prefix_text) = if let Some((addr, mask)) = token.split_once('/') {
            (addr, Some(mask))
        } else {
            (token, None)
        };

        if let Ok(ipv4) = address_str.parse::<Ipv4Addr>() {
            let prefix = prefix_text
                .map(|value| {
                    value
                        .parse::<u8>()
                        .map_err(|_| "invalid IPv4 prefix length".to_string())
                })
                .transpose()?;
            return Self::from_ipv4(ipv4, prefix.unwrap_or(32));
        }

        if let Ok(ipv6) = address_str.parse::<Ipv6Addr>() {
            let prefix = prefix_text
                .map(|value| {
                    value
                        .parse::<u8>()
                        .map_err(|_| "invalid IPv6 prefix length".to_string())
                })
                .transpose()?;
            return Self::from_ipv6(ipv6, prefix.unwrap_or(128));
        }

        if prefix_text.is_some() {
            return Err("invalid host pattern; expected IPv4/IPv6 address".to_string());
        }

        HostnamePattern::parse(address_str).map(Self::Hostname)
    }

    fn from_ipv4(addr: Ipv4Addr, prefix: u8) -> Result<Self, String> {
        if prefix > 32 {
            return Err("IPv4 prefix length must be between 0 and 32".to_string());
        }

        if prefix == 0 {
            return Ok(Self::Ipv4 {
                network: Ipv4Addr::UNSPECIFIED,
                prefix,
            });
        }

        let shift = 32 - u32::from(prefix);
        let mask = u32::MAX.checked_shl(shift).unwrap_or(0);
        let network = u32::from(addr) & mask;
        Ok(Self::Ipv4 {
            network: Ipv4Addr::from(network),
            prefix,
        })
    }

    fn from_ipv6(addr: Ipv6Addr, prefix: u8) -> Result<Self, String> {
        if prefix > 128 {
            return Err("IPv6 prefix length must be between 0 and 128".to_string());
        }

        if prefix == 0 {
            return Ok(Self::Ipv6 {
                network: Ipv6Addr::UNSPECIFIED,
                prefix,
            });
        }

        let shift = 128 - u32::from(prefix);
        let mask = u128::MAX.checked_shl(shift).unwrap_or(0);
        let network = u128::from(addr) & mask;
        Ok(Self::Ipv6 {
            network: Ipv6Addr::from(network),
            prefix,
        })
    }

    fn matches(&self, addr: IpAddr, hostname: Option<&str>) -> bool {
        match (self, addr) {
            (Self::Any, _) => true,
            (Self::Ipv4 { network, prefix }, IpAddr::V4(candidate)) => {
                if *prefix == 0 {
                    true
                } else {
                    let shift = 32 - u32::from(*prefix);
                    let mask = u32::MAX.checked_shl(shift).unwrap_or(0);
                    (u32::from(candidate) & mask) == u32::from(*network)
                }
            }
            (Self::Ipv6 { network, prefix }, IpAddr::V6(candidate)) => {
                if *prefix == 0 {
                    true
                } else {
                    let shift = 128 - u32::from(*prefix);
                    let mask = u128::MAX.checked_shl(shift).unwrap_or(0);
                    (u128::from(candidate) & mask) == u128::from(*network)
                }
            }
            (Self::Hostname(pattern), _) => {
                hostname.map(|name| pattern.matches(name)).unwrap_or(false)
            }
            _ => false,
        }
    }

    fn requires_hostname(&self) -> bool {
        matches!(self, Self::Hostname(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HostnamePattern {
    kind: HostnamePatternKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum HostnamePatternKind {
    Exact(String),
    Suffix(String),
    Wildcard(String),
}

impl HostnamePattern {
    fn parse(pattern: &str) -> Result<Self, String> {
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            return Err("host pattern must be non-empty".to_string());
        }

        let normalized = trimmed.trim_end_matches('.');
        let lower = normalized.to_ascii_lowercase();

        if lower.contains('*') || lower.contains('?') {
            return Ok(Self {
                kind: HostnamePatternKind::Wildcard(lower),
            });
        }

        if lower.starts_with('.') {
            let suffix = lower.trim_start_matches('.').to_string();
            return Ok(Self {
                kind: HostnamePatternKind::Suffix(suffix),
            });
        }

        Ok(Self {
            kind: HostnamePatternKind::Exact(lower),
        })
    }

    fn matches(&self, hostname: &str) -> bool {
        match &self.kind {
            HostnamePatternKind::Exact(expected) => hostname == expected,
            HostnamePatternKind::Suffix(suffix) => {
                if suffix.is_empty() {
                    return true;
                }

                if hostname == suffix {
                    return true;
                }

                if hostname.len() <= suffix.len() {
                    return false;
                }

                hostname.ends_with(suffix)
                    && hostname
                        .as_bytes()
                        .get(hostname.len() - suffix.len() - 1)
                        .is_some_and(|byte| *byte == b'.')
            }
            HostnamePatternKind::Wildcard(pattern) => wildcard_match(pattern, hostname),
        }
    }
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern_bytes = pattern.as_bytes();
    let text_bytes = text.as_bytes();

    let mut pat_index = 0usize;
    let mut text_index = 0usize;
    let mut star_index: Option<usize> = None;
    let mut match_index = 0usize;

    while text_index < text_bytes.len() {
        if pat_index < pattern_bytes.len()
            && (pattern_bytes[pat_index] == b'?'
                || pattern_bytes[pat_index] == text_bytes[text_index])
        {
            pat_index += 1;
            text_index += 1;
        } else if pat_index < pattern_bytes.len() && pattern_bytes[pat_index] == b'*' {
            // Record the position of the wildcard and optimistically advance past it.
            star_index = Some(pat_index);
            pat_index += 1;
            match_index = text_index;
        } else if let Some(star_pos) = star_index {
            // Retry the match by letting the last '*' consume one additional character.
            pat_index = star_pos + 1;
            match_index += 1;
            text_index = match_index;
        } else {
            return false;
        }
    }

    while pat_index < pattern_bytes.len() && pattern_bytes[pat_index] == b'*' {
        pat_index += 1;
    }

    pat_index == pattern_bytes.len()
}

fn parse_host_list(
    value: &str,
    config_path: &Path,
    line: usize,
    directive: &str,
) -> Result<Vec<HostPattern>, DaemonError> {
    let mut patterns = Vec::new();

    for token in value.split(|ch: char| ch.is_ascii_whitespace() || ch == ',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }

        let pattern = HostPattern::parse(token).map_err(|message| {
            config_parse_error(
                config_path,
                line,
                format!("{directive} directive contains invalid pattern '{token}': {message}"),
            )
        })?;
        patterns.push(pattern);
    }

    if patterns.is_empty() {
        return Err(config_parse_error(
            config_path,
            line,
            format!("{directive} directive must specify at least one pattern"),
        ));
    }

    Ok(patterns)
}

/// Runs the daemon orchestration using the provided configuration.
///
/// The helper binds a TCP listener (defaulting to `0.0.0.0:873`), accepts a
/// single connection, performs the legacy ASCII handshake, and replies with a
/// deterministic `@ERROR` message explaining that module serving is not yet
/// available. This behaviour gives higher layers a concrete negotiation target
/// while keeping the observable output stable.
pub fn run_daemon(config: DaemonConfig) -> Result<(), DaemonError> {
    let options = RuntimeOptions::parse_with_brand(
        config.arguments(),
        config.brand(),
        config.load_default_paths(),
    )?;
    serve_connections(options)
}

/// Parsed command produced by [`parse_args`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgramName {
    Rsyncd,
    OcRsyncd,
}

impl ProgramName {
    #[inline]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Rsyncd => Brand::Upstream.daemon_program_name(),
            Self::OcRsyncd => Brand::Oc.daemon_program_name(),
        }
    }

    #[inline]
    pub(crate) const fn brand(self) -> Brand {
        match self {
            Self::Rsyncd => Brand::Upstream,
            Self::OcRsyncd => Brand::Oc,
        }
    }
}

fn detect_program_name(program: Option<&OsStr>) -> ProgramName {
    match branding::detect_brand(program) {
        Brand::Oc => ProgramName::OcRsyncd,
        Brand::Upstream => ProgramName::Rsyncd,
    }
}

pub(crate) struct ParsedArgs {
    pub(crate) program_name: ProgramName,
    pub(crate) show_help: bool,
    pub(crate) show_version: bool,
    pub(crate) delegate_system_rsync: bool,
    pub(crate) remainder: Vec<OsString>,
}

fn clap_command(program_name: &'static str) -> Command {
    Command::new(program_name)
        .disable_help_flag(true)
        .disable_version_flag(true)
        .arg_required_else_help(false)
        .arg(
            Arg::new("help")
                .long("help")
                .help("Show this help message and exit.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("version")
                .long("version")
                .short('V')
                .help("Output version information and exit.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("delegate-system-rsync")
                .long("delegate-system-rsync")
                .help("Launch the system rsync daemon with the supplied arguments.")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("args")
                .action(ArgAction::Append)
                .num_args(0..)
                .allow_hyphen_values(true)
                .trailing_var_arg(true)
                .value_parser(OsStringValueParser::new()),
        )
}

pub(crate) fn parse_args<I, S>(arguments: I) -> Result<ParsedArgs, clap::Error>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut args: Vec<OsString> = arguments.into_iter().map(Into::into).collect();

    let program_name = detect_program_name(args.first().map(OsString::as_os_str));

    if args.is_empty() {
        args.push(OsString::from(program_name.as_str()));
    }

    let mut matches = clap_command(program_name.as_str()).try_get_matches_from(args)?;

    let show_help = matches.get_flag("help");
    let show_version = matches.get_flag("version");
    let delegate_system_rsync = matches.get_flag("delegate-system-rsync");
    let remainder = matches
        .remove_many::<OsString>("args")
        .map(|values| values.collect())
        .unwrap_or_default();

    Ok(ParsedArgs {
        program_name,
        show_help,
        show_version,
        delegate_system_rsync,
        remainder,
    })
}

pub(crate) fn render_help(program_name: ProgramName) -> String {
    help_text(program_name.brand())
}

pub(crate) fn write_message<W: Write>(
    message: &Message,
    sink: &mut MessageSink<W>,
) -> io::Result<()> {
    sink.write(message)
}

fn log_sd_notify_failure(log: Option<&SharedLogSink>, context: &str, error: &io::Error) {
    if let Some(sink) = log {
        let payload = format!("failed to notify systemd about {}: {}", context, error);
        let message = rsync_warning!(payload).with_role(Role::Daemon);
        log_message(sink, &message);
    }
}

fn format_connection_status(active: usize) -> String {
    match active {
        0 => String::from("Idle; waiting for connections"),
        1 => String::from("Serving 1 connection"),
        count => format!("Serving {count} connections"),
    }
}

fn serve_connections(options: RuntimeOptions) -> Result<(), DaemonError> {
    let manifest = manifest();
    let version = manifest.rust_version();
    let RuntimeOptions {
        bind_address,
        port,
        max_sessions,
        modules,
        motd_lines,
        bandwidth_limit,
        bandwidth_burst,
        log_file,
        pid_file,
        reverse_lookup,
        lock_file,
        delegate_arguments,
        inline_modules,
        ..
    } = options;

    let delegation = configured_fallback_binary().and_then(|binary| {
        if inline_modules {
            None
        } else {
            Some(SessionDelegation::new(binary, delegate_arguments))
        }
    });

    let pid_guard = if let Some(path) = pid_file {
        Some(PidFileGuard::create(path)?)
    } else {
        None
    };

    let log_sink = if let Some(path) = log_file {
        Some(open_log_sink(&path)?)
    } else {
        None
    };

    let connection_limiter = if let Some(path) = lock_file {
        Some(Arc::new(ConnectionLimiter::open(path)?))
    } else {
        None
    };

    let modules: Arc<Vec<ModuleRuntime>> = Arc::new(
        modules
            .into_iter()
            .map(|definition| ModuleRuntime::new(definition, connection_limiter.clone()))
            .collect(),
    );
    let motd_lines = Arc::new(motd_lines);
    let requested_addr = SocketAddr::new(bind_address, port);
    let listener =
        TcpListener::bind(requested_addr).map_err(|error| bind_error(requested_addr, error))?;
    let local_addr = listener.local_addr().unwrap_or(requested_addr);

    let notifier = systemd::ServiceNotifier::new();
    let ready_status = format!("Listening on {}", local_addr);
    if let Err(error) = notifier.ready(Some(&ready_status)) {
        log_sd_notify_failure(log_sink.as_ref(), "service readiness", &error);
    }

    if let Some(log) = log_sink.as_ref() {
        let text = format!(
            "rsyncd version {} starting, listening on port {}",
            version,
            local_addr.port()
        );
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    let mut served = 0usize;
    let mut workers: Vec<thread::JoinHandle<WorkerResult>> = Vec::new();
    let max_sessions = max_sessions.map(NonZeroUsize::get);
    let mut active_connections = 0usize;

    loop {
        reap_finished_workers(&mut workers)?;

        let current_active = workers.len();
        if current_active != active_connections {
            let status = format_connection_status(current_active);
            if let Err(error) = notifier.status(&status) {
                log_sd_notify_failure(log_sink.as_ref(), "connection status update", &error);
            }
            active_connections = current_active;
        }

        match listener.accept() {
            Ok((stream, peer_addr)) => {
                let modules = Arc::clone(&modules);
                let motd_lines = Arc::clone(&motd_lines);
                let log_for_worker = log_sink.as_ref().map(Arc::clone);
                let delegation_clone = delegation.clone();
                let handle = thread::spawn(move || {
                    let modules_vec = modules.as_ref();
                    let motd_vec = motd_lines.as_ref();
                    handle_session(
                        stream,
                        peer_addr,
                        SessionParams {
                            modules: modules_vec.as_slice(),
                            motd_lines: motd_vec.as_slice(),
                            daemon_limit: bandwidth_limit,
                            daemon_burst: bandwidth_burst,
                            log_sink: log_for_worker,
                            reverse_lookup,
                            delegation: delegation_clone,
                        },
                    )
                    .map_err(|error| (Some(peer_addr), error))
                });
                workers.push(handle);
                served = served.saturating_add(1);

                let current_active = workers.len();
                if current_active != active_connections {
                    let status = format_connection_status(current_active);
                    if let Err(error) = notifier.status(&status) {
                        log_sd_notify_failure(
                            log_sink.as_ref(),
                            "connection status update",
                            &error,
                        );
                    }
                    active_connections = current_active;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                continue;
            }
            Err(error) => {
                return Err(accept_error(local_addr, error));
            }
        }

        if let Some(limit) = max_sessions {
            if served >= limit {
                if let Err(error) = notifier.status("Draining worker threads") {
                    log_sd_notify_failure(log_sink.as_ref(), "connection status update", &error);
                }
                break;
            }
        }
    }

    let result = drain_workers(&mut workers);

    let shutdown_status = match served {
        0 => String::from("No connections handled; shutting down"),
        1 => String::from("Served 1 connection; shutting down"),
        count => format!("Served {count} connections; shutting down"),
    };
    if let Err(error) = notifier.status(&shutdown_status) {
        log_sd_notify_failure(log_sink.as_ref(), "shutdown status", &error);
    }
    if let Err(error) = notifier.stopping() {
        log_sd_notify_failure(log_sink.as_ref(), "service shutdown", &error);
    }

    if let Some(log) = log_sink.as_ref() {
        let text = format!("rsyncd version {} shutting down", version);
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    drop(pid_guard);

    result
}

struct PidFileGuard {
    path: PathBuf,
}

impl PidFileGuard {
    fn create(path: PathBuf) -> Result<Self, DaemonError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|error| pid_file_error(&path, error))?;
            }
        }

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .map_err(|error| pid_file_error(&path, error))?;
        writeln!(file, "{}", std::process::id()).map_err(|error| pid_file_error(&path, error))?;
        file.sync_all()
            .map_err(|error| pid_file_error(&path, error))?;

        Ok(Self { path })
    }
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

type WorkerResult = Result<(), (Option<SocketAddr>, io::Error)>;

fn reap_finished_workers(
    workers: &mut Vec<thread::JoinHandle<WorkerResult>>,
) -> Result<(), DaemonError> {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            let handle = workers.remove(index);
            join_worker(handle)?;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn drain_workers(workers: &mut Vec<thread::JoinHandle<WorkerResult>>) -> Result<(), DaemonError> {
    while let Some(handle) = workers.pop() {
        join_worker(handle)?;
    }
    Ok(())
}

fn join_worker(handle: thread::JoinHandle<WorkerResult>) -> Result<(), DaemonError> {
    match handle.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err((peer, error))) => {
            let kind = error.kind();
            if is_connection_closed_error(kind) {
                Ok(())
            } else {
                Err(stream_error(peer, "serve legacy handshake", error))
            }
        }
        Err(panic) => {
            let description = match panic.downcast::<String>() {
                Ok(message) => *message,
                Err(payload) => match payload.downcast::<&str>() {
                    Ok(message) => (*message).to_string(),
                    Err(_) => "worker thread panicked".to_string(),
                },
            };
            let error = io::Error::other(description);
            Err(stream_error(None, "serve legacy handshake", error))
        }
    }
}

fn is_connection_closed_error(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

fn configure_stream(stream: &TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))
}

struct SessionParams<'a> {
    modules: &'a [ModuleRuntime],
    motd_lines: &'a [String],
    daemon_limit: Option<NonZeroU64>,
    daemon_burst: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
    reverse_lookup: bool,
    delegation: Option<SessionDelegation>,
}

struct LegacySessionParams<'a> {
    modules: &'a [ModuleRuntime],
    motd_lines: &'a [String],
    daemon_limit: Option<NonZeroU64>,
    daemon_burst: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
    peer_host: Option<String>,
    reverse_lookup: bool,
}

fn handle_session(
    stream: TcpStream,
    peer_addr: SocketAddr,
    params: SessionParams<'_>,
) -> io::Result<()> {
    let SessionParams {
        modules,
        motd_lines,
        daemon_limit,
        daemon_burst,
        log_sink,
        reverse_lookup,
        delegation,
    } = params;

    if let Some(delegation) = delegation.as_ref() {
        let delegated = stream
            .try_clone()
            .and_then(|clone| delegate_binary_session(clone, delegation, log_sink.as_ref()));
        if delegated.is_ok() {
            drop(stream);
            return Ok(());
        }

        if let Some(log) = log_sink.as_ref() {
            let text = format!(
                "failed to delegate session to '{}'; continuing with internal handler",
                Path::new(delegation.binary()).display()
            );
            let message = rsync_warning!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
    }

    let style = detect_session_style(&stream, delegation.is_some())?;
    configure_stream(&stream)?;

    let peer_host = if reverse_lookup {
        resolve_peer_hostname(peer_addr.ip())
    } else {
        None
    };
    if let Some(log) = log_sink.as_ref() {
        log_connection(log, peer_host.as_deref(), peer_addr);
    }

    match style {
        SessionStyle::Binary => handle_binary_session(stream, daemon_limit, daemon_burst, log_sink),
        SessionStyle::Legacy => handle_legacy_session(
            stream,
            peer_addr,
            LegacySessionParams {
                modules,
                motd_lines,
                daemon_limit,
                daemon_burst,
                log_sink,
                peer_host,
                reverse_lookup,
            },
        ),
    }
}

fn detect_session_style(stream: &TcpStream, fallback_available: bool) -> io::Result<SessionStyle> {
    stream.set_nonblocking(true)?;
    let mut peek_buf = [0u8; LEGACY_DAEMON_PREFIX_LEN];
    let decision = match stream.peek(&mut peek_buf) {
        Ok(0) => Ok(SessionStyle::Legacy),
        Ok(_) => {
            if peek_buf[0] == b'@' {
                Ok(SessionStyle::Legacy)
            } else {
                Ok(SessionStyle::Binary)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock && fallback_available => {
            Ok(SessionStyle::Binary)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(SessionStyle::Legacy),
        Err(error) => Err(error),
    };
    let restore_result = stream.set_nonblocking(false);
    match (decision, restore_result) {
        (Ok(style), Ok(())) => Ok(style),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
        (Err(primary), Err(restore)) => Err(io::Error::new(
            primary.kind(),
            format!("{primary}; also failed to restore blocking mode: {restore}",),
        )),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionStyle {
    Legacy,
    Binary,
}

fn write_limited(
    stream: &mut TcpStream,
    limiter: &mut Option<BandwidthLimiter>,
    payload: &[u8],
) -> io::Result<()> {
    if let Some(limiter) = limiter {
        let mut remaining = payload;
        while !remaining.is_empty() {
            let chunk_len = limiter.recommended_read_size(remaining.len());
            stream.write_all(&remaining[..chunk_len])?;
            let _ = limiter.register(chunk_len);
            remaining = &remaining[chunk_len..];
        }
        Ok(())
    } else {
        stream.write_all(payload)
    }
}

fn handle_legacy_session(
    stream: TcpStream,
    peer_addr: SocketAddr,
    params: LegacySessionParams<'_>,
) -> io::Result<()> {
    let LegacySessionParams {
        modules,
        motd_lines,
        daemon_limit,
        daemon_burst,
        log_sink,
        peer_host,
        reverse_lookup,
    } = params;
    let mut reader = BufReader::new(stream);
    let mut limiter = BandwidthLimitComponents::new(daemon_limit, daemon_burst).into_limiter();

    let greeting = legacy_daemon_greeting();
    write_limited(reader.get_mut(), &mut limiter, greeting.as_bytes())?;
    reader.get_mut().flush()?;

    let mut request = None;
    let mut refused_options = Vec::new();

    while let Some(line) = read_trimmed_line(&mut reader)? {
        match parse_legacy_daemon_message(&line) {
            Ok(LegacyDaemonMessage::Version(_)) => {
                let ok = format_legacy_daemon_message(LegacyDaemonMessage::Ok);
                write_limited(reader.get_mut(), &mut limiter, ok.as_bytes())?;
                reader.get_mut().flush()?;
                continue;
            }
            Ok(LegacyDaemonMessage::Other(payload)) => {
                if let Some(option) = parse_daemon_option(payload) {
                    refused_options.push(option.to_string());
                    continue;
                }
            }
            Ok(LegacyDaemonMessage::Exit) => return Ok(()),
            Ok(
                LegacyDaemonMessage::Ok
                | LegacyDaemonMessage::Capabilities { .. }
                | LegacyDaemonMessage::AuthRequired { .. }
                | LegacyDaemonMessage::AuthChallenge { .. },
            ) => {
                request = Some(line);
                break;
            }
            Err(_) => {}
        }

        request = Some(line);
        break;
    }

    let request = request.unwrap_or_default();

    advertise_capabilities(reader.get_mut(), modules)?;

    if request == "#list" {
        if let Some(log) = log_sink.as_ref() {
            log_list_request(log, peer_host.as_deref(), peer_addr);
        }
        respond_with_module_list(
            reader.get_mut(),
            &mut limiter,
            modules,
            motd_lines,
            peer_addr.ip(),
            reverse_lookup,
        )?;
    } else if request.is_empty() {
        write_limited(
            reader.get_mut(),
            &mut limiter,
            HANDSHAKE_ERROR_PAYLOAD.as_bytes(),
        )?;
        write_limited(reader.get_mut(), &mut limiter, b"\n")?;
        let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
        write_limited(reader.get_mut(), &mut limiter, exit.as_bytes())?;
        reader.get_mut().flush()?;
    } else {
        respond_with_module_request(
            &mut reader,
            &mut limiter,
            modules,
            &request,
            peer_addr.ip(),
            peer_host.as_deref(),
            &refused_options,
            log_sink.as_ref(),
            reverse_lookup,
        )?;
    }

    Ok(())
}

fn handle_binary_session(
    stream: TcpStream,
    daemon_limit: Option<NonZeroU64>,
    daemon_burst: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
) -> io::Result<()> {
    handle_binary_session_internal(stream, daemon_limit, daemon_burst, log_sink)
}

fn handle_binary_session_internal(
    mut stream: TcpStream,
    daemon_limit: Option<NonZeroU64>,
    daemon_burst: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
) -> io::Result<()> {
    let mut limiter = BandwidthLimitComponents::new(daemon_limit, daemon_burst).into_limiter();

    let mut client_bytes = [0u8; 4];
    stream.read_exact(&mut client_bytes)?;
    let client_raw = u32::from_be_bytes(client_bytes);
    let client_byte = client_raw.min(u32::from(u8::MAX)) as u8;
    ProtocolVersion::from_peer_advertisement(client_byte).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "binary negotiation protocol identifier outside supported range",
        )
    })?;

    let server_bytes = u32::from(ProtocolVersion::NEWEST.as_u8()).to_be_bytes();
    stream.write_all(&server_bytes)?;
    stream.flush()?;

    let mut frames = Vec::new();
    MessageFrame::new(
        MessageCode::Error,
        HANDSHAKE_ERROR_PAYLOAD.as_bytes().to_vec(),
    )?
    .encode_into_writer(&mut frames)?;
    let exit_code = u32::try_from(FEATURE_UNAVAILABLE_EXIT_CODE).unwrap_or_default();
    MessageFrame::new(MessageCode::ErrorExit, exit_code.to_be_bytes().to_vec())?
        .encode_into_writer(&mut frames)?;
    write_limited(&mut stream, &mut limiter, &frames)?;
    stream.flush()?;

    if let Some(log) = log_sink.as_ref() {
        let message =
            rsync_info!("binary negotiation forwarded error frames").with_role(Role::Daemon);
        log_message(log, &message);
    }

    Ok(())
}

fn forward_client_to_child(
    mut upstream: TcpStream,
    mut child_stdin: ChildStdin,
    done: Arc<AtomicBool>,
) -> io::Result<u64> {
    upstream.set_read_timeout(Some(Duration::from_millis(200)))?;
    let mut forwarded = 0u64;
    let mut buffer = [0u8; 8192];

    loop {
        if done.load(Ordering::SeqCst) {
            break;
        }

        match upstream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                child_stdin.write_all(&buffer[..count])?;
                forwarded += u64::try_from(count).unwrap_or_default();
            }
            Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(ref err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(err) => {
                if is_connection_closed_error(err.kind()) {
                    break;
                }

                return Err(err);
            }
        }
    }

    child_stdin.flush()?;
    Ok(forwarded)
}

#[derive(Clone)]
struct SessionDelegation {
    binary: OsString,
    args: Arc<[OsString]>,
}

impl SessionDelegation {
    fn new(binary: OsString, args: Vec<OsString>) -> Self {
        Self {
            binary,
            args: Arc::from(args.into_boxed_slice()),
        }
    }

    fn binary(&self) -> &OsString {
        &self.binary
    }

    fn args(&self) -> &[OsString] {
        &self.args
    }
}

fn delegate_binary_session(
    stream: TcpStream,
    delegation: &SessionDelegation,
    log_sink: Option<&SharedLogSink>,
) -> io::Result<()> {
    let binary = delegation.binary();
    if let Some(log) = log_sink {
        let text = format!(
            "delegating binary session to '{}'",
            Path::new(binary).display()
        );
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    let mut command = ProcessCommand::new(binary);
    command.arg("--daemon");
    command.arg("--no-detach");
    command.args(delegation.args());
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::inherit());

    let mut child = command.spawn()?;
    let child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "fallback stdin unavailable"))?;
    let mut child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "fallback stdout unavailable"))?;

    let upstream = stream.try_clone()?;
    let downstream = stream.try_clone()?;
    let control_stream = stream;
    let completion = Arc::new(AtomicBool::new(false));
    let reader_completion = Arc::clone(&completion);
    let writer_completion = Arc::clone(&completion);

    let reader =
        thread::spawn(move || forward_client_to_child(upstream, child_stdin, reader_completion));

    let writer = thread::spawn(move || {
        let mut downstream = downstream;
        let result = io::copy(&mut child_stdout, &mut downstream);
        writer_completion.store(true, Ordering::SeqCst);
        result
    });

    let status = child.wait()?;
    completion.store(true, Ordering::SeqCst);

    let write_bytes = writer
        .join()
        .map_err(|_| io::Error::other("failed to join writer thread"))??;

    #[allow(unused_must_use)]
    {
        use std::net::Shutdown;
        control_stream.shutdown(Shutdown::Both);
    }

    let read_bytes = reader
        .join()
        .map_err(|_| io::Error::other("failed to join reader thread"))??;

    if let Some(log) = log_sink {
        let text =
            format!("forwarded {read_bytes} bytes to fallback and received {write_bytes} bytes");
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    if !status.success() {
        if let Some(log) = log_sink {
            let text = format!(
                "fallback daemon '{}' exited with status {}",
                Path::new(binary).display(),
                status
            );
            let message = rsync_warning!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
    }

    Ok(())
}

fn legacy_daemon_greeting() -> String {
    let mut greeting =
        format_legacy_daemon_message(LegacyDaemonMessage::Version(ProtocolVersion::NEWEST));
    debug_assert!(greeting.ends_with('\n'));
    greeting.pop();

    for digest in LEGACY_HANDSHAKE_DIGESTS {
        greeting.push(' ');
        greeting.push_str(digest);
    }

    greeting.push('\n');
    greeting
}

fn read_trimmed_line<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = String::new();
    let bytes = reader.read_line(&mut line)?;

    if bytes == 0 {
        return Ok(None);
    }

    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }

    Ok(Some(line))
}

fn advertise_capabilities(stream: &mut TcpStream, modules: &[ModuleRuntime]) -> io::Result<()> {
    for payload in advertised_capability_lines(modules) {
        let message = format_legacy_daemon_message(LegacyDaemonMessage::Capabilities {
            flags: payload.as_str(),
        });
        stream.write_all(message.as_bytes())?;
    }

    if modules.is_empty() {
        Ok(())
    } else {
        stream.flush()
    }
}

fn advertised_capability_lines(modules: &[ModuleRuntime]) -> Vec<String> {
    if modules.is_empty() {
        return Vec::new();
    }

    let mut features = Vec::with_capacity(2);
    features.push(String::from("modules"));

    if modules
        .iter()
        .any(|module| module.requires_authentication())
    {
        features.push(String::from("authlist"));
    }

    vec![features.join(" ")]
}

fn respond_with_module_list(
    stream: &mut TcpStream,
    limiter: &mut Option<BandwidthLimiter>,
    modules: &[ModuleRuntime],
    motd_lines: &[String],
    peer_ip: IpAddr,
    reverse_lookup: bool,
) -> io::Result<()> {
    for line in motd_lines {
        let payload = if line.is_empty() {
            "MOTD".to_string()
        } else {
            format!("MOTD {line}")
        };
        let message = format_legacy_daemon_message(LegacyDaemonMessage::Other(&payload));
        write_limited(stream, limiter, message.as_bytes())?;
    }

    let ok = format_legacy_daemon_message(LegacyDaemonMessage::Ok);
    write_limited(stream, limiter, ok.as_bytes())?;

    let mut hostname_cache: Option<Option<String>> = None;
    for module in modules {
        if !module.listable {
            continue;
        }

        let peer_host = module_peer_hostname(module, &mut hostname_cache, peer_ip, reverse_lookup);
        if !module.permits(peer_ip, peer_host) {
            continue;
        }

        let mut line = module.name.clone();
        if let Some(comment) = &module.comment {
            if !comment.is_empty() {
                line.push('\t');
                line.push_str(comment);
            }
        }
        line.push('\n');
        write_limited(stream, limiter, line.as_bytes())?;
    }

    let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
    write_limited(stream, limiter, exit.as_bytes())?;
    stream.flush()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthenticationStatus {
    Granted,
    Denied,
}

fn perform_module_authentication(
    reader: &mut BufReader<TcpStream>,
    limiter: &mut Option<BandwidthLimiter>,
    module: &ModuleDefinition,
    peer_ip: IpAddr,
) -> io::Result<AuthenticationStatus> {
    let challenge = generate_auth_challenge(peer_ip);
    {
        let stream = reader.get_mut();
        let message = format_legacy_daemon_message(LegacyDaemonMessage::AuthRequired {
            module: Some(&challenge),
        });
        write_limited(stream, limiter, message.as_bytes())?;
        stream.flush()?;
    }

    let response = match read_trimmed_line(reader)? {
        Some(line) => line,
        None => {
            deny_module(reader.get_mut(), module, peer_ip, limiter)?;
            return Ok(AuthenticationStatus::Denied);
        }
    };

    let mut segments = response.splitn(2, |ch: char| ch.is_ascii_whitespace());
    let username = segments.next().unwrap_or_default();
    let digest = segments
        .next()
        .map(|segment| segment.trim_start_matches(|ch: char| ch.is_ascii_whitespace()))
        .unwrap_or("");

    if username.is_empty() || digest.is_empty() {
        deny_module(reader.get_mut(), module, peer_ip, limiter)?;
        return Ok(AuthenticationStatus::Denied);
    }

    if !module.auth_users.iter().any(|user| user == username) {
        deny_module(reader.get_mut(), module, peer_ip, limiter)?;
        return Ok(AuthenticationStatus::Denied);
    }

    if !verify_secret_response(module, username, &challenge, digest)? {
        deny_module(reader.get_mut(), module, peer_ip, limiter)?;
        return Ok(AuthenticationStatus::Denied);
    }

    Ok(AuthenticationStatus::Granted)
}

fn generate_auth_challenge(peer_ip: IpAddr) -> String {
    let mut input = [0u8; 32];
    let address_text = peer_ip.to_string();
    let address_bytes = address_text.as_bytes();
    let copy_len = address_bytes.len().min(16);
    input[..copy_len].copy_from_slice(&address_bytes[..copy_len]);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = (timestamp.as_secs() & u64::from(u32::MAX)) as u32;
    let micros = timestamp.subsec_micros();
    let pid = std::process::id();

    input[16..20].copy_from_slice(&seconds.to_le_bytes());
    input[20..24].copy_from_slice(&micros.to_le_bytes());
    input[24..28].copy_from_slice(&pid.to_le_bytes());

    let mut hasher = Md5::new();
    hasher.update(&input);
    let digest = hasher.finalize();
    STANDARD_NO_PAD.encode(digest)
}

fn verify_secret_response(
    module: &ModuleDefinition,
    username: &str,
    challenge: &str,
    response: &str,
) -> io::Result<bool> {
    let secrets_path = match &module.secrets_file {
        Some(path) => path,
        None => return Ok(false),
    };

    let contents = fs::read_to_string(secrets_path)?;

    for raw_line in contents.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((user, secret)) = line.split_once(':') {
            if user == username {
                let expected = compute_auth_response(secret, challenge);
                return Ok(expected == response);
            }
        }
    }

    Ok(false)
}

fn compute_auth_response(secret: &str, challenge: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(secret.as_bytes());
    hasher.update(challenge.as_bytes());
    let digest = hasher.finalize();
    STANDARD_NO_PAD.encode(digest)
}

fn deny_module(
    stream: &mut TcpStream,
    module: &ModuleDefinition,
    peer_ip: IpAddr,
    limiter: &mut Option<BandwidthLimiter>,
) -> io::Result<()> {
    let module_display = sanitize_module_identifier(&module.name);
    let payload = ACCESS_DENIED_PAYLOAD
        .replace("{module}", module_display.as_ref())
        .replace("{addr}", &peer_ip.to_string());
    write_limited(stream, limiter, payload.as_bytes())?;
    write_limited(stream, limiter, b"\n")?;
    let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
    write_limited(stream, limiter, exit.as_bytes())?;
    stream.flush()
}

fn send_daemon_ok(
    stream: &mut TcpStream,
    limiter: &mut Option<BandwidthLimiter>,
) -> io::Result<()> {
    let ok = format_legacy_daemon_message(LegacyDaemonMessage::Ok);
    write_limited(stream, limiter, ok.as_bytes())?;
    stream.flush()
}

/// Applies the module-specific bandwidth directives to the active limiter.
///
/// The helper mirrors upstream rsync's precedence rules: a module `bwlimit`
/// directive overrides the daemon-wide limit with the strictest rate while
/// honouring explicitly configured bursts. When a module omits the directive
/// the limiter remains in the state established by the daemon scope, ensuring
/// clients observe inherited throttling exactly as the C implementation does.
/// The function returns the [`LimiterChange`] reported by
/// [`apply_effective_limit`], allowing callers and tests to verify whether the
/// limiter configuration changed as a result of the module overrides.
fn apply_module_bandwidth_limit(
    limiter: &mut Option<BandwidthLimiter>,
    module_limit: Option<NonZeroU64>,
    module_limit_specified: bool,
    module_limit_configured: bool,
    module_burst: Option<NonZeroU64>,
    module_burst_specified: bool,
) -> LimiterChange {
    if module_limit_configured && module_limit.is_none() {
        let burst_only_override =
            module_burst_specified && module_burst.is_some() && limiter.is_some();
        if !burst_only_override {
            return if limiter.take().is_some() {
                LimiterChange::Disabled
            } else {
                LimiterChange::Unchanged
            };
        }
    }

    let limit_specified =
        module_limit_specified || (module_limit_configured && module_limit.is_some());
    let burst_specified =
        module_burst_specified && (module_limit_configured || module_limit_specified);

    BandwidthLimitComponents::new_with_flags(
        module_limit,
        module_burst,
        limit_specified,
        burst_specified,
    )
    .apply_to_limiter(limiter)
}

#[allow(clippy::too_many_arguments)]
fn respond_with_module_request(
    reader: &mut BufReader<TcpStream>,
    limiter: &mut Option<BandwidthLimiter>,
    modules: &[ModuleRuntime],
    request: &str,
    peer_ip: IpAddr,
    session_peer_host: Option<&str>,
    options: &[String],
    log_sink: Option<&SharedLogSink>,
    reverse_lookup: bool,
) -> io::Result<()> {
    if let Some(module) = modules.iter().find(|module| module.name == request) {
        let change = apply_module_bandwidth_limit(
            limiter,
            module.bandwidth_limit(),
            module.bandwidth_limit_specified(),
            module.bandwidth_limit_configured(),
            module.bandwidth_burst(),
            module.bandwidth_burst_specified(),
        );

        let mut hostname_cache: Option<Option<String>> = None;
        let module_peer_host =
            module_peer_hostname(module, &mut hostname_cache, peer_ip, reverse_lookup);

        if change != LimiterChange::Unchanged {
            if let Some(log) = log_sink {
                log_module_bandwidth_change(
                    log,
                    module_peer_host.or(session_peer_host),
                    peer_ip,
                    request,
                    limiter.as_ref(),
                    change,
                );
            }
        }
        if module.permits(peer_ip, module_peer_host) {
            let _connection_guard = match module.try_acquire_connection() {
                Ok(guard) => guard,
                Err(ModuleConnectionError::Limit(limit)) => {
                    let payload =
                        MODULE_MAX_CONNECTIONS_PAYLOAD.replace("{limit}", &limit.get().to_string());
                    let stream = reader.get_mut();
                    write_limited(stream, limiter, payload.as_bytes())?;
                    write_limited(stream, limiter, b"\n")?;
                    let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
                    write_limited(stream, limiter, exit.as_bytes())?;
                    stream.flush()?;
                    if let Some(log) = log_sink {
                        log_module_limit(
                            log,
                            module_peer_host.or(session_peer_host),
                            peer_ip,
                            request,
                            limit,
                        );
                    }
                    return Ok(());
                }
                Err(ModuleConnectionError::Io(error)) => {
                    let stream = reader.get_mut();
                    write_limited(stream, limiter, MODULE_LOCK_ERROR_PAYLOAD.as_bytes())?;
                    write_limited(stream, limiter, b"\n")?;
                    let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
                    write_limited(stream, limiter, exit.as_bytes())?;
                    stream.flush()?;
                    if let Some(log) = log_sink {
                        log_module_lock_error(
                            log,
                            module_peer_host.or(session_peer_host),
                            peer_ip,
                            request,
                            &error,
                        );
                    }
                    return Ok(());
                }
            };

            if let Some(log) = log_sink {
                log_module_request(
                    log,
                    module_peer_host.or(session_peer_host),
                    peer_ip,
                    request,
                );
            }

            if let Some(refused) = refused_option(module, options) {
                let payload = format!("@ERROR: The server is configured to refuse {}", refused);
                let stream = reader.get_mut();
                write_limited(stream, limiter, payload.as_bytes())?;
                write_limited(stream, limiter, b"\n")?;
                let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
                write_limited(stream, limiter, exit.as_bytes())?;
                stream.flush()?;
                if let Some(log) = log_sink {
                    log_module_refused_option(
                        log,
                        module_peer_host.or(session_peer_host),
                        peer_ip,
                        request,
                        refused,
                    );
                }
                return Ok(());
            }

            apply_module_timeout(reader.get_mut(), module)?;
            let mut acknowledged = false;
            if module.requires_authentication() {
                match perform_module_authentication(reader, limiter, module, peer_ip)? {
                    AuthenticationStatus::Denied => {
                        if let Some(log) = log_sink {
                            log_module_auth_failure(
                                log,
                                module_peer_host.or(session_peer_host),
                                peer_ip,
                                request,
                            );
                        }
                        return Ok(());
                    }
                    AuthenticationStatus::Granted => {
                        if let Some(log) = log_sink {
                            log_module_auth_success(
                                log,
                                module_peer_host.or(session_peer_host),
                                peer_ip,
                                request,
                            );
                        }
                        send_daemon_ok(reader.get_mut(), limiter)?;
                        acknowledged = true;
                    }
                }
            }

            if !acknowledged {
                send_daemon_ok(reader.get_mut(), limiter)?;
            }

            let module_display = sanitize_module_identifier(request);
            let payload = MODULE_UNAVAILABLE_PAYLOAD.replace("{module}", module_display.as_ref());
            let stream = reader.get_mut();
            write_limited(stream, limiter, payload.as_bytes())?;
            write_limited(stream, limiter, b"\n")?;
            if let Some(log) = log_sink {
                log_module_unavailable(
                    log,
                    module_peer_host.or(session_peer_host),
                    peer_ip,
                    request,
                );
            }
        } else {
            if let Some(log) = log_sink {
                log_module_denied(
                    log,
                    module_peer_host.or(session_peer_host),
                    peer_ip,
                    request,
                );
            }
            deny_module(reader.get_mut(), module, peer_ip, limiter)?;
            return Ok(());
        }
    } else {
        let module_display = sanitize_module_identifier(request);
        let payload = UNKNOWN_MODULE_PAYLOAD.replace("{module}", module_display.as_ref());
        let stream = reader.get_mut();
        write_limited(stream, limiter, payload.as_bytes())?;
        write_limited(stream, limiter, b"\n")?;
        if let Some(log) = log_sink {
            log_unknown_module(log, session_peer_host, peer_ip, request);
        }
    }

    let exit = format_legacy_daemon_message(LegacyDaemonMessage::Exit);
    let stream = reader.get_mut();
    write_limited(stream, limiter, exit.as_bytes())?;
    stream.flush()
}

fn open_log_sink(path: &Path) -> Result<SharedLogSink, DaemonError> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| log_file_error(path, error))?;
    Ok(Arc::new(Mutex::new(MessageSink::new(file))))
}

fn log_file_error(path: &Path, error: io::Error) -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            format!("failed to open log file '{}': {}", path.display(), error)
        )
        .with_role(Role::Daemon),
    )
}

fn pid_file_error(path: &Path, error: io::Error) -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            format!("failed to write pid file '{}': {}", path.display(), error)
        )
        .with_role(Role::Daemon),
    )
}

fn lock_file_error(path: &Path, error: io::Error) -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            format!("failed to open lock file '{}': {}", path.display(), error)
        )
        .with_role(Role::Daemon),
    )
}

fn log_message(log: &SharedLogSink, message: &Message) {
    if let Ok(mut sink) = log.lock() {
        if sink.write(message).is_ok() {
            let _ = sink.flush();
        }
    }
}

fn format_host(host: Option<&str>, fallback: IpAddr) -> String {
    host.map(str::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

/// Returns a sanitised view of a module identifier suitable for diagnostics.
///
/// Module names originate from user input (daemon operands) or configuration
/// files. When composing diagnostics the value must not embed control
/// characters, otherwise adversarial requests could smuggle terminal control
/// sequences or split log lines. The helper replaces ASCII control characters
/// with a visible `'?'` marker while borrowing clean identifiers to avoid
/// unnecessary allocations.
fn sanitize_module_identifier(input: &str) -> Cow<'_, str> {
    if input.chars().all(|ch| !ch.is_control()) {
        return Cow::Borrowed(input);
    }

    let mut sanitized = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_control() {
            sanitized.push('?');
        } else {
            sanitized.push(ch);
        }
    }

    Cow::Owned(sanitized)
}

fn format_bandwidth_rate(value: NonZeroU64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;
    const PIB: u64 = TIB * 1024;

    let bytes = value.get();
    if bytes.is_multiple_of(PIB) {
        format!("{} PiB/s", bytes / PIB)
    } else if bytes.is_multiple_of(TIB) {
        format!("{} TiB/s", bytes / TIB)
    } else if bytes.is_multiple_of(GIB) {
        format!("{} GiB/s", bytes / GIB)
    } else if bytes.is_multiple_of(MIB) {
        format!("{} MiB/s", bytes / MIB)
    } else if bytes.is_multiple_of(KIB) {
        format!("{} KiB/s", bytes / KIB)
    } else {
        format!("{} bytes/s", bytes)
    }
}

fn log_module_bandwidth_change(
    log: &SharedLogSink,
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
    limiter: Option<&BandwidthLimiter>,
    change: LimiterChange,
) {
    if change == LimiterChange::Unchanged {
        return;
    }

    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);

    let message = match change {
        LimiterChange::Unchanged => return,
        LimiterChange::Disabled => {
            let text = format!(
                "removed bandwidth limit for module '{}' requested from {} ({})",
                module_display, display, peer_ip,
            );
            rsync_info!(text).with_role(Role::Daemon)
        }
        LimiterChange::Enabled | LimiterChange::Updated => {
            let Some(limiter) = limiter else {
                return;
            };
            let limit = format_bandwidth_rate(limiter.limit_bytes());
            let burst = limiter
                .burst_bytes()
                .map(|value| format!(" with burst {}", format_bandwidth_rate(value)))
                .unwrap_or_default();
            let action = match change {
                LimiterChange::Enabled => "enabled",
                LimiterChange::Updated => "updated",
                LimiterChange::Disabled | LimiterChange::Unchanged => unreachable!(),
            };
            let text = format!(
                "{action} bandwidth limit {limit}{burst} for module '{}' requested from {} ({})",
                module_display, display, peer_ip,
            );
            rsync_info!(text).with_role(Role::Daemon)
        }
    };

    log_message(log, &message);
}

fn log_connection(log: &SharedLogSink, host: Option<&str>, peer_addr: SocketAddr) {
    let display = format_host(host, peer_addr.ip());
    let text = format!("connect from {} ({})", display, peer_addr.ip());
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_list_request(log: &SharedLogSink, host: Option<&str>, peer_addr: SocketAddr) {
    let display = format_host(host, peer_addr.ip());
    let text = format!("list request from {} ({})", display, peer_addr.ip());
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_request(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "module '{}' requested from {} ({})",
        module_display, display, peer_ip
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_limit(
    log: &SharedLogSink,
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
    limit: NonZeroU32,
) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "refusing module '{}' from {} ({}): max connections {}",
        module_display, display, peer_ip, limit,
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_lock_error(
    log: &SharedLogSink,
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
    error: &io::Error,
) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "failed to update lock for module '{}' requested from {} ({}): {}",
        module_display, display, peer_ip, error
    );
    let message = rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_refused_option(
    log: &SharedLogSink,
    host: Option<&str>,
    peer_ip: IpAddr,
    module: &str,
    refused: &str,
) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "refusing option '{}' for module '{}' from {} ({})",
        refused, module_display, display, peer_ip,
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_auth_failure(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "authentication failed for module '{}' from {} ({})",
        module_display, display, peer_ip,
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_auth_success(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "authentication succeeded for module '{}' from {} ({})",
        module_display, display, peer_ip,
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_unavailable(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "module '{}' transfers unavailable for {} ({})",
        module_display, display, peer_ip,
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_module_denied(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "access denied to module '{}' from {} ({})",
        module_display, display, peer_ip,
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn log_unknown_module(log: &SharedLogSink, host: Option<&str>, peer_ip: IpAddr, module: &str) {
    let display = format_host(host, peer_ip);
    let module_display = sanitize_module_identifier(module);
    let text = format!(
        "unknown module '{}' requested from {} ({})",
        module_display, display, peer_ip,
    );
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

fn parse_daemon_option(payload: &str) -> Option<&str> {
    let (keyword, remainder) = payload.split_once(char::is_whitespace)?;
    if !keyword.eq_ignore_ascii_case("OPTION") {
        return None;
    }

    let option = remainder.trim();
    if option.is_empty() {
        None
    } else {
        Some(option)
    }
}

fn refused_option<'a>(module: &ModuleDefinition, options: &'a [String]) -> Option<&'a str> {
    options.iter().find_map(|candidate| {
        let canonical_candidate = canonical_option(candidate);
        module
            .refuse_options
            .iter()
            .map(String::as_str)
            .any(|refused| canonical_option(refused) == canonical_candidate)
            .then_some(candidate.as_str())
    })
}

fn canonical_option(text: &str) -> String {
    let token = text
        .trim()
        .trim_start_matches('-')
        .split([' ', '\t', '='])
        .next()
        .unwrap_or("");
    token.to_ascii_lowercase()
}

fn apply_module_timeout(stream: &TcpStream, module: &ModuleDefinition) -> io::Result<()> {
    if let Some(timeout) = module.timeout {
        let duration = Duration::from_secs(timeout.get());
        stream.set_read_timeout(Some(duration))?;
        stream.set_write_timeout(Some(duration))?;
    }

    Ok(())
}

fn missing_argument_value(option: &str) -> DaemonError {
    config_error(format!("missing value for {option}"))
}

fn parse_port(value: &OsString) -> Result<u16, DaemonError> {
    let text = value.to_string_lossy();
    text.parse::<u16>()
        .map_err(|_| config_error(format!("invalid value for --port: '{text}'")))
}

fn parse_bind_address(value: &OsString) -> Result<IpAddr, DaemonError> {
    let text = value.to_string_lossy();
    let trimmed = text.trim();
    let candidate = trimmed
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(trimmed);

    if let Ok(address) = candidate.parse::<IpAddr>() {
        return Ok(address);
    }

    lookup_host(candidate)
        .map_err(|_| config_error(format!("invalid bind address '{text}'")))?
        .into_iter()
        .next()
        .ok_or_else(|| config_error(format!("invalid bind address '{text}'")))
}

fn parse_max_sessions(value: &OsString) -> Result<NonZeroUsize, DaemonError> {
    let text = value.to_string_lossy();
    let parsed: usize = text
        .parse()
        .map_err(|_| config_error(format!("invalid value for --max-sessions: '{text}'")))?;
    NonZeroUsize::new(parsed)
        .ok_or_else(|| config_error("--max-sessions must be greater than zero".to_string()))
}

fn parse_module_definition(
    value: &OsString,
    default_secrets: Option<&Path>,
) -> Result<ModuleDefinition, DaemonError> {
    let text = value.to_string_lossy();
    let (name_part, remainder) = text.split_once('=').ok_or_else(|| {
        config_error(format!(
            "invalid module specification '{text}': expected NAME=PATH"
        ))
    })?;

    let name = name_part.trim();
    ensure_valid_module_name(name).map_err(|msg| config_error(msg.to_string()))?;

    let (path_part, comment_part, options_part) = split_module_path_comment_and_options(remainder);

    let path_text = path_part.trim();
    if path_text.is_empty() {
        return Err(config_error("module path must be non-empty".to_string()));
    }

    let path_text = unescape_module_component(path_text);
    let comment = comment_part
        .map(|value| unescape_module_component(value.trim()))
        .filter(|value| !value.is_empty());

    let mut module = ModuleDefinition {
        name: name.to_string(),
        path: PathBuf::from(&path_text),
        comment,
        hosts_allow: Vec::new(),
        hosts_deny: Vec::new(),
        auth_users: Vec::new(),
        secrets_file: None,
        bandwidth_limit: None,
        bandwidth_limit_specified: false,
        bandwidth_burst: None,
        bandwidth_burst_specified: false,
        bandwidth_limit_configured: false,
        refuse_options: Vec::new(),
        read_only: true,
        numeric_ids: false,
        uid: None,
        gid: None,
        timeout: None,
        listable: true,
        use_chroot: true,
        max_connections: None,
    };

    if let Some(options_text) = options_part {
        apply_inline_module_options(options_text, &mut module)?;
    }

    if module.use_chroot && !module.path.is_absolute() {
        return Err(config_error(format!(
            "module path '{}' must be absolute when 'use chroot' is enabled",
            path_text
        )));
    }

    if module.auth_users.is_empty() {
        if module.secrets_file.is_none() {
            if let Some(path) = default_secrets {
                module.secrets_file = Some(path.to_path_buf());
            }
        }
        return Ok(module);
    }

    if module.secrets_file.is_none() {
        if let Some(path) = default_secrets {
            module.secrets_file = Some(path.to_path_buf());
        } else {
            return Err(config_error(
                "module specified 'auth users' but did not supply a secrets file".to_string(),
            ));
        }
    }

    Ok(module)
}

fn split_module_path_comment_and_options(value: &str) -> (&str, Option<&str>, Option<&str>) {
    enum Segment {
        Path,
        Comment { start: usize },
    }

    let mut state = Segment::Path;
    let mut escape = false;

    for (idx, ch) in value.char_indices() {
        if escape {
            escape = false;
            continue;
        }

        match ch {
            '\\' => {
                escape = true;
            }
            ';' => {
                let options = value.get(idx + ch.len_utf8()..);
                return match state {
                    Segment::Path => {
                        let path = &value[..idx];
                        (path, None, options)
                    }
                    Segment::Comment { start } => {
                        let comment = value.get(start..idx);
                        let path = &value[..start - 1];
                        (path, comment, options)
                    }
                };
            }
            ',' => {
                if matches!(state, Segment::Path) {
                    state = Segment::Comment {
                        start: idx + ch.len_utf8(),
                    };
                }
            }
            _ => {}
        }
    }

    match state {
        Segment::Path => (value, None, None),
        Segment::Comment { start } => (&value[..start - 1], value.get(start..), None),
    }
}

fn split_inline_options(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut escape = false;

    for ch in text.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }

        match ch {
            '\\' => escape = true,
            ';' => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        parts.push(current.trim().to_string());
    }

    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

fn apply_inline_module_options(
    options: &str,
    module: &mut ModuleDefinition,
) -> Result<(), DaemonError> {
    let path = Path::new("--module");
    let mut seen = HashSet::new();

    for option in split_inline_options(options) {
        let (key_raw, value_raw) = option
            .split_once('=')
            .ok_or_else(|| config_error(format!("module option '{option}' is missing '='")))?;

        let key = key_raw.trim().to_ascii_lowercase();
        if !seen.insert(key.clone()) {
            return Err(config_error(format!("duplicate module option '{key_raw}'")));
        }

        let value = value_raw.trim();
        match key.as_str() {
            "read only" | "read-only" => {
                let parsed = parse_boolean_directive(value).ok_or_else(|| {
                    config_error(format!("invalid boolean value '{value}' for 'read only'"))
                })?;
                module.read_only = parsed;
            }
            "list" => {
                let parsed = parse_boolean_directive(value).ok_or_else(|| {
                    config_error(format!("invalid boolean value '{value}' for 'list'"))
                })?;
                module.listable = parsed;
            }
            "numeric ids" | "numeric-ids" => {
                let parsed = parse_boolean_directive(value).ok_or_else(|| {
                    config_error(format!("invalid boolean value '{value}' for 'numeric ids'"))
                })?;
                module.numeric_ids = parsed;
            }
            "use chroot" | "use-chroot" => {
                let parsed = parse_boolean_directive(value).ok_or_else(|| {
                    config_error(format!("invalid boolean value '{value}' for 'use chroot'"))
                })?;
                module.use_chroot = parsed;
            }
            "hosts allow" | "hosts-allow" => {
                let patterns = parse_host_list(value, path, 0, "hosts allow")?;
                module.hosts_allow = patterns;
            }
            "hosts deny" | "hosts-deny" => {
                let patterns = parse_host_list(value, path, 0, "hosts deny")?;
                module.hosts_deny = patterns;
            }
            "auth users" | "auth-users" => {
                let users = parse_auth_user_list(value).map_err(|error| {
                    config_error(format!("invalid 'auth users' directive: {error}"))
                })?;
                if users.is_empty() {
                    return Err(config_error(
                        "'auth users' option must list at least one user".to_string(),
                    ));
                }
                module.auth_users = users;
            }
            "secrets file" | "secrets-file" => {
                if value.is_empty() {
                    return Err(config_error(
                        "'secrets file' option must not be empty".to_string(),
                    ));
                }
                module.secrets_file = Some(PathBuf::from(unescape_module_component(value)));
            }
            "bwlimit" => {
                if value.is_empty() {
                    return Err(config_error(
                        "'bwlimit' option must not be empty".to_string(),
                    ));
                }
                let components = parse_runtime_bwlimit(&OsString::from(value))?;
                module.bandwidth_limit = components.rate();
                module.bandwidth_burst = components.burst();
                module.bandwidth_burst_specified = components.burst_specified();
                module.bandwidth_limit_specified = true;
                module.bandwidth_limit_configured = true;
            }
            "refuse options" | "refuse-options" => {
                let options = parse_refuse_option_list(value).map_err(|error| {
                    config_error(format!("invalid 'refuse options' directive: {error}"))
                })?;
                module.refuse_options = options;
            }
            "uid" => {
                let uid = parse_numeric_identifier(value)
                    .ok_or_else(|| config_error(format!("invalid uid '{value}'")))?;
                module.uid = Some(uid);
            }
            "gid" => {
                let gid = parse_numeric_identifier(value)
                    .ok_or_else(|| config_error(format!("invalid gid '{value}'")))?;
                module.gid = Some(gid);
            }
            "timeout" => {
                let timeout = parse_timeout_seconds(value)
                    .ok_or_else(|| config_error(format!("invalid timeout '{value}'")))?;
                module.timeout = timeout;
            }
            "max connections" | "max-connections" => {
                let max = parse_max_connections_directive(value).ok_or_else(|| {
                    config_error(format!("invalid max connections value '{value}'"))
                })?;
                module.max_connections = max;
            }
            _ => {
                return Err(config_error(format!(
                    "unsupported module option '{key_raw}'"
                )));
            }
        }
    }

    Ok(())
}

fn unescape_module_component(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                result.push(next);
            } else {
                result.push(ch);
            }
        } else {
            result.push(ch);
        }
    }
    result
}

fn parse_runtime_bwlimit(value: &OsString) -> Result<BandwidthLimitComponents, DaemonError> {
    let text = value.to_string_lossy();
    match parse_bandwidth_limit(&text) {
        Ok(components) => Ok(components),
        Err(error) => Err(runtime_bwlimit_error(&text, error)),
    }
}

fn parse_config_bwlimit(
    value: &str,
    path: &Path,
    line: usize,
) -> Result<BandwidthLimitComponents, DaemonError> {
    match parse_bandwidth_limit(value) {
        Ok(components) => Ok(components),
        Err(error) => Err(config_bwlimit_error(path, line, value, error)),
    }
}

fn runtime_bwlimit_error(value: &str, error: BandwidthParseError) -> DaemonError {
    let text = match error {
        BandwidthParseError::Invalid => format!("--bwlimit={} is invalid", value),
        BandwidthParseError::TooSmall => format!(
            "--bwlimit={} is too small (min: 512 or 0 for unlimited)",
            value
        ),
        BandwidthParseError::TooLarge => format!("--bwlimit={} is too large", value),
    };
    config_error(text)
}

fn config_bwlimit_error(
    path: &Path,
    line: usize,
    value: &str,
    error: BandwidthParseError,
) -> DaemonError {
    let detail = match error {
        BandwidthParseError::Invalid => format!("invalid 'bwlimit' value '{value}'"),
        BandwidthParseError::TooSmall => {
            format!("'bwlimit' value '{value}' is too small (min: 512 or 0 for unlimited)")
        }
        BandwidthParseError::TooLarge => format!("'bwlimit' value '{value}' is too large"),
    };
    config_parse_error(path, line, detail)
}

fn unsupported_option(option: OsString) -> DaemonError {
    let text = format!("unsupported daemon argument '{}'", option.to_string_lossy());
    config_error(text)
}

fn config_error(text: String) -> DaemonError {
    let message = Message::error(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon);
    DaemonError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
}

fn secrets_env_error(env: &'static str, path: &Path, detail: impl Into<String>) -> DaemonError {
    config_error(format!(
        "environment variable {env} points to invalid secrets file '{}': {}",
        path.display(),
        detail.into()
    ))
}

fn config_parse_error(path: &Path, line: usize, message: impl Into<String>) -> DaemonError {
    let text = format!(
        "failed to parse config '{}': {} (line {})",
        path.display(),
        message.into(),
        line
    );
    let message = Message::error(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon);
    DaemonError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
}

fn config_io_error(action: &str, path: &Path, error: io::Error) -> DaemonError {
    let text = format!("failed to {action} config '{}': {error}", path.display());
    let message = Message::error(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon);
    DaemonError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
}

fn ensure_valid_module_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("module name must be non-empty and cannot contain whitespace");
    }

    if name
        .chars()
        .any(|ch| ch.is_whitespace() || ch == '/' || ch == '\\')
    {
        return Err("module name cannot contain whitespace or path separators");
    }

    Ok(())
}

fn duplicate_argument(option: &str) -> DaemonError {
    config_error(format!("duplicate daemon argument '{option}'"))
}

fn duplicate_module(name: &str) -> DaemonError {
    config_error(format!("duplicate module definition '{name}'"))
}

fn bind_error(address: SocketAddr, error: io::Error) -> DaemonError {
    network_error("bind listener", address, error)
}

fn accept_error(address: SocketAddr, error: io::Error) -> DaemonError {
    network_error("accept connection on", address, error)
}

fn stream_error(peer: Option<SocketAddr>, action: &str, error: io::Error) -> DaemonError {
    match peer {
        Some(addr) => network_error(action, addr, error),
        None => network_error(action, "connection", error),
    }
}

fn network_error<T: fmt::Display>(action: &str, target: T, error: io::Error) -> DaemonError {
    let text = format!("failed to {action} {target}: {error}");
    let message = Message::error(SOCKET_IO_EXIT_CODE, text).with_role(Role::Daemon);
    DaemonError::new(SOCKET_IO_EXIT_CODE, message)
}

pub(crate) fn configured_fallback_binary() -> Option<OsString> {
    if let Some(selection) = fallback_override(DAEMON_FALLBACK_ENV) {
        return selection.resolve_or_default(OsStr::new(Brand::Upstream.client_program_name()));
    }

    if let Some(selection) = fallback_override(CLIENT_FALLBACK_ENV) {
        return selection.resolve_or_default(OsStr::new(Brand::Upstream.client_program_name()));
    }

    Some(OsString::from(Brand::Upstream.client_program_name()))
}

#[cfg(test)]
mod tests;
