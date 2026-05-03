# Audit: `--iconv` charset transcoding pipeline completeness

Tracking: oc-rsync task #1840.

This audit maps every place where upstream rsync 3.4.1 calls `iconvbufs(ic_send, ...)`
or `iconvbufs(ic_recv, ...)` to its counterpart in the oc-rsync codebase, then
reports gaps. The goal is to determine whether oc-rsync transcodes the same
byte streams in the same direction as upstream when the user passes `--iconv`.

References:

- `target/interop/upstream-src/rsync-3.4.1/io.c`
- `target/interop/upstream-src/rsync-3.4.1/flist.c`
- `target/interop/upstream-src/rsync-3.4.1/rsync.c`
- `target/interop/upstream-src/rsync-3.4.1/compat.c`
- `target/interop/upstream-src/rsync-3.4.1/log.c`

The wire-protocol contract for `--iconv` is:

- `ic_send` converts local-charset bytes -> remote-charset bytes (used on
  paths and arg lists going out on the wire).
- `ic_recv` converts remote-charset bytes -> local-charset bytes (used on
  paths and arg lists coming in from the wire).
- The receiver applies `ic_recv` to symlink targets only when
  `sender_symlink_iconv` is true, which itself depends on the negotiated
  `CF_SYMLINK_ICONV` capability flag (`compat.c:763-767`).

## Coverage table

| Upstream symbol (file:line) | Direction | oc-rsync symbol | Status |
|---|---|---|---|
| `flist.c:738-754` `recv_file_entry()` filename `iconvbufs(ic_recv, ...)` | remote -> local | `crates/protocol/src/flist/read/name.rs::apply_encoding_conversion` | Implemented but never wired (see Finding 4). |
| `flist.c:1127-1150` `recv_file_entry()` symlink target `iconvbufs(ic_recv, ...)` gated on `sender_symlink_iconv` | remote -> local | `crates/protocol/src/flist/read/extras.rs::read_symlink_target` | Implemented (Finding 1). Applies `self.iconv.as_ref()` `remote_to_local` per target; absent converter passes bytes through. Gating tracked under Finding 4 wiring. |
| `flist.c:1579-1603` `send_file_entry()` filename `iconvbufs(ic_send, ...)` (dirname + '/' + basename) | local -> remote | `crates/protocol/src/flist/write/encoding.rs::apply_encoding_conversion` (called from `write_entry`) | Implemented but never wired (see Finding 4). |
| `flist.c:1605-1621` `send_file_entry()` symlink target `iconvbufs(ic_send, ...)` gated on `symlink_len && sender_symlink_iconv` | local -> remote | `crates/protocol/src/flist/write/encoding.rs::write_symlink_target` | Implemented (Finding 2). Calls `self.apply_encoding_conversion` on the wire-form target before emitting; absent converter passes bytes through. Gating tracked under Finding 4 wiring. |
| `io.c:416-452` `forward_filesfrom_data()` per-string `iconvbufs(ic_send, ...)` for `--files-from` over the wire (`filesfrom_convert`) | local -> remote | `crates/protocol/src/files_from.rs::forward_files_from` and `read_files_from_stream` | Implemented (Finding 3a). Optional `iconv: Option<&FilenameConverter>` parameter applies `local_to_remote` / `remote_to_local` per entry with ICB_INCLUDE_BAD fallback. Daemon-client phase-2 derives the converter gated on `protect_args` (`compat.c:799-806` `filesfrom_convert`). |
| `io.c:983-1031` `send_msg()` text-message `iconvbufs(ic_send, ...)` for `MSG_*` UTF-8 conversion | local -> remote | `crates/protocol` multiplex sender (`envelope/`, `multiplex/`) | Missing. Text messages are written verbatim. Acceptable when both peers use UTF-8 only. |
| `io.c:1240-1289` `read_line()` with `RL_CONVERT` -> `iconvbufs(ic_recv, ...)` for daemon-side arg reading | remote -> local | `crates/protocol/src/secluded_args.rs::recv_secluded_args` | Implemented (Finding 3b). Optional `iconv: Option<&FilenameConverter>` parameter applies `remote_to_local` per arg with ICB_INCLUDE_BAD fallback. Production callers pass `None` because `ic_recv` is unset at `read_args` time (mirrors upstream where `setup_iconv()` runs in `setup_protocol()` after args exchange). |
| `io.c:1559-1591` `MSG_DELETED` payload `iconvbufs(ic_send, ...)` (note: this is `ic_send` because the path was native on the receiver and is being forwarded to the generator/sender peer for logging) | local -> remote | oc-rsync does not currently transmit `MSG_DELETED` from the receiver back to the generator/sender. | Out of scope until MSG_DELETED forwarding lands. Tracked separately. |
| `rsync.c:283-320` `send_protected_args()` per-arg `iconvbufs(ic_send, ...)` | local -> remote | `crates/protocol/src/secluded_args.rs::send_secluded_args` | Implemented (Finding 3b). Optional `iconv: Option<&FilenameConverter>` parameter applies `local_to_remote` per arg with ICB_INCLUDE_BAD fallback. SSH and daemon client send paths derive `iconv_converter` from `config.iconv().resolve_converter()` gated on `protect_args` (`compat.c:799-806`). |
| `log.c:251-371` `rwrite()` log-line transcoding via `ic_recv` (when `is_utf8`) or `ic_chck` (locale) | mixed | `crates/logging/src/...` | Missing (`ic_chck` not modeled at all). Acceptable in an all-UTF-8 environment but should be tracked. |
| `compat.c:716-718, 763-767` `CF_SYMLINK_ICONV` advertised only when `iconv_opt` is set; `sender_symlink_iconv` derived from peer side | negotiation | `crates/transfer/src/setup/capability.rs::build_capability_string` and `build_compat_flags_from_client_info` filter the `'s'` mapping via `iconv_capability_compiled_in()` | Implemented (Finding 5). The `'s'` row carries a `requires_iconv: true` flag that the build-time predicate filters when the `iconv` cargo feature is off, mirroring upstream's `#ifdef ICONV_OPTION` (compat.c:716-718). Run-time `IconvSetting` gating is tracked under Finding 4 wiring. |
| `xattrs.c` (no iconv) | n/a | `crates/protocol/src/xattr/*` | Matches upstream (no transcoding for xattr names or values). |

