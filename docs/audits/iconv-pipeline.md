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
| `flist.c:1127-1150` `recv_file_entry()` symlink target `iconvbufs(ic_recv, ...)` gated on `sender_symlink_iconv` | remote -> local | `crates/protocol/src/flist/read/extras.rs::read_symlink_target` | Missing. No iconv call, no double-buffer allocation, no `sender_symlink_iconv` gate. |
| `flist.c:1579-1603` `send_file_entry()` filename `iconvbufs(ic_send, ...)` (dirname + '/' + basename) | local -> remote | `crates/protocol/src/flist/write/encoding.rs::apply_encoding_conversion` (called from `write_entry`) | Implemented but never wired (see Finding 4). |
| `flist.c:1605-1621` `send_file_entry()` symlink target `iconvbufs(ic_send, ...)` gated on `symlink_len && sender_symlink_iconv` | local -> remote | `crates/protocol/src/flist/write/encoding.rs::write_symlink_target` | Missing. No iconv call, no `sender_symlink_iconv` gate. |
| `io.c:416-452` `forward_filesfrom_data()` per-string `iconvbufs(ic_send, ...)` for `--files-from` over the wire (`filesfrom_convert`) | local -> remote | `crates/protocol/src/files_from.rs` (no iconv hook) | Missing. |
| `io.c:983-1031` `send_msg()` text-message `iconvbufs(ic_send, ...)` for `MSG_*` UTF-8 conversion | local -> remote | `crates/protocol` multiplex sender (`envelope/`, `multiplex/`) | Missing. Text messages are written verbatim. Acceptable when both peers use UTF-8 only. |
| `io.c:1240-1289` `read_line()` with `RL_CONVERT` -> `iconvbufs(ic_recv, ...)` for daemon-side arg reading | remote -> local | `crates/protocol/src/secluded_args.rs::recv_secluded_args` | Missing. |
| `io.c:1559-1591` `MSG_DELETED` payload `iconvbufs(ic_send, ...)` (note: this is `ic_send` because the path was native on the receiver and is being forwarded to the generator/sender peer for logging) | local -> remote | oc-rsync does not currently transmit `MSG_DELETED` from the receiver back to the generator/sender. | Out of scope until MSG_DELETED forwarding lands. Tracked separately. |
| `rsync.c:283-320` `send_protected_args()` per-arg `iconvbufs(ic_send, ...)` | local -> remote | `crates/protocol/src/secluded_args.rs::send_secluded_args` | Missing. |
| `log.c:251-371` `rwrite()` log-line transcoding via `ic_recv` (when `is_utf8`) or `ic_chck` (locale) | mixed | `crates/logging/src/...` | Missing (`ic_chck` not modeled at all). Acceptable in an all-UTF-8 environment but should be tracked. |
| `compat.c:716-718, 763-767` `CF_SYMLINK_ICONV` advertised only when `iconv_opt` is set; `sender_symlink_iconv` derived from peer side | negotiation | `crates/transfer/src/setup/capability.rs::build_capability_string` always emits `s`; `KnownCompatibilityFlag::SymlinkIconv` exists but is not driven by `IconvSetting` | Divergent. |
| `xattrs.c` (no iconv) | n/a | `crates/protocol/src/xattr/*` | Matches upstream (no transcoding for xattr names or values). |

## Findings

### Finding 1 (high severity): receiver-side symlink target is never transcoded

`crates/protocol/src/flist/read/extras.rs::read_symlink_target` reads the raw
bytes from the wire and stores them in a `PathBuf` without ever calling
`FilenameConverter::remote_to_local`. Upstream `flist.c:1127-1150` doubles the
allocation, reads into the tail, then runs `iconvbufs(ic_recv, ...)` on the
buffer, but only when `sender_symlink_iconv` is true. oc-rsync handles
neither the conversion nor the gating flag.

This causes a transfer with a non-UTF-8 sender locale to land mojibake symlink
targets on the receiver side even when the user requested `--iconv`.

### Finding 2 (high severity): sender-side symlink target is never transcoded

`crates/protocol/src/flist/write/encoding.rs::write_symlink_target` writes
`target.as_os_str().as_encoded_bytes()` straight to the wire. Upstream
`flist.c:1606-1621` runs `iconvbufs(ic_send, ...)` over the symlink target
into a separate buffer, again gated on `sender_symlink_iconv`. The oc-rsync
counterpart skips both.

