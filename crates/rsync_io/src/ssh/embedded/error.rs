//! Error types for the embedded SSH transport.
//!
//! Covers every failure mode in the SSH connection lifecycle: URL parsing,
//! TCP connect, host key verification, authentication, key loading, and
//! I/O during data transfer.

/// Errors from the embedded SSH transport layer.
///
/// Each variant maps to a distinct failure mode in the SSH connection
/// lifecycle - from URL parsing through authentication to data transfer.
#[derive(Debug, thiserror::Error)]
pub enum SshError {
    /// SSH protocol or connection error from the russh library.
    #[error("SSH connection error: {0}")]
    Connect(#[from] russh::Error),

    /// The TCP connection to the server could not be made.
    ///
    /// upstream: openssh/sshconnect.c:525-526 `ssh_connect_direct()`.
    #[error("ssh: connect to host {host} port {port}: {reason}")]
    ConnectFailed {
        /// The host as given on the command line (after `HostName`).
        host: String,
        /// The port dialled.
        port: u16,
        /// `strerror(errno)` of the failed `connect()`.
        reason: String,
    },

    /// The server accepted the TCP connection but sent no identification
    /// string within `ConnectTimeout`.
    ///
    /// upstream: openssh/kex.c:1302-1303 `kex_exchange_identification()`,
    /// then openssh/packet.c:2005-2008 `sshpkt_vfatal()` for
    /// `SSH_ERR_CONN_TIMEOUT`.
    #[error("{}", lines(&banner_timeout_messages(.remote)))]
    BannerTimeout {
        /// The peer as upstream's `remote_id` prints it: `<ip> port <port>`.
        remote: String,
    },

    /// The server closed the connection before the session was set up.
    ///
    /// upstream: openssh/packet.c:2002-2004 `sshpkt_vfatal()` for
    /// `SSH_ERR_CONN_CLOSED`.
    #[error("Connection closed by {remote}")]
    ConnectionClosed {
        /// The peer as upstream's `remote_id` prints it: `<ip> port <port>`.
        remote: String,
    },

    /// Every authentication method was exhausted without success.
    ///
    /// upstream: openssh/sshconnect2.c:541-543 `userauth()`.
    #[error("{user}@{host}: Permission denied ({methods}).")]
    AuthenticationFailed {
        /// The remote user name.
        user: String,
        /// The host as given on the command line (after `HostName`).
        host: String,
        /// The method list the server last advertised, comma-separated.
        methods: String,
    },

    /// The server's host key differs from the key `known_hosts` records.
    ///
    /// upstream: openssh/sshconnect.c:1280-1333 `check_host_key()`
    /// `HOST_CHANGED`, openssh/sshconnect.c:1669-1688 `warn_changed_key()`,
    /// then openssh/sshconnect2.c:102-103 `verify_host_key_callback()`.
    #[error("{}", lines(&changed_host_key_messages(.host, .changed)))]
    HostKeyMismatch {
        /// `hostfile_hostname`: `HostKeyAlias`, else `[host]:port` (bare
        /// host on port 22).
        host: String,
        /// The offending entry and the key the server sent.
        changed: Box<ChangedHostKey>,
    },

    /// The server's host key is not in `known_hosts` and
    /// `StrictHostKeyChecking yes` forbids learning it.
    ///
    /// upstream: openssh/sshconnect.c:1169-1178 `check_host_key()`, then
    /// openssh/sshconnect2.c:102-103.
    #[error("{}", lines(&unknown_host_messages(.host, .key_type)))]
    UnknownHost {
        /// `hostfile_hostname`, as for [`SshError::HostKeyMismatch`].
        host: String,
        /// `sshkey_type()` of the server's key.
        key_type: &'static str,
    },

    /// Host key verification failed without a reason line of its own: the
    /// key was unknown under `StrictHostKeyChecking ask` and no answer
    /// accepted it.
    ///
    /// upstream: openssh/sshconnect2.c:102-103.
    #[error("Host key verification failed.")]
    HostKeyVerificationFailed,

    /// The server's host key is listed in the `RevokedHostKeys` file.
    ///
    /// upstream: openssh/sshconnect.c:1517-1528 `verify_host_key()` - a key
    /// found in the revoked list is refused before any `known_hosts` check,
    /// under every `StrictHostKeyChecking` mode. A revoked key is never
    /// learned or accepted, so the connection is refused rather than
    /// downgraded. openssh/sshconnect2.c:102-103 adds the final line.
    #[error("{}", lines(&revoked_host_key_messages(.key_type, .fingerprint, .file)))]
    HostKeyRevoked {
        /// `sshkey_type()` of the server's key.
        key_type: &'static str,
        /// The server key's `SHA256:` fingerprint.
        fingerprint: String,
        /// The `RevokedHostKeys` file, as configured.
        file: String,
    },

    /// Server authenticated with an OpenSSH host certificate.
    ///
    /// A certificate replaces the plain host-key check rather than adding to
    /// it, so it can only be trusted against a certificate authority the
    /// client was told to trust. `known_hosts` `@cert-authority` lines are not
    /// consulted here, so no CA is ever trusted and the connection is refused
    /// instead of falling back to the key inside the certificate - that key is
    /// not something the client was asked to trust.
    #[error("host certificate presented by {host}, but certificate authorities are not configured")]
    HostCertificateUnsupported {
        /// Hostname or IP that presented the certificate.
        host: String,
    },

    /// Failed to parse an ssh:// URL.
    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),