## Findings

### Finding 1 (high severity, CLOSED): receiver-side symlink target is never transcoded

`crates/protocol/src/flist/read/extras.rs::read_symlink_target` now applies
`self.iconv.as_ref()`'s `remote_to_local` to the wire bytes before constructing
the `PathBuf`, returning `io::ErrorKind::InvalidData` on conversion failure and
borrowing the raw bytes through a `Cow` when no converter is configured. This
mirrors upstream `flist.c:1127-1150`'s `iconvbufs(ic_recv, ...)` call. The
runtime gate on `sender_symlink_iconv` follows the converter's presence: when
`Finding 4` wires `IconvSetting -> FilenameConverter` into `FileListReader`,
the read path transcodes; otherwise the bytes pass through untouched, which
matches upstream's behaviour when `ic_recv == (iconv_t)-1`.

Tests: `read_symlink_target_converts_latin1_wire_bytes_to_utf8` and
`read_symlink_target_without_iconv_preserves_wire_bytes` in
`crates/protocol/src/flist/read/tests.rs`.

### Finding 2 (high severity, CLOSED): sender-side symlink target is never transcoded

`crates/protocol/src/flist/write/encoding.rs::write_symlink_target` now calls
`self.apply_encoding_conversion(&target_bytes)` before emitting the
varint30(len) + raw bytes pair. The shared `apply_encoding_conversion` helper
transcodes via `local_to_remote` when a converter is configured and falls back
to `Cow::Borrowed` otherwise. This mirrors upstream `flist.c:1606-1621`'s
`iconvbufs(ic_send, ...)` call. The runtime gate on `sender_symlink_iconv` is
implicit in the converter's presence; runtime wiring is tracked under
Finding 4.

