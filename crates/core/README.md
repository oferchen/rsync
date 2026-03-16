# core

The `core` crate is the orchestration facade for all rsync transfers. Both the
CLI binary and daemon mode route every transfer through this crate. It
aggregates the workspace-wide shared infrastructure - configuration, error
codes, authentication, I/O helpers, and the client/server entry points - into
a single re-export surface so upper layers have a single dependency.

## Crate Position in the Dependency Graph

```
cli ──► core ──► engine, protocol, transport, flist, rsync_io
                 core ──► checksums, filters, compress, bandwidth, metadata
```

`core` sits directly below `cli` and above the implementation crates. It does
not contain protocol or engine logic itself - it imports those crates and
exposes a unified API.

## Key Types and Their Roles

- [`client::ClientConfig`] - transfer configuration built by the CLI before
  invoking the engine. Constructed via `ClientConfig::builder()`.
- [`client::run_client`] - main entry point for all CLI-initiated transfers
  (local copy, SSH, and daemon). Returns [`client::ClientSummary`] on success
  or [`client::ClientError`] on failure.
- [`client::ClientError`] - carries the upstream-compatible exit code and the
  fully formatted [`message::Message`] diagnostic. Never uses `unwrap` or
  `expect` on fallible paths.
- [`server`] (re-export of the `transfer` crate) - server-side orchestration
  consumed by the daemon and by the server half of SSH transfers.
- [`exit_code`] - centralized exit code definitions that match upstream
  rsync's `errcode.h` exactly.
- [`message`] - message formatting utilities that produce upstream-compatible
  diagnostics with role trailers (`[sender=...]`, `[daemon=...]`, etc.).
- [`auth`] - shared daemon authentication helpers (challenge/response, secrets
  file parsing).
- [`timeout`] - connection and I/O timeout configuration shared by all
  transport modes.
- [`signal`] - Unix signal handling for graceful shutdown (SIGINT, SIGTERM,
  SIGHUP, SIGPIPE).
- [`remote_shell`] - SSH argument construction and remote shell command
  parsing.
- [`bandwidth`] - re-export of the `bandwidth` crate for rate-limit parsing
  and pacing.
- [`version`] - oc-rsync release version and compiled feature set, used by
  the CLI `--version` output.
- [`flist`] (re-export) - file list generation and transmission, mirroring
  upstream `flist.c`.
- [`io`] (re-export of `rsync_io`) - multiplexed socket and pipe I/O,
  mirroring upstream `io.c`.

## Invariants

- No `unwrap` or `expect` on any fallible path. All errors are propagated via
  `Result` and routed through [`client::ClientError`].
- Exit codes always match the upstream rsync `errcode.h` values.
- Message trailers include the oc-rsync version string and the caller's role.
- Source locations in diagnostics are normalised to repo-relative POSIX paths,
  keeping error output stable across platforms.

## Error Handling

Errors are typed via `thiserror` and carry upstream-compatible exit codes.
[`client::ClientError`] is the top-level error type for transfer failures. The
[`exit_code`] module maps every failure mode to its upstream exit code value.
Path context is attached to I/O errors via an extension trait so diagnostics
always include the affected file path.