    /// Underlying I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Failed to load an SSH private key.
    #[error("key load error: {0}")]
    KeyLoad(String),

    /// The SSH goodbye phase did not complete within the configured budget.
    ///
    /// Surfaced when the local side has finished writing the rsync protocol
    /// stream and dropped its writer half, but the remote channel never
    /// signals EOF and the bridge task that drives `russh::Channel::wait()`
    /// does not exit within [`super::connect::SSH_GOODBYE_TIMEOUT`].
    /// Guards against the v0.6.1 200x-slowdown class of regression in which a
    /// deadlocked shutdown surfaces as a multi-minute hang rather than a
    /// typed failure.
    #[error("SSH goodbye phase timed out after {elapsed:?}")]
    GoodbyePhaseTimeout {
        /// Wall-clock duration the local side waited for the remote to ack
        /// EOF before giving up.
        elapsed: std::time::Duration,
    },

    /// The URL was syntactically valid but semantically invalid for SSH.
    #[error("invalid SSH URL: {reason}")]
    InvalidUrl {
        /// Description of why the URL is invalid.
        reason: String,
    },

    /// The default `~/.ssh/config` failed upstream's owner/permission
    /// check.
    ///
    /// upstream: openssh/readconf.c:2579-2587 - under `SSHCONF_CHECKPERM`
    /// (openssh/readconf.h:216) the file must be owned by root or the
    /// caller and must not be group/world-writable, and a failure is
    /// fatal, so the connection never starts. The check applies to the
    /// default user file only (openssh/ssh.c:583) - never to an explicit
    /// `-F` file or to the system file. The rendered text is upstream's
    /// own fatal wording.
    #[error("Bad owner or permissions on {path}")]
    SshConfigPermissions {
        /// The config file that failed the check.
        path: String,
    },

    /// One `ssh_config` line refused while resolving the host alias.
    ///
    /// Upstream counts such a line and keeps scanning
    /// (openssh/readconf.c:2604-2607); the reader collects every one into
    /// [`SshError::SshConfigBadOptions`] once the file ends. Outside that
    /// scan it is returned on its own.
    ///
    /// The rendered text reproduces upstream's `%s line %d: <reason>`
    /// shape so an operator comparing the two diagnostics sees the same
    /// words.
    #[error("{path} line {line}: {reason}")]
    SshConfig {
        /// The config file the refused line came from.
        path: String,
        /// 1-based physical line number, as upstream counts them
        /// (openssh/readconf.c:2654 `linenum++` per `getline`).
        line: usize,
        /// Upstream's reason text for this refusal.
        reason: String,
    },
    /// An `ssh_config` file held lines upstream refuses.
    ///
    /// Upstream reports each refused line as it parses it, finishes the
    /// file, then aborts the whole load (openssh/readconf.c:2611-2613
    /// `fatal("%s: terminating, %d bad configuration options")`), so the
    /// connection never starts. oc mirrors that rather than resolving to
    /// connection parameters `ssh` would never have used. The rendered text
    /// is every line's diagnostic in file order, then upstream's fatal line.
    #[error("{}{path}: terminating, {} bad configuration options", render_lines(.lines), .lines.len())]
    SshConfigBadOptions {
        /// The config file the refused lines came from.
        path: String,
        /// Each refused line's diagnostic, in file order.
        lines: Vec<String>,
    },
    /// An `ssh_config` condition upstream treats as immediately fatal, such
    /// as `Include` nesting past its depth limit
    /// (openssh/readconf.c:2573-2574).
    #[error("{reason}")]
    SshConfigFatal {
        /// Upstream's fatal wording.
        reason: String,
    },

