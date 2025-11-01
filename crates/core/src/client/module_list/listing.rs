use std::io::{BufReader, Write};
use std::time::Duration;

use rsync_protocol::{
    LEGACY_DAEMON_PREFIX, LegacyDaemonMessage, parse_legacy_daemon_message,
    parse_legacy_warning_message,
};
use rsync_transport::negotiate_legacy_daemon_session;

use super::super::{
    ClientError, DAEMON_SOCKET_TIMEOUT, PARTIAL_TRANSFER_EXIT_CODE, TransferTimeout,
    daemon_access_denied_error, daemon_authentication_failed_error,
    daemon_authentication_required_error, daemon_error, daemon_listing_unavailable_error,
    daemon_protocol_error, socket_error,
};
use super::auth::{
    DaemonAuthContext, SensitiveBytes, is_motd_payload, load_daemon_password,
    normalize_motd_payload, send_daemon_auth_credentials,
};
use super::connect::{open_daemon_stream, resolve_connect_timeout};
use super::errors::{legacy_daemon_error_payload, map_daemon_handshake_error, read_trimmed_line};
use super::request::{ModuleListOptions, ModuleListRequest};

/// Collection of daemon modules together with MOTD, warnings, and capabilities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleList {
    motd: Vec<String>,
    warnings: Vec<String>,
    capabilities: Vec<String>,
    entries: Vec<ModuleListEntry>,
}

impl ModuleList {
    fn new(
        motd: Vec<String>,
        warnings: Vec<String>,
        capabilities: Vec<String>,
        entries: Vec<ModuleListEntry>,
    ) -> Self {
        Self {
            motd,
            warnings,
            capabilities,
            entries,
        }
    }

    /// Returns the advertised module entries.
    #[must_use]
    pub fn entries(&self) -> &[ModuleListEntry] {
        &self.entries
    }

    /// Returns the optional message-of-the-day lines emitted by the daemon.
    #[must_use]
    pub fn motd_lines(&self) -> &[String] {
        &self.motd
    }

    /// Returns the warning messages emitted by the daemon while processing the request.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Returns the capability strings advertised by the daemon via `@RSYNCD: CAP` lines.
    #[must_use]
    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }
}

/// Entry describing a single daemon module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleListEntry {
    name: String,
    comment: Option<String>,
}

impl ModuleListEntry {
    fn from_line(line: &str) -> Self {
        match line.split_once('\t') {
            Some((name, comment)) => Self {
                name: name.to_string(),
                comment: if comment.is_empty() {
                    None
                } else {
                    Some(comment.to_string())
                },
            },
            None => Self {
                name: line.to_string(),
                comment: None,
            },
        }
    }

    /// Returns the module name advertised by the daemon.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the optional comment associated with the module.
    #[must_use]
    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }
}

/// Performs a daemon module listing by connecting to the supplied address.
///
/// The helper honours the `RSYNC_PROXY` environment variable, establishing an
/// HTTP `CONNECT` tunnel through the specified proxy before negotiating with
/// the daemon when the variable is set. This mirrors the behaviour of
/// upstream rsync.
pub fn run_module_list(request: ModuleListRequest) -> Result<ModuleList, ClientError> {
    run_module_list_with_options(request, ModuleListOptions::default())
}

/// Performs a daemon module listing using caller-provided options.
///
/// This variant mirrors [`run_module_list`] while allowing callers to configure
/// behaviours such as suppressing daemon MOTD lines when `--no-motd` is supplied.
pub fn run_module_list_with_options(
    request: ModuleListRequest,
    options: ModuleListOptions,
) -> Result<ModuleList, ClientError> {
    run_module_list_with_password_and_options(
        request,
        options,
        None,
        TransferTimeout::Default,
        TransferTimeout::Default,
    )
}

/// Performs a daemon module listing using an optional password override.
///
/// When `password_override` is `Some`, the bytes are used for authentication
/// instead of loading `RSYNC_PASSWORD`. This mirrors `--password-file` in the
/// CLI and simplifies testing by avoiding environment manipulation.
pub fn run_module_list_with_password(
    request: ModuleListRequest,
    password_override: Option<Vec<u8>>,
    timeout: TransferTimeout,
) -> Result<ModuleList, ClientError> {
    run_module_list_with_password_and_options(
        request,
        ModuleListOptions::default(),
        password_override,
        timeout,
        TransferTimeout::Default,
    )
}

