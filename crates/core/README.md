# rsync-core

`rsync-core` centralises workspace-wide facilities that are reused by the
`oc-rsync` client (including daemon mode via `oc-rsync --daemon`) together with supporting transport crates. The
crate focuses on user-visible message formatting, source-location remapping, and
shared configuration types so diagnostics mirror upstream rsync while pointing
at the Rust implementation.

## Design

- [`message`](src/message.rs) exposes [`Message`](src/message/struct.Message.html)
  alongside helpers such as [`message_source!`](src/message/macro.message_source.html)
  for capturing repo-relative source locations. Higher layers construct
  diagnostics through this API so trailer roles and version suffixes remain
  consistent.
- [`branding`](src/branding.rs) centralises branded program names alongside the
  default configuration and secrets paths installed by the packaging tooling.
- [`version`](src/version/mod.rs) holds the canonical `3.4.1-rust` identifier and the
  compiled feature set consumed by the CLI when rendering `--version` output.
- [`client`](src/client.rs) defines configuration builders and entry points used
  by the CLI wrapper. Today the facade orchestrates the local copy pipeline and
  falls back to the system `rsync` binary for remote transfers while the native
  delta engine is completed.
- [`fallback`](src/fallback.rs) interprets delegation overrides shared by the
  CLI and daemon binaries.
- [`bandwidth`](src/bandwidth.rs) re-exports the [`rsync-bandwidth`](../bandwidth/README.md)
  crate so callers share the same parsing and pacing primitives.

## Invariants

- [`client::run_client`](src/client.rs) reports the delta-transfer engine gap
  using the same wording that the CLI emits when delegation occurs.
- Message trailers always include the `3.4.1-rust` version string and reference
  the caller's role (sender, receiver, generator, server, or daemon).
- Source locations are normalised to repo-relative POSIX-style paths, keeping
  diagnostics stable across platforms.
- Formatting a [`Message`](src/message/struct.Message.html) touches only the
  stored payload and metadata so diagnostics avoid unnecessary allocations.

## Error Handling

The crate does not define new error types. Instead, it supplies builders that
attach upstream error codes to [`Message`](src/message/struct.Message.html)
values via [`Message::error`](src/message/struct.Message.html#method.error). The
resulting diagnostics can be rendered directly or wrapped in higher level error
structures such as [`client::ClientError`](src/client/struct.ClientError.html).

## Example

Create an error message using the helper APIs and render it into the canonical
user-facing form:

```rust
use rsync_core::{message::Message, message::Role, message_source};

let rendered = Message::error(23, "delta-transfer failure")
    .with_role(Role::Sender)
    .with_source(message_source!())
    .to_string();

assert!(rendered.contains("rsync error: delta-transfer failure (code 23)"));
assert!(rendered.contains("[sender=3.4.1-rust]"));
```

## See also

- [`rsync_core::message::strings`](src/message/strings.rs) exposes upstream-aligned
  exit-code wording so higher layers render identical diagnostics.
- [`client::ClientConfig`](src/client/struct.ClientConfig.html) mirrors the
  structure populated by the CLI before invoking the transfer engine.
- [`client::ModuleListRequest`](src/client/struct.ModuleListRequest.html) parses
  daemon operands and retrieves module listings via the legacy `@RSYNCD:`
  handshake.
- [`fallback::fallback_override`](src/fallback/fn.fallback_override.html)
  interprets shared delegation overrides for CLI and daemon wrappers.
- [`rsync_exit_code!`](src/message/macro.rsync_exit_code.html) constructs
  canonical exit-code diagnostics while capturing the caller's source location.
