//! SSH connection spawn and secluded-args plumbing.
//!
//! Builds an [`SshCommand`] from the parsed connection details and transfer
//! config, spawns the remote process (upstream: `main.c:do_cmd()`), and ships
//! secluded args over stdin when active (upstream:
//! `rsync.c:283-320 send_protected_args()`).

use std::ffi::OsString;
use std::time::Duration;

use rsync_io::ssh::{SshCommand, SshConnection};

use super::super::super::config::ClientConfig;
use super::super::super::error::{ClientError, invalid_argument_error};
use super::super::ssh_address_family;

/// Builds and spawns an SSH connection with the remote rsync invocation.
///
/// When `stdin_args` is non-empty (secluded-args mode), the arguments are
/// sent over stdin immediately after spawning the SSH process, before
/// returning the connection for protocol negotiation.
pub(super) fn build_ssh_connection(
    user: &Option<String>,
    host: &str,
    port: Option<u16>,
    invocation_args: &[OsString],
    config: &ClientConfig,
    stdin_args: &[String],
) -> Result<SshConnection, ClientError> {
    let mut ssh = SshCommand::new(host);

    if let Some(user) = user {
        ssh.set_user(user);
    }

    if let Some(port) = port {
        ssh.set_port(port);
    }

    if let Some(shell_args) = config.remote_shell()
        && !shell_args.is_empty()
    {
        ssh.set_program(&shell_args[0]);
        for arg in &shell_args[1..] {
            ssh.push_option(arg.clone());
        }
    }

    // upstream: clientserver.c start_socket_client() binds the local address;
    // forward --address to SSH as -o BindAddress=<addr>.
    if let Some(bind_addr) = config.bind_address() {
        ssh.set_bind_address(Some(bind_addr.socket().ip()));
    }

    // upstream: main.c:587-594 do_cmd() - forward --ipv4/--ipv6 to the ssh
    // child as -4/-6 (only honoured when the remote shell is `ssh`).
    ssh.set_address_family(ssh_address_family(config.address_mode()));

    // upstream: main.c:600-601 do_cmd() - force blocking_io for the rsh/remsh
    // remote shells when the user left --blocking-io/--no-blocking-io unset. The
    // builder applies the auto-enable from the resolved program basename.
    ssh.set_blocking_io(config.blocking_io());

    ssh.set_prefer_aes_gcm(config.prefer_aes_gcm());
    ssh.set_jump_hosts(config.jump_hosts().map(OsString::from));

    // upstream: options.c - contimeout is forwarded as SSH's -o ConnectTimeout.
    let connect_timeout = config.connect_timeout().effective(Duration::from_secs(30));
    ssh.set_connect_timeout(connect_timeout);

    // upstream: options.c:2369 set_io_timeout(io_timeout) applies --timeout
    // uniformly to every transport; on the SSH pipe it drives the stall
    // watchdog. 0/unset leaves it disabled.
    ssh.set_io_timeout(config.ssh_io_timeout());

    ssh.set_remote_command(invocation_args);

    warn_double_compression_once(config.compress(), ssh.has_ssh_compression());

    // upstream: pipe.c:85 - SSH spawn failures return IPC error code.
    let mut connection = ssh.spawn().map_err(|e| {
        invalid_argument_error(
            &format!("failed to spawn SSH connection: {e}"),
            super::super::super::IPC_EXIT_CODE,
        )
    })?;

    // upstream: rsync.c:283-320 send_protected_args() sends args as
    // null-separated strings over the pipe before protocol negotiation begins,
    // applying iconvbufs(ic_send, ...) to each arg when iconv is configured
    // (compat.c:799-806 filesfrom_convert / protect-args iconv gating).
    if !stdin_args.is_empty() {
        let arg_refs: Vec<&str> = stdin_args.iter().map(String::as_str).collect();
        // upstream: rsync.c:296-297 - DEBUG_GTE(CMD, 1) emits
        // `print_child_argv("protected args:", args + i + 1)` right before the
        // per-arg `iconvbufs(ic_send, ...)` loop. `arg_refs` is the same
        // payload we are about to ship over stdin.
        protocol::cmd::trace_protected_args(&arg_refs);
        let iconv_converter = if config.protect_args().unwrap_or(false) {
            config.iconv().resolve_converter()
        } else {
            None
        };
        protocol::secluded_args::send_secluded_args(
            &mut connection,
            &arg_refs,
            iconv_converter.as_ref(),
        )
        .map_err(|e| {
            invalid_argument_error(
                &format!("failed to send secluded args: {e}"),
                super::super::super::IPC_EXIT_CODE,
            )
        })?;
    }

    Ok(connection)
}

/// Returns `true` when both rsync wire compression and SSH stream
/// compression are enabled and a warning should be emitted.
///
/// Extracted so the predicate can be exercised independently of the
/// process-global `OnceLock` that suppresses duplicate warnings.
pub(super) const fn should_warn_double_compression(
    rsync_compress: bool,
    ssh_compress: bool,
) -> bool {
    rsync_compress && ssh_compress
}

/// Emits a one-time warning to stderr when SSH built-in compression and
/// rsync `--compress` are both active.
///
/// Compressing twice wastes CPU and may expand already-compressed data,
/// since SSH cannot meaningfully recompress the rsync stream. Upstream
/// rsync does not detect this case; this is an oc-rsync usability
/// enhancement.
///
/// Detection is conservative: it only inspects the SSH command-line
/// arguments built up from `-e` / `--rsh` and friends, and does not parse
/// `~/.ssh/config`, since that file is merged at spawn time and we cannot
/// reliably read it. SSC-3 (#2563) tracks future `ssh_config` parsing.
///
/// The warning is suppressed after the first emission via a process-wide
/// `OnceLock<bool>`, so callers may invoke this once per SSH spawn without
/// flooding stderr.
pub(super) fn warn_double_compression_once(rsync_compress: bool, ssh_compress: bool) {
    static EMITTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !should_warn_double_compression(rsync_compress, ssh_compress) {
        return;
    }
    EMITTED.get_or_init(|| {
        eprintln!("warning: both rsync wire compression (--compress) and SSH stream compression");
        eprintln!("         (-C in your ssh command) are enabled. Compressed data will be");
        eprintln!("         re-compressed by SSH, wasting CPU. Recommend dropping one of them.");
        true
    });
}
