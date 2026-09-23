//! `ProxyCommand` / `ProxyJump` dialing for the embedded transport.
//!
//! The embedded transport reaches the SSH server one of two ways, captured
//! by [`ProxyDial`]:
//!
//! * [`ProxyDial::Direct`] - the default: a direct TCP dial, exactly what the
//!   transport did before proxy support existed.
//! * [`ProxyDial::Command`] - a command is run through a shell and its stdio
//!   becomes the transport stream in place of the socket, mirroring upstream's
//!   `ssh_proxy_connect` (openssh/sshconnect.c:222-286).
//!
//! `ProxyJump` is not a third path: upstream lowers it into an equivalent
//! `ProxyCommand` (openssh/ssh.c:1310-1360), and so does
//! [`lower_jump_to_command`], keeping a single dial abstraction rather than a
//! forked connect path.
//!
//! `ProxyUseFdpass` has no representation here. Upstream's fd-passing variant
//! (openssh/sshconnect.c:151-210 `ssh_proxy_fdpass_connect`) expects the
//! command to hand back a connected descriptor over `SCM_RIGHTS`; russh dials
//! over an `AsyncRead + AsyncWrite` byte stream and cannot receive a passed
//! descriptor, so [`ProxyDial::from_config`] refuses the request loudly rather
//! than running it as an ordinary stdio proxy.

use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use super::config::SshConfig;
use super::error::SshError;

/// How the embedded transport reaches the SSH server's socket.
pub(super) enum ProxyDial {
    /// A direct TCP dial - no `ProxyCommand`/`ProxyJump` in effect.
    Direct,
    /// Run this (still un-expanded) command template and use its stdio as the
    /// transport stream. `%h`/`%p`/`%r` are expanded by
    /// [`expand_proxy_tokens`] at the dial site, where the target host, port
    /// and user are final.
    Command(String),
}

impl ProxyDial {
    /// Selects the dial strategy for `cfg`, mirroring upstream's precedence.
    ///
    /// `ProxyCommand` wins over `ProxyJump` (openssh/ssh.c:1305-1310 only
    /// synthesises the jump command when no `ProxyCommand` is set), and the
    /// literal `none` in either means a direct connection
    /// (openssh/ssh.c:1298 `option_clear_or_none`). A `ProxyUseFdpass yes`
    /// paired with an active proxy is refused here rather than silently
    /// downgraded to stdio.
    pub(super) fn from_config(cfg: &SshConfig) -> Result<ProxyDial, SshError> {
        let template = if let Some(cmd) = cfg.proxy_command.as_deref() {
            (!is_none_token(cmd)).then(|| cmd.to_owned())
        } else if let Some(jump) = cfg.jump_hosts.as_deref() {
            if is_none_token(jump) {
                None
            } else {
                Some(lower_jump_to_command(jump)?)
            }
        } else {
            None
        };

        match template {
            // `ProxyUseFdpass` is inert without a proxy command, exactly as
            // upstream leaves it unused on a direct connection.
            None => Ok(ProxyDial::Direct),
            Some(template) => {
                if cfg.proxy_use_fdpass {
                    return Err(SshError::ProxyUseFdpassUnsupported);
                }
                Ok(ProxyDial::Command(template))
            }
        }
    }
}

/// Whether a value is the literal `none` (case-insensitive), upstream's
/// sentinel for "no proxy" (openssh/ssh.c:1298 `option_clear_or_none`).
fn is_none_token(s: &str) -> bool {
    s.eq_ignore_ascii_case("none")
}

/// Lowers a `ProxyJump` chain into the equivalent `ProxyCommand` template.
///
/// Faithful to upstream's synthesis (openssh/ssh.c:1310-1360): the LAST hop
/// is the proxy directly adjacent to the target and becomes the `ssh`
/// operand, its `[user@]` and `[:port]` becoming `-l`/`-p`; every earlier hop
/// is handed to that `ssh` as a recursive `-J`. The target is reached through
/// it with `-W '[%h]:%p'`, whose `%h`/`%p` are expanded against the final
/// destination by [`expand_proxy_tokens`].
///
/// # Errors
///
/// [`SshError::ProxyCommand`] when the chain is empty.
pub(super) fn lower_jump_to_command(jump: &str) -> Result<String, SshError> {
    let hops: Vec<&str> = jump
        .split(',')
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .collect();
    let Some((last, extra)) = hops.split_last() else {
        return Err(SshError::ProxyCommand {
            reason: "empty ProxyJump specification".to_owned(),
        });
    };

    let hop = parse_jump_hop(last);
    let mut cmd = String::from("ssh");
    if let Some(user) = hop.user {
        cmd.push_str(" -l ");
        cmd.push_str(&user);
    }
    if let Some(port) = hop.port {
        cmd.push_str(" -p ");
        cmd.push_str(&port);
    }
    if !extra.is_empty() {
        cmd.push_str(" -J ");
        cmd.push_str(&extra.join(","));
    }
    // upstream quotes the forward address the same way (openssh/ssh.c:1349
    // `-W '[%h]:%p'`); the quotes are stripped by the shell after the
    // percent expansion produces the concrete `[host]:port`.
    cmd.push_str(" -W '[%h]:%p' ");
    cmd.push_str(&hop.host);
    Ok(cmd)
}