/// Performs a daemon module listing with the supplied options and password override.
///
/// The helper is primarily used by the CLI to honour flags such as `--no-motd`
/// while still exercising the optional password override path used for
/// `--password-file`. The [`ModuleListOptions`] parameter defaults to the same
/// behaviour as [`run_module_list`].
pub fn run_module_list_with_password_and_options(
    request: ModuleListRequest,
    options: ModuleListOptions,
    password_override: Option<Vec<u8>>,
    timeout: TransferTimeout,
    connect_timeout: TransferTimeout,
) -> Result<ModuleList, ClientError> {
    let addr = request.address();
    let username = request.username().map(str::to_owned);
    let mut password_bytes = password_override.map(SensitiveBytes::new);
    let mut auth_attempted = false;
    let mut auth_context: Option<DaemonAuthContext> = None;
    let suppress_motd = options.suppresses_motd();
    let address_mode = options.address_mode();

    let effective_timeout = effective_timeout(timeout, DAEMON_SOCKET_TIMEOUT);
    let connect_duration = resolve_connect_timeout(connect_timeout, timeout, DAEMON_SOCKET_TIMEOUT);

    let stream = open_daemon_stream(
        addr,
        connect_duration,
        effective_timeout,
        address_mode,
        options.connect_program(),
        options.bind_address(),
    )?;

    let handshake = negotiate_legacy_daemon_session(stream, request.protocol())
        .map_err(|error| map_daemon_handshake_error(error, addr))?;
    let mut reader = BufReader::new(handshake.into_stream());

    reader
        .get_mut()
        .write_all(b"#list\n")
        .map_err(|error| socket_error("write to", addr.socket_addr_display(), error))?;
    reader
        .get_mut()
        .flush()
        .map_err(|error| socket_error("flush", addr.socket_addr_display(), error))?;

    let mut entries = Vec::new();
    let mut motd = Vec::new();
    let mut warnings = Vec::new();
    let mut capabilities = Vec::new();
    let mut acknowledged = false;
    let mut pre_ack_messages = Vec::new();

    while let Some(line) = read_trimmed_line(&mut reader)
        .map_err(|error| socket_error("read from", addr.socket_addr_display(), error))?
    {
        if let Some(payload) = legacy_daemon_error_payload(&line) {
            return Err(daemon_error(payload, PARTIAL_TRANSFER_EXIT_CODE));
        }

        if let Some(payload) = parse_legacy_warning_message(&line) {
            warnings.push(payload.to_string());
            continue;
        }

        if line.starts_with(LEGACY_DAEMON_PREFIX) {
            match parse_legacy_daemon_message(&line) {
                Ok(LegacyDaemonMessage::Ok) => {
                    acknowledged = true;
                    continue;
                }
                Ok(LegacyDaemonMessage::Exit) => break,
                Ok(LegacyDaemonMessage::Capabilities { flags }) => {
                    capabilities.push(flags.to_string());
                    continue;
                }
                Ok(LegacyDaemonMessage::AuthRequired { module }) => {
                    if auth_attempted {
                        return Err(daemon_protocol_error(
                            "daemon repeated authentication challenge",
                        ));
                    }

                    let username = username.as_deref().ok_or_else(|| {
                        daemon_authentication_required_error(
                            "supply a username in the daemon URL (e.g. rsync://user@host/)",
                        )
                    })?;

                    let secret = if let Some(secret) = password_bytes.as_ref() {
                        secret.to_vec()
                    } else {
                        password_bytes = load_daemon_password().map(SensitiveBytes::new);
                        password_bytes
                            .as_ref()
                            .map(SensitiveBytes::to_vec)
                            .ok_or_else(|| {
                                daemon_authentication_required_error(
                                    "set RSYNC_PASSWORD before contacting authenticated daemons",
                                )
                            })?
                    };

                    let context = DaemonAuthContext::new(username.to_owned(), secret);
                    if let Some(challenge) = module {
                        send_daemon_auth_credentials(&mut reader, &context, challenge, addr)?;
                    }

                    auth_context = Some(context);
                    auth_attempted = true;
                    continue;
                }
                Ok(LegacyDaemonMessage::AuthChallenge { challenge }) => {
                    let context = auth_context.as_ref().ok_or_else(|| {
                        daemon_protocol_error(
                            "daemon issued authentication challenge before requesting credentials",
                        )
                    })?;

                    send_daemon_auth_credentials(&mut reader, context, challenge, addr)?;
                    continue;
                }
                Ok(LegacyDaemonMessage::Other(payload)) => {
                    if let Some(reason) = payload.strip_prefix("DENIED") {
                        return Err(daemon_access_denied_error(reason.trim()));
                    }

                    if let Some(reason) = payload.strip_prefix("AUTHFAILED") {
                        let reason = reason.trim();
                        return Err(daemon_authentication_failed_error(if reason.is_empty() {
                            None
                        } else {
                            Some(reason)
                        }));
                    }

                    if is_motd_payload(payload) {
                        if !suppress_motd {
                            motd.push(normalize_motd_payload(payload));
                        }
                        continue;
                    }

                    if !acknowledged {
                        pre_ack_messages.push(payload.to_string());
                        continue;
                    }

                    return Err(daemon_protocol_error(&line));
                }
                Ok(LegacyDaemonMessage::Version(_)) => {
                    return Err(daemon_protocol_error(&line));
                }
                Err(_) => {
                    return Err(daemon_protocol_error(&line));
                }
            }
        }

        if !acknowledged {
            return Err(daemon_protocol_error(&line));
        }

        entries.push(ModuleListEntry::from_line(&line));
    }

    if !acknowledged {
        if !pre_ack_messages.is_empty() {
            let mut detail = pre_ack_messages.join("\n");
            if detail.is_empty() {
                detail = String::from("daemon closed connection before acknowledging module list");
            }
            return Err(daemon_listing_unavailable_error(&detail));
        }

        return Err(daemon_protocol_error(
            "daemon did not acknowledge module listing",
        ));
    }

    Ok(ModuleList::new(motd, warnings, capabilities, entries))
}

fn effective_timeout(timeout: TransferTimeout, default: Duration) -> Option<Duration> {
    match timeout {
        TransferTimeout::Default => Some(default),
        TransferTimeout::Disabled => None,
        TransferTimeout::Seconds(value) => Some(Duration::from_secs(value.get())),
    }
}