    /// The host does not resolve, reported as ssh reports it
    /// (openssh/ssh.c:274-276).
    #[error("ssh: Could not resolve hostname {host}: {reason}")]
    HostUnresolvable {
        /// The host as given, at most 100 characters (`%.100s`).
        host: String,
        /// The resolver's `gai_strerror` text.
        reason: String,
    },

    /// DNS resolution produced no addresses matching the IP version preference.
    #[error("no {preference} addresses found for {host}")]
    DnsResolution {
        /// Hostname that was resolved.
        host: String,
        /// IP version preference that filtered out all results.
        preference: String,
    },

    /// A percent token or `${ENV}` expansion in an ssh_config value could
    /// not be performed.
    ///
    /// Upstream is fatal at both expansion layers: an unknown `%` key or a
    /// trailing `%` aborts inside the expander (openssh/misc.c:1345-1363),
    /// and a failed `${ENV}` substitution aborts at the client call sites
    /// (openssh/ssh.c:240-241 `invalid environment variable expansion`).
    /// The reason text carries upstream's own wording so the operator sees
    /// the same diagnosis.
    #[error("ssh_config token expansion failed for {option}: {reason}")]
    TokenExpansion {
        /// The directive whose value was being expanded.
        option: String,
        /// Upstream's reason wording for the refusal.
        reason: String,
    },

    /// A `ProxyCommand` (or a `ProxyJump` lowered into one) could not be
    /// spawned, or its stdin/stdout could not be captured.
    ///
    /// Mirrors the failure upstream reports when its proxy fork/exec fails
    /// (openssh/sshconnect.c:222-286 `ssh_proxy_connect`): the connection
    /// never starts rather than silently falling back to a direct dial.
    #[error("proxy command failed: {reason}")]
    ProxyCommand {
        /// What went wrong spawning the command or capturing its stdio.
        reason: String,
    },

    /// A `ProxyCommand`/`ProxyJump` referenced a percent token the embedded
    /// transport does not expand.
    ///
    /// Only the connection tokens `%h`, `%p`, `%r` and the literal `%%` are
    /// supported (upstream's `ssh_proxy_connect` expands the same
    /// connection set, openssh/sshconnect.c:236-241). An unknown token is
    /// refused loudly rather than passed to the shell verbatim, since a
    /// stray `%n`/`%C` would otherwise reach the command unexpanded.
    #[error("unsupported percent token %{token} in proxy command")]
    ProxyTokenUnsupported {
        /// The character following the unhandled `%`.
        token: char,
    },

    /// `ProxyUseFdpass yes` was requested with an active `ProxyCommand`.
    ///
    /// Upstream's fd-passing variant expects the command to hand back a
    /// connected socket over its stdout via `SCM_RIGHTS`
    /// (openssh/sshconnect.c:151-210 `ssh_proxy_fdpass_connect`). The
    /// embedded transport dials russh over a byte stream and has no way to
    /// receive a passed descriptor, so the request is refused rather than
    /// run as an ordinary stdio proxy - which would misinterpret a
    /// descriptor-passing helper's protocol.
    #[error(
        "ProxyUseFdpass is not supported by the embedded SSH transport; \
         drop it or use an external ssh binary"
    )]
    ProxyUseFdpassUnsupported,
}

/// Renders each refused line on its own line, ahead of the fatal summary.
fn render_lines(lines: &[String]) -> String {
    lines.iter().map(|line| format!("{line}\n")).collect()
}