/// One parsed `ProxyJump` hop.
pub(super) struct JumpHop {
    user: Option<String>,
    pub(super) host: String,
    port: Option<String>,
}

/// Parses a single `[ssh://][user@]host[:port]` hop.
///
/// Mirrors upstream's `parse_ssh_uri`/`hpdelim` handling (openssh/misc.c):
/// an optional `ssh://` scheme, a `user@` prefix, and a bracketed `[v6]:port`
/// or bare `host:port`. A bare address literal carrying multiple colons has
/// no port.
pub(super) fn parse_jump_hop(spec: &str) -> JumpHop {
    let spec = spec.strip_prefix("ssh://").unwrap_or(spec);
    let (user, hostport) = match spec.split_once('@') {
        Some((u, rest)) => (Some(u.to_owned()), rest),
        None => (None, spec),
    };
    let (host, port) = split_host_port(hostport);
    JumpHop { user, host, port }
}

/// Splits `host[:port]`, honouring `[v6]:port` brackets and treating a bare
/// multi-colon literal as a portless IPv6 address.
fn split_host_port(hostport: &str) -> (String, Option<String>) {
    if let Some(rest) = hostport.strip_prefix('[') {
        if let Some((host, after)) = rest.split_once(']') {
            let port = after
                .strip_prefix(':')
                .filter(|p| !p.is_empty())
                .map(str::to_owned);
            return (host.to_owned(), port);
        }
        return (hostport.to_owned(), None);
    }
    match hostport.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && !port.is_empty() => {
            (host.to_owned(), Some(port.to_owned()))
        }
        _ => (hostport.to_owned(), None),
    }
}

/// Expands the connection percent tokens a `ProxyCommand` may carry.
///
/// Supports exactly the proxy-command set upstream expands at dial time
/// (openssh/sshconnect.c:89-107 `expand_proxy_command`): `%h` (target
/// host), `%k` (`HostKeyAlias`, falling back to the typed alias), `%n`
/// (the typed alias), `%p` (target port), `%r` (remote user) and the
/// literal `%%`. This set is DELIBERATELY narrower than the default client
/// tokens, and env `${}`/tilde do not apply here - any other `%x` is
/// refused rather than passed to the shell verbatim.
///
/// # Errors
///
/// [`SshError::ProxyTokenUnsupported`] for an unhandled `%x`;
/// [`SshError::ProxyCommand`] for a trailing `%`.
pub(super) fn expand_proxy_tokens(
    template: &str,
    host: &str,
    host_arg: &str,
    host_key_alias: Option<&str>,
    port: u16,
    user: Option<&str>,
) -> Result<String, SshError> {
    // upstream: openssh/sshconnect.c:94-95 - the alias fallback for `%k`
    // is the TYPED host, not the resolved one.
    let keyalias = host_key_alias.unwrap_or(host_arg);
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('h') => out.push_str(host),
            Some('k') => out.push_str(keyalias),
            Some('n') => out.push_str(host_arg),
            Some('p') => out.push_str(&port.to_string()),
            Some('r') => out.push_str(&effective_user(user)),
            Some('%') => out.push('%'),
            Some(other) => return Err(SshError::ProxyTokenUnsupported { token: other }),
            None => {
                return Err(SshError::ProxyCommand {
                    reason: "trailing % in proxy command".to_owned(),
                });
            }
        }
    }
    Ok(out)
}

/// The user `%r` expands to: the configured remote user, or the local login
/// name when none was set.
///
/// upstream fills `options.user` from the local passwd entry when it is unset
/// (openssh/ssh.c `fill_default_options` -> `getpwuid`). oc reads the same
/// login-name environment the shell exports as a portable stand-in, shared
/// with the config-side expander.
fn effective_user(user: Option<&str>) -> String {
    user.map_or_else(super::token_expand::local_user_name, str::to_owned)
}