Tests: `write_symlink_target_transcodes_with_iconv_to_remote_charset` and
`write_symlink_target_without_iconv_emits_raw_bytes` in
`crates/protocol/src/flist/write/tests.rs`.

### Finding 3 (high severity, CLOSED): `--files-from` and protected-args bypass iconv

- Finding 3a (CLOSED): `crates/protocol/src/files_from.rs` now applies
  `iconv: Option<&FilenameConverter>` per entry with ICB_INCLUDE_BAD-equivalent
  fallback, mirroring upstream `forward_filesfrom_data()` in `io.c:417-452`.
  The pull-side daemon-transfer plumbing in
  `crates/core/src/client/remote/daemon_transfer/orchestration/transfer.rs`
  derives the converter from `config.iconv().resolve_converter()` gated on
  `protect_args.unwrap_or(false)`, mirroring `compat.c:799-806`'s
  `filesfrom_convert` predicate. The receiver-side decode path runs through
  `read_files_from_stream(..., iconv)` with the same conversion direction.
- Finding 3b (CLOSED): `crates/protocol/src/secluded_args.rs`
  `send_secluded_args` / `recv_secluded_args` accept `iconv: Option<&FilenameConverter>`
  and apply `local_to_remote` / `remote_to_local` per arg. Production
  send-side callers (SSH client, daemon client phase-2) derive the converter
  from `config.iconv().resolve_converter()`. Production recv-side callers
  (CLI server-mode stdin reader, daemon `module_access` phase-2 reader) pass
  `None` because `ic_recv` is unset at `read_args` time (`setup_iconv()` runs
  inside `setup_protocol()` AFTER the args exchange in upstream). Module-level
  iconv negotiation for the daemon recv path is tracked separately under
  Finding 4 wiring.

### Finding 4 (critical, systemic): `IconvSetting` is never converted to `FilenameConverter`

`crates/protocol/src/flist/read/mod.rs::FileListReader::with_iconv` and the
matching writer hook are present, plumbed through `transfer::ConnectionConfig::iconv`,
and consumed in `crates/transfer/src/{generator,receiver}/mod.rs` via
`with_iconv(converter.clone())`. However, **no production caller ever sets
that field**. The CLI parses `--iconv` into
`crates/core/src/client/config/iconv.rs::IconvSetting`, but the bridge that
turns `IconvSetting::Explicit { local, remote }` (or `LocaleDefault`) into a
`protocol::FilenameConverter` and feeds it into
`transfer::ServerConfigBuilder::iconv(...)` does not exist.

A grep for production constructors confirms it:

```
grep -rn 'FilenameConverter::new\|EncodingConverter::new\|converter_from_locale'
```

Every match outside the tests in `crates/protocol/src/iconv/` is documentation
or a re-export. `transfer::ServerConfigBuilder::iconv(...)` is similarly never
called from production code (only `core::client::config::builder::network::iconv`,
which sets the `IconvSetting`, not a converter).

The practical effect: even after Findings 1-3 are fixed, `--iconv` is a
pure no-op end to end. The CLI accepts the option, encodes it into the
forwarded SSH/daemon arg list (`IconvSetting::cli_value`), but the local
receiver/generator runs with `iconv: None` and therefore copies bytes
verbatim.

### Finding 5 (medium severity, CLOSED): `CF_SYMLINK_ICONV` is unconditionally advertised

`crates/transfer/src/setup/capability.rs` now carries a `requires_iconv: bool`
column on every `CapabilityMapping` entry; `'s'` (CF_SYMLINK_ICONV) is the
only row currently flagged. The const fn `iconv_capability_compiled_in()`
returns `cfg!(feature = "iconv")`, and both `build_capability_string` (the
emit path) and `build_compat_flags_from_client_info` (the parse path) skip
mappings whose `requires_iconv` is `true` when the cargo feature is off.

This mirrors upstream `compat.c:716-718` where the row is wrapped in
`#ifdef ICONV_OPTION`: a build without iconv neither advertises nor accepts
`CF_SYMLINK_ICONV`. Run-time gating on `IconvSetting` (so we omit `'s'` even
on iconv-enabled builds when the user did not pass `--iconv`) is tracked
under Finding 4 wiring. The new `transfer/iconv` cargo feature propagates
from `core/iconv` and the `protocol::CompatibilityFlags::SYMLINK_ICONV` bit
in batch headers is gated identically (`crates/cli/src/frontend/execution/drive/workflow/run.rs`,
`crates/transfer/src/setup/mod.rs`).