/// A `known_hosts` entry that records a different key than the server sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedHostKey {
    /// `sshkey_type()` of the server's key.
    pub key_type: &'static str,
    /// The server key's `SHA256:` fingerprint.
    pub fingerprint: String,
    /// The file `Add correct host key in` names: the first user
    /// `known_hosts` file, else the first system one; `None` when neither
    /// list has a file.
    pub add_to: Option<String>,
    /// The file holding the offending entry.
    pub offending_file: String,
    /// The offending entry's 1-based line number.
    pub offending_line: usize,
}

/// upstream: openssh/sshconnect2.c:103 `fatal("Host key verification failed.")`.
const HOST_KEY_VERIFICATION_FAILED: &str = "Host key verification failed.";

/// Joins log messages for `Display`, one per line.
fn lines(messages: &[String]) -> String {
    messages.join("\n")
}

fn banner_timeout_messages(remote: &str) -> Vec<String> {
    vec![
        "Connection timed out during banner exchange".to_owned(),
        format!("Connection to {remote} timed out"),
    ]
}

fn unknown_host_messages(host: &str, key_type: &str) -> Vec<String> {
    vec![
        format!(
            "No {key_type} host key is known for {host} and you have requested strict checking."
        ),
        HOST_KEY_VERIFICATION_FAILED.to_owned(),
    ]
}

fn revoked_host_key_messages(key_type: &str, fingerprint: &str, file: &str) -> Vec<String> {
    vec![
        format!("Host key {key_type} {fingerprint} revoked by file {file}"),
        HOST_KEY_VERIFICATION_FAILED.to_owned(),
    ]
}

/// One message per upstream `error()`/`fatal()` call. The fingerprint
/// message carries an embedded newline, as `warn_changed_key()`'s format
/// does (openssh/sshconnect.c:1684).
fn changed_host_key_messages(host: &str, changed: &ChangedHostKey) -> Vec<String> {
    let banner = "@".repeat(59);
    let mut messages = vec![
        banner.clone(),
        "@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @".to_owned(),
        banner,
        "IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!".to_owned(),
        "Someone could be eavesdropping on you right now (man-in-the-middle attack)!".to_owned(),
        "It is also possible that a host key has just been changed.".to_owned(),
        format!(
            "The fingerprint for the {} key sent by the remote host is\n{}.",
            changed.key_type, changed.fingerprint
        ),
        "Please contact your system administrator.".to_owned(),
    ];
    if let Some(add_to) = &changed.add_to {
        messages.push(format!(
            "Add correct host key in {add_to} to get rid of this message."
        ));
    }
    messages.push(format!(
        "Offending {} key in {}:{}",
        changed.key_type, changed.offending_file, changed.offending_line
    ));
    messages.push(format!(
        "Host key for {host} has changed and you have requested strict checking."
    ));
    messages.push(HOST_KEY_VERIFICATION_FAILED.to_owned());
    messages
}

impl SshError {
    /// The log messages an `ssh` child would print for this failure, one per
    /// upstream `error()`/`fatal()` call.
    ///
    /// A failure with no OpenSSH counterpart yields its own text as the
    /// single message.
    pub fn ssh_messages(&self) -> Vec<String> {
        match self {
            Self::BannerTimeout { remote } => banner_timeout_messages(remote),
            Self::HostKeyMismatch { host, changed } => changed_host_key_messages(host, changed),
            Self::UnknownHost { host, key_type } => unknown_host_messages(host, key_type),
            Self::HostKeyRevoked {
                key_type,
                fingerprint,
                file,
            } => revoked_host_key_messages(key_type, fingerprint, file),
            other => vec![other.to_string()],
        }
    }