/// Spawns `command` through the platform shell and captures its stdio as an
/// async duplex stream for russh's `connect_stream`.
///
/// upstream runs the proxy command via a shell (openssh/sshconnect.c:260-278
/// `execv(shell, ...)` with `sh -c`); oc uses `/bin/sh -c` on Unix and
/// `cmd /C` on Windows for a predictable shell independent of the caller's
/// interactive one. The child's stderr is inherited so its own diagnostics
/// reach the operator.
///
/// # Errors
///
/// [`SshError::ProxyCommand`] when the child cannot be spawned or its stdin /
/// stdout cannot be captured.
pub(super) fn spawn_proxy_command(command: &str) -> Result<ChildStdio, SshError> {
    let mut cmd = shell_command(command);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| SshError::ProxyCommand {
        reason: format!("spawn `{command}`: {e}"),
    })?;
    let stdin = child.stdin.take().ok_or_else(|| SshError::ProxyCommand {
        reason: "proxy command stdin was not captured".to_owned(),
    })?;
    let stdout = child.stdout.take().ok_or_else(|| SshError::ProxyCommand {
        reason: "proxy command stdout was not captured".to_owned(),
    })?;
    Ok(ChildStdio {
        child,
        stdin,
        stdout,
    })
}

/// Builds the shell invocation for the current platform.
#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg(command);
    c
}

/// Builds the shell invocation for the current platform.
#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut c = Command::new("cmd");
    c.arg("/C").arg(command);
    c
}