Tests: `build_capability_string_includes_symlink_iconv_when_iconv_compiled_in`
and `build_capability_string_omits_symlink_iconv_when_iconv_disabled` in
`crates/transfer/src/setup/tests.rs`.

### Finding 6 (low severity): log-line transcoding is absent

`log.c:251-371` runs every `rwrite()` payload through `ic_chck` (locale
sanitiser) or `ic_recv` (when the message is tagged UTF-8 from a sibling).
The oc-rsync logging crate does no transcoding. In an all-UTF-8 deployment
this is invisible. In mixed-locale deployments, log lines containing
non-ASCII paths can carry raw remote bytes. Acceptable for now, but worth
filing.

### Finding 7 (information): xattr names are not transcoded

Upstream `xattrs.c` does not call `iconvbufs`. Our xattr code does not
either. No divergence.

## Severity rollup

| Severity | Count | Status |
|---|---|---|
| Critical | 1 (Finding 4) | Open (in-progress; iconv resolver / runtime wiring) |
| High | 3 (Findings 1, 2, 3) | CLOSED (this PR + previous v0.6.1 PRs) |
| Medium | 1 (Finding 5) | CLOSED (this PR) |
| Low | 1 (Finding 6) | Open (logging-sink transcoding deferred) |
| Information | 1 (Finding 7) | n/a (no divergence) |

## Decision: ship the closures alongside the resolver work

The audit shipped first; the high-severity surface (Findings 1, 2, 3) and
the medium-severity capability gating (Finding 5) are now closed in this PR
on `release/v0.6.1`. The remaining gap is Finding 4 (the `IconvSetting ->
FilenameConverter` resolver and its plumbing through
`transfer::ServerConfigBuilder::iconv`); until that lands, the
flist read/write hooks, the symlink target hooks, the secluded-args hooks,
and the files-from hooks all run in pass-through mode (matching upstream's
behaviour when `iconv_opt == NULL`). Once Finding 4 wiring is complete, the
existing hooks transparently activate without further protocol or wire
changes, and the corresponding capability flag advertisement is already
gated on the cargo feature so we never lie to the peer about our build.

## Follow-up tasks

- Finding 1 (CLOSED): receiver-side symlink target now applies
  `remote_to_local` per the converter's presence; runtime gate inherited
  from Finding 4 wiring once it lands.
- Finding 2 (CLOSED): sender-side symlink target now applies
  `local_to_remote` via `apply_encoding_conversion`; runtime gate inherited
  from Finding 4 wiring.
- Finding 3a (CLOSED): iconv hook added to `--files-from` forwarding with
  RL_CONVERT semantics, gated on `protect_args` per `compat.c:799-806`.
- Finding 3b (CLOSED): iconv hook added to `send_secluded_args` and
  `recv_secluded_args`; recv-side daemon module-config iconv plumbing is
  tracked under Finding 4.
- Finding 4: add `IconvSetting -> FilenameConverter` translation in
  `core::client::config::iconv` (or a new `core::iconv::resolve`),
  and call `transfer::ServerConfigBuilder::iconv(...)` from the CLI
  config plumbing in `crates/cli/src/frontend/execution/drive/config.rs`.
  The runtime gate that activates Findings 1, 2, 3, 5 hangs off this work.
- Finding 5 (CLOSED): emission and acceptance of `'s'` (`CF_SYMLINK_ICONV`)
  is now gated on the `iconv` cargo feature via `requires_iconv` on
  `CapabilityMapping`, mirroring `compat.c:716-718`'s `#ifdef ICONV_OPTION`.
  Run-time `IconvSetting` gating is the remaining condition and will be
  added when Finding 4 wiring lands.
- Finding 6: route logging-sink output through a `FilenameConverter` when
  iconv is active and the message is local-origin.
