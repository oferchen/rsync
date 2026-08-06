//! Remote transfer orchestration for SSH and daemon transports.
//!
//! This module provides the infrastructure for executing rsync transfers over
//! remote connections, including SSH and rsync daemon protocols. It mirrors
//! the dispatch paths in upstream `main.c:do_cmd()` (SSH) and
//! `clientserver.c:start_daemon_client()` (daemon).
//!
//! # Submodules
//!
//! - `daemon_transfer` - Daemon protocol (rsync://) connection, handshake, and transfer
//! - `ssh_transfer` - SSH-based remote transfers via `--rsh`/`-e`
//! - `invocation` - Remote rsync `--server` argument construction and role detection
//! - `flags` - Shared flag builder functions for compact server option strings
//! - `remote_to_remote` - Two-host proxy relay via local machine
//!
//! # Upstream Reference
//!
//! - `main.c:do_cmd()` - SSH command spawning
//! - `main.c:start_server()` - Server-side entry after SSH
//! - `clientserver.c:start_daemon_client()` - Daemon URL dispatch
//! - `options.c:server_options()` - Server flag string generation

#[cfg(feature = "async-ssh")]
pub mod async_ssh_transport;
pub(crate) mod batch_support;
/// Daemon transfer orchestration for `rsync://` URLs.
pub mod daemon_transfer;
/// Embedded SSH transfer orchestration using the russh library.
#[cfg(feature = "embedded-ssh")]
pub mod embedded_ssh_transfer;
pub(crate) mod files_from_forwarding;
pub(crate) mod flags;
/// Implied-include source-arg computation for pull-side flist validation.
pub(crate) mod implied_source;
/// Remote rsync `--server` invocation argument builder.
pub mod invocation;
/// Shared client-visible itemize sink for the remote transports.
pub(crate) mod itemize_sink;
/// `--info=`/`--debug=` server-argument construction (make_output_option).
pub(crate) mod output_option;
/// Remote-to-remote transfer via local proxy relay.
pub mod remote_to_remote;
/// SSH transfer orchestration for `ssh://` and `host:path` targets.
pub mod ssh_transfer;
#[cfg(feature = "async-ssh")]
pub use async_ssh_transport::{
    ENV_OPT_IN as ASYNC_SSH_ENV_OPT_IN, is_enabled_by_env as async_ssh_enabled,
    run_async_ssh_transfer,
};
pub use daemon_transfer::{run_daemon_over_remote_shell, run_daemon_transfer};
#[cfg(feature = "embedded-ssh")]
pub use embedded_ssh_transfer::run_embedded_ssh_transfer;
pub use invocation::{
    RemoteInvocationBuilder, RemoteOperands, RemoteRole, SecludedInvocation, TransferSpec,
    determine_transfer_role, operand_is_remote,
};
pub use ssh_transfer::run_ssh_transfer;

use rsync_io::ssh::SshAddressFamily;

use super::config::AddressMode;

/// Checks whether an operand is an `ssh://` URL.
///
/// Returns `true` for operands beginning with `ssh://`, the scheme that selects
/// the built-in (russh) SSH transport rather than the `host:path` subprocess
/// ssh path. Compiled into every build - not gated on `embedded-ssh` - so the
/// dispatcher can recognise the scheme even when that feature is absent and
/// reject it with a clear diagnostic instead of misparsing it as a `host:path`
/// spec with host `ssh`.
///
/// oc-specific: upstream rsync has no `ssh://` operand scheme.
pub(crate) fn is_ssh_url(operand: &str) -> bool {
    operand.starts_with("ssh://")
}

/// Checks whether an operand is a `quic://` URL.
///
/// Returns `true` for operands beginning with `quic://` (case-insensitive
/// scheme, matching the daemon-URL dispatch), the scheme that carries the
/// daemon protocol over QUIC. Compiled into every build - not gated on the
/// `quic` feature - so the dispatcher can recognise the scheme even when that
/// feature is absent and reject it with a clear diagnostic instead of
/// misparsing it as a `host:path` spec with host `quic`.
///
/// oc-specific: upstream rsync has no `quic://` operand scheme.
pub(crate) fn is_quic_url(operand: &str) -> bool {
    operand.starts_with("quic://") || operand.starts_with("QUIC://")
}

/// Maps the negotiated [`AddressMode`] onto the SSH `-4`/`-6` hint shared by
/// every `do_cmd()`-equivalent SSH spawn (single-host and remote-to-remote).
///
/// [`AddressMode::Default`] yields `None`, leaving the ssh child free to pick
/// whichever family resolves first; the forced modes map to the matching flag.
///
/// upstream: main.c:587-594 `do_cmd()` gates the `-4`/`-6` append on
/// `default_af_hint` being set (and the remote-shell basename being `ssh`,
/// which the builder enforces).
pub(in crate::client::remote) const fn ssh_address_family(
    mode: AddressMode,
) -> Option<SshAddressFamily> {
    match mode {
        AddressMode::Default => None,
        AddressMode::Ipv4 => Some(SshAddressFamily::V4),
        AddressMode::Ipv6 => Some(SshAddressFamily::V6),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_quic_url, is_ssh_url};

    /// The `ssh://` detector is the single source of truth for the scheme that
    /// selects the embedded transport, so it must recognise the scheme (with or
    /// without user/port) and reject every other operand shape - `host:path`,
    /// daemon modules, `rsync://`, `quic://`, and plain local paths.
    #[test]
    fn is_ssh_url_matches_only_ssh_scheme() {
        assert!(is_ssh_url("ssh://host/path"));
        assert!(is_ssh_url("ssh://user@host/path"));
        assert!(is_ssh_url("ssh://user:pass@host:2222/path"));

        assert!(!is_ssh_url("host:path"));
        assert!(!is_ssh_url("host::module"));
        assert!(!is_ssh_url("rsync://host/module"));
        assert!(!is_ssh_url("quic://host/module"));
        assert!(!is_ssh_url("/local/path"));
    }

    /// The `quic://` detector is the single source of truth for the QUIC daemon
    /// scheme (compiled into every build), so it must recognise `quic://` in
    /// either case and reject every other operand shape - `host:path`, daemon
    /// modules, `rsync://`, `ssh://`, and plain local paths.
    #[test]
    fn is_quic_url_matches_only_quic_scheme() {
        assert!(is_quic_url("quic://host/module"));
        assert!(is_quic_url("quic://user@host:8730/module"));
        assert!(is_quic_url("QUIC://host/module"));

        assert!(!is_quic_url("host:path"));
        assert!(!is_quic_url("host::module"));
        assert!(!is_quic_url("rsync://host/module"));
        assert!(!is_quic_url("ssh://host/path"));
        assert!(!is_quic_url("/local/path"));
    }
}