### Finding 3 (high severity): `--files-from` and protected-args bypass iconv

- `crates/protocol/src/files_from.rs` has no iconv hook. Upstream
  `io.c:417-452` runs each null-terminated string through `iconvbufs(ic_send, ...)`
  before forwarding when `filesfrom_convert` is true (i.e. `protect_args && files_from`
  with iconv configured per `compat.c:799-806`). The receiver side runs
  `read_line(..., RL_CONVERT)` (`io.c:1276-1287`), invoking `iconvbufs(ic_recv, ...)`.
- `crates/protocol/src/secluded_args.rs` (`send_secluded_args` /
  `recv_secluded_args`) writes and reads bytes as-is. Upstream
  `rsync.c:286-313` runs each protected arg through `iconvbufs(ic_send, ...)`
  before writing.

Together this means non-ASCII paths passed via `--files-from` or transmitted
through `--secluded-args`/`--protect-args` are not transcoded.

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

### Finding 5 (medium severity): `CF_SYMLINK_ICONV` is unconditionally advertised

`crates/transfer/src/setup/capability.rs` puts `'s'` into the
`-e.xxx` capability string for every session, regardless of whether the user
asked for `--iconv`. Upstream `compat.c:716-718` only sets `CF_SYMLINK_ICONV`
when `ICONV_OPTION` is compiled in, and `compat.c:763-767` only treats the
peer as a symlink-iconv sender when `iconv_opt` is non-null. We will start
asserting `s` even when iconv is disabled, which is harmless to upstream
peers (they intersect with their own `iconv_opt`) but is still a divergence
from upstream behaviour and a bug magnet for anyone reading interop traces.

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

| Severity | Count |
|---|---|
| Critical | 1 (Finding 4) |
| High | 3 (Findings 1, 2, 3) |
| Medium | 1 (Finding 5) |
| Low | 1 (Finding 6) |
| Information | 1 (Finding 7) |

## Decision: audit only, no in-PR fix

Each individual gap is small in code size but the set is interlocked:
fixing Findings 1-3 without Finding 4 changes nothing observable, and
fixing Finding 4 without Findings 1-3 leaves the symlink and
`--files-from` paths broken. A correct fix needs:

1. A new `IconvSetting -> FilenameConverter` adapter in `core` (or a new
   `core::iconv` module) wired through the existing
   `transfer::ServerConfigBuilder::iconv` setter.
2. A `sender_symlink_iconv` flag negotiated from the compat-flags exchange
   and threaded into both `FileListReader` and `FileListWriter`.
3. iconv hooks added to `read_symlink_target`, `write_symlink_target`,
   `send_secluded_args`, `recv_secluded_args`, and `files_from` forwarding.
4. The capability string emit gated on `iconv_opt.is_some()` (Finding 5).
5. Regression tests using `tempfile::TempDir` and `EnvGuard`, plus a
   golden wire test that includes a non-ASCII path and a non-ASCII symlink
   target round-tripping through `--iconv=utf-8,latin1`.

That is more than a 'small isolated fix' per the task scoping rules, so
this PR ships the audit only and links follow-ups for each finding.

## Follow-up tasks

- Finding 1 + 2: implement `sender_symlink_iconv` gating and apply iconv to
  symlink targets in both directions. Add round-trip golden test.
- Finding 3a: add iconv hook to `--files-from` forwarding (RL_CONVERT
  semantics).
- Finding 3b: add iconv hook to `send_secluded_args` and
  `recv_secluded_args`.
- Finding 4: add `IconvSetting -> FilenameConverter` translation in
  `core::client::config::iconv` (or a new `core::iconv::resolve`),
  and call `transfer::ServerConfigBuilder::iconv(...)` from the CLI
  config plumbing in `crates/cli/src/frontend/execution/drive/config.rs`.
- Finding 5: gate emission of `'s'` in `CAPABILITY_MAPPINGS` on the
  resolved `iconv_opt.is_some()` (mirroring `compat.c:716-718`). The
  flag negotiation table can stay as is; only the advertisement condition
  needs to change.
- Finding 6: route logging-sink output through a `FilenameConverter` when
  iconv is active and the message is local-origin.