    /// The bytes an `ssh` child writes to stderr for this failure: each log
    /// message followed by `"\r\n"`.
    ///
    /// upstream: openssh/log.c:409 `do_log()` - `"%s%s%.*s\r\n"`.
    pub fn ssh_stderr(&self) -> String {
        self.ssh_messages()
            .iter()
            .map(|message| format!("{message}\r\n"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each message ends in CRLF, as ssh's `do_log()` writes it.
    #[test]
    fn single_message_failures_end_in_crlf() {
        let refused = SshError::ConnectFailed {
            host: "localhost".to_owned(),
            port: 1,
            reason: "Connection refused".to_owned(),
        };
        assert_eq!(
            refused.ssh_stderr(),
            "ssh: connect to host localhost port 1: Connection refused\r\n"
        );
        let denied = SshError::AuthenticationFailed {
            user: "nobody".to_owned(),
            host: "127.0.0.1".to_owned(),
            methods: "publickey,password".to_owned(),
        };
        assert_eq!(
            denied.ssh_stderr(),
            "nobody@127.0.0.1: Permission denied (publickey,password).\r\n"
        );
        let closed = SshError::ConnectionClosed {
            remote: "127.0.0.1 port 22979".to_owned(),
        };
        assert_eq!(
            closed.ssh_stderr(),
            "Connection closed by 127.0.0.1 port 22979\r\n"
        );
        assert_eq!(
            SshError::HostKeyVerificationFailed.ssh_stderr(),
            "Host key verification failed.\r\n"
        );
    }

    /// A failure upstream reports in two calls prints two CRLF lines.
    #[test]
    fn two_message_failures_print_two_lines() {
        let timeout = SshError::BannerTimeout {
            remote: "127.0.0.1 port 22978".to_owned(),
        };
        assert_eq!(
            timeout.ssh_stderr(),
            "Connection timed out during banner exchange\r\n\
             Connection to 127.0.0.1 port 22978 timed out\r\n"
        );
        let unknown = SshError::UnknownHost {
            host: "[127.0.0.1]:22977".to_owned(),
            key_type: "ED25519",
        };
        assert_eq!(
            unknown.ssh_stderr(),
            "No ED25519 host key is known for [127.0.0.1]:22977 and you have requested \
             strict checking.\r\nHost key verification failed.\r\n"
        );
        let revoked = SshError::HostKeyRevoked {
            key_type: "ED25519",
            fingerprint: "SHA256:abc".to_owned(),
            file: "/tmp/revoked".to_owned(),
        };
        assert_eq!(
            revoked.ssh_stderr(),
            "Host key ED25519 SHA256:abc revoked by file /tmp/revoked\r\n\
             Host key verification failed.\r\n"
        );
    }

    /// The changed-key block: the fingerprint message keeps its embedded
    /// bare newline, and every message ends in CRLF.
    #[test]
    fn changed_host_key_block_matches_upstream_layout() {
        let changed = SshError::HostKeyMismatch {
            host: "[127.0.0.1]:22977".to_owned(),
            changed: Box::new(ChangedHostKey {
                key_type: "ED25519",
                fingerprint: "SHA256:abc".to_owned(),
                add_to: Some("/tmp/kh".to_owned()),
                offending_file: "/tmp/kh".to_owned(),
                offending_line: 1,
            }),
        };
        let banner = "@".repeat(59);
        let expected = format!(
            "{banner}\r\n\
             @    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\r\n\
             {banner}\r\n\
             IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\r\n\
             Someone could be eavesdropping on you right now (man-in-the-middle attack)!\r\n\
             It is also possible that a host key has just been changed.\r\n\
             The fingerprint for the ED25519 key sent by the remote host is\nSHA256:abc.\r\n\
             Please contact your system administrator.\r\n\
             Add correct host key in /tmp/kh to get rid of this message.\r\n\
             Offending ED25519 key in /tmp/kh:1\r\n\
             Host key for [127.0.0.1]:22977 has changed and you have requested strict checking.\r\n\
             Host key verification failed.\r\n"
        );
        assert_eq!(changed.ssh_stderr(), expected);
    }

    /// With no `known_hosts` file configured upstream omits the
    /// `Add correct host key` line (openssh/sshconnect.c:1316).
    #[test]
    fn changed_host_key_without_a_file_omits_the_add_line() {
        let changed = SshError::HostKeyMismatch {
            host: "h".to_owned(),
            changed: Box::new(ChangedHostKey {
                key_type: "RSA",
                fingerprint: "SHA256:abc".to_owned(),
                add_to: None,
                offending_file: "/tmp/kh".to_owned(),
                offending_line: 3,
            }),
        };
        assert!(!changed.ssh_stderr().contains("Add correct host key"));
    }

    /// A failure with no ssh counterpart keeps its own text as one line.
    #[test]
    fn other_failures_print_their_own_text() {
        let error = SshError::KeyLoad("bad key".to_owned());
        assert_eq!(error.ssh_stderr(), "key load error: bad key\r\n");
    }
}