/// A spawned proxy command's stdio, presented as one bidirectional stream:
/// reads come from the child's stdout, writes go to its stdin. Suitable for
/// russh's `connect_stream`, which drives the SSH protocol over any
/// `AsyncRead + AsyncWrite` transport.
pub(super) struct ChildStdio {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl AsyncRead for ChildStdio {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for ChildStdio {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stdin).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}

impl Drop for ChildStdio {
    fn drop(&mut self) {
        // The proxy helper is only useful while the SSH session lives; reap
        // it when the transport stream is dropped rather than leak the child.
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_when_no_proxy_configured() {
        let cfg = SshConfig::default();
        assert!(matches!(
            ProxyDial::from_config(&cfg).unwrap(),
            ProxyDial::Direct
        ));
    }

    #[test]
    fn proxy_command_is_carried_verbatim() {
        let cfg = SshConfig {
            proxy_command: Some("ssh -W %h:%p bastion".to_owned()),
            ..SshConfig::default()
        };
        match ProxyDial::from_config(&cfg).unwrap() {
            ProxyDial::Command(c) => assert_eq!(c, "ssh -W %h:%p bastion"),
            ProxyDial::Direct => panic!("expected a proxy command"),
        }
    }

    #[test]
    fn proxy_command_none_is_a_direct_dial() {
        let cfg = SshConfig {
            proxy_command: Some("None".to_owned()),
            ..SshConfig::default()
        };
        assert!(matches!(
            ProxyDial::from_config(&cfg).unwrap(),
            ProxyDial::Direct
        ));
    }

    #[test]
    fn jump_lowers_to_the_proxy_command_form() {
        let cfg = SshConfig {
            jump_hosts: Some("bastion.example.com".to_owned()),
            ..SshConfig::default()
        };
        match ProxyDial::from_config(&cfg).unwrap() {
            ProxyDial::Command(c) => assert_eq!(c, "ssh -W '[%h]:%p' bastion.example.com"),
            ProxyDial::Direct => panic!("expected a lowered jump command"),
        }
    }

    #[test]
    fn single_jump_with_user_and_port() {
        assert_eq!(
            lower_jump_to_command("alice@bastion:2222").unwrap(),
            "ssh -l alice -p 2222 -W '[%h]:%p' bastion"
        );
    }

    #[test]
    fn multi_hop_jump_recurses_via_dash_j() {
        // Last hop `c` is the operand; the earlier hops `a,b` become the
        // recursive `-J`, matching openssh/ssh.c:1310-1360.
        assert_eq!(
            lower_jump_to_command("a,b,c").unwrap(),
            "ssh -J a,b -W '[%h]:%p' c"
        );
    }

    #[test]
    fn multi_hop_last_hop_user_port_and_recursion() {
        assert_eq!(
            lower_jump_to_command("a,bob@c:2200").unwrap(),
            "ssh -l bob -p 2200 -J a -W '[%h]:%p' c"
        );
    }

    /// Non-vacuity control for the lowering: a multi-hop chain must not
    /// collapse to the single-hop form, i.e. the earlier hops are not lost.
    #[test]
    fn multi_hop_differs_from_single_hop() {
        let single = lower_jump_to_command("c").unwrap();
        let multi = lower_jump_to_command("a,b,c").unwrap();
        assert_ne!(single, multi);
        assert!(multi.contains(" -J a,b "));
        assert!(!single.contains(" -J "));
    }

    #[test]
    fn ipv6_bracketed_jump_hop() {
        assert_eq!(
            lower_jump_to_command("user@[2001:db8::1]:2022").unwrap(),
            "ssh -l user -p 2022 -W '[%h]:%p' 2001:db8::1"
        );
    }

    #[test]
    fn bare_ipv6_jump_hop_has_no_port() {
        assert_eq!(
            lower_jump_to_command("2001:db8::1").unwrap(),
            "ssh -W '[%h]:%p' 2001:db8::1"
        );
    }

    #[test]
    fn empty_jump_is_refused() {
        assert!(matches!(
            lower_jump_to_command("  ,  "),
            Err(SshError::ProxyCommand { .. })
        ));
    }

    #[test]
    fn expands_connection_tokens() {
        assert_eq!(
            expand_proxy_tokens(
                "connect %h %p %r",
                "host.example",
                "alias",
                None,
                2022,
                Some("deploy")
            )
            .unwrap(),
            "connect host.example 2022 deploy"
        );
    }

    /// `%n` is the TYPED alias and `%k` the `HostKeyAlias`-or-alias, the two
    /// tokens upstream's proxy set carries beyond `%h %p %r`
    /// (openssh/sshconnect.c:94-103).
    #[test]
    fn expands_alias_and_keyalias_tokens() {
        assert_eq!(
            expand_proxy_tokens("nc %n %k", "resolved", "typed", None, 22, None).unwrap(),
            "nc typed typed"
        );
        assert_eq!(
            expand_proxy_tokens("nc %n %k", "resolved", "typed", Some("kalias"), 22, None).unwrap(),
            "nc typed kalias"
        );
    }

    #[test]
    fn expands_double_percent_to_literal() {
        assert_eq!(
            expand_proxy_tokens("100%% %h", "h", "h", None, 22, None).unwrap(),
            "100% h"
        );
    }

    /// The proxy set stays NARROW: a default-client token like `%C` is
    /// refused here even though the config-side expander knows it, because
    /// upstream's `expand_proxy_command` never receives it.
    #[test]
    fn unknown_token_is_refused() {
        match expand_proxy_tokens("nc %C", "h", "h", None, 22, None) {
            Err(SshError::ProxyTokenUnsupported { token }) => assert_eq!(token, 'C'),
            other => panic!("expected ProxyTokenUnsupported, got {other:?}"),
        }
    }

    #[test]
    fn trailing_percent_is_refused() {
        assert!(matches!(
            expand_proxy_tokens("oops %", "h", "h", None, 22, None),
            Err(SshError::ProxyCommand { .. })
        ));
    }

    /// Non-vacuity control for token expansion: without a token the string is
    /// returned unchanged, so the expander is not rewriting arbitrary text.
    #[test]
    fn no_tokens_passes_through_unchanged() {
        assert_eq!(
            expand_proxy_tokens("nc host 22", "other", "other", None, 99, Some("root")).unwrap(),
            "nc host 22"
        );
    }

    #[test]
    fn fdpass_with_proxy_command_is_refused_loudly() {
        let cfg = SshConfig {
            proxy_command: Some("nc %h %p".to_owned()),
            proxy_use_fdpass: true,
            ..SshConfig::default()
        };
        assert!(matches!(
            ProxyDial::from_config(&cfg),
            Err(SshError::ProxyUseFdpassUnsupported)
        ));
    }

    /// Non-vacuity control for the fd-pass gap: fd-passing without any proxy
    /// command is inert (a direct dial), so the refusal is tied to an active
    /// proxy rather than to the flag alone.
    #[test]
    fn fdpass_without_proxy_command_is_inert() {
        let cfg = SshConfig {
            proxy_use_fdpass: true,
            ..SshConfig::default()
        };
        assert!(matches!(
            ProxyDial::from_config(&cfg).unwrap(),
            ProxyDial::Direct
        ));
    }

    /// Behavioural proof that the transport rides the spawned command's
    /// stdio: `cat` echoes stdin to stdout, so bytes written to the stream
    /// (the child's stdin) come back on a read (the child's stdout). If the
    /// wiring instead used a socket, nothing would echo.
    #[cfg(unix)]
    #[tokio::test]
    async fn spawned_command_stdio_round_trips() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = spawn_proxy_command("cat").unwrap();
        stream.write_all(b"proxy-roundtrip").await.unwrap();
        stream.flush().await.unwrap();
        let mut buf = [0u8; 15];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"proxy-roundtrip");
    }

    /// Non-vacuity control for the round-trip: a command that produces no
    /// output reads as EOF, so the echo test above is not passing on some
    /// always-ready stream.
    #[cfg(unix)]
    #[tokio::test]
    async fn spawned_command_without_output_reads_eof() {
        use tokio::io::AsyncReadExt;

        let mut stream = spawn_proxy_command("true").unwrap();
        let mut buf = [0u8; 8];
        let n = stream.read(&mut buf).await.unwrap();
        assert_eq!(n, 0, "a command producing no output must read as EOF");
    }
}
