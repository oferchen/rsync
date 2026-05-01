# Audit: `--iconv` parsed but never applied (inert option)

Tracking: oc-rsync tasks #1909, #1910, #1918.

Companion to [`iconv-pipeline.md`](./iconv-pipeline.md). That document maps
every upstream `iconvbufs()` call site to its oc-rsync counterpart. This
document narrows in on the single architectural dead-end that makes every
gap in that pipeline simultaneously unobservable: `--iconv` parses cleanly
on the CLI, validates, persists onto `ClientConfig`, and even re-emits to
the remote peer over SSH, but the in-process side of the transfer never
receives a `FilenameConverter`. The flag is fully inert for local-side
file-list ingest, file-list emit, filter matching, and daemon module
serving.

## Severity

**Critical.** With `--iconv=utf-8,latin1` (or any non-identity charset
pair), filenames containing non-ASCII bytes are silently passed through
without transcoding on every local-side code path. The user receives no
error, no warning, and a successful exit code. Files that should round-trip
between mismatched locales are stored or matched against raw bytes from
the wire, producing mojibake on disk and silent filter-rule mismatches.

The `--iconv` plumbing is not a stub or a `todo!()`. Every layer compiles,
every type exists, and every consumer (`FileListReader::with_iconv`,
`FileListWriter::with_iconv`) is already wired to call into the iconv
hook *if* a converter is present. The defect is the absent bridge between
the CLI-side `IconvSetting` value object and the transfer-side
`Option<FilenameConverter>` it should resolve into.

## Discovered in

Audit performed 2026-04-29 on branch `feat/throughput-optimizations`
(commit `ad5feca7`), per tasks #1909 (audit) and #1918 (write report).
Read-only grep over `crates/{cli,core,transfer,engine,protocol,daemon,filters}`
plus `Cargo.toml` feature gating verification.

## Evidence

### CLI-side: parses, stores, forwards over SSH

Every site below is exercised in production. None of them produces a
`FilenameConverter`.

- `crates/cli/src/frontend/execution/options/iconv.rs:16` -
  `resolve_iconv_setting()` parses `--iconv` / `--no-iconv` into an
  `IconvSetting` enum value.
- `crates/core/src/client/config/iconv.rs:24` - `IconvSetting::parse()`
  validates the `LOCAL,REMOTE` form and rejects malformed input.
- `crates/core/src/client/config/client/mod.rs:169` -
  `ClientConfig.iconv: IconvSetting` field stores the parsed setting.
- `crates/core/src/client/config/builder/network.rs:93` -
  `ClientConfigBuilder::iconv(IconvSetting)` setter persists the value.
- `crates/cli/src/frontend/execution/drive/config.rs:252` - the **only**
  production call site of the setter:
  `.iconv(inputs.iconv.clone())`. After this point nothing downstream
  consumes the value for local-side conversion.
- `crates/core/src/client/config/iconv.rs:77` - `IconvSetting::cli_value()`
  re-renders the setting back into `--iconv=...` for the *remote* peer
  (SSH/daemon arg vector). This is the only consumer of the field outside
  trace logging. The remote peer transcodes; the local process does not.

### Transfer-side: complete consumer wiring, zero producer

The transfer crate already accepts a converter. Nobody hands it one.

- `crates/transfer/src/config/mod.rs:101` -
  `ConnectionConfig.iconv: Option<FilenameConverter>` is declared.
- `crates/transfer/src/config/builder.rs:184` -
  `ServerConfigBuilder::iconv(Option<FilenameConverter>)` setter exists.
  **Zero call sites in production code.** Confirmed via repo-wide grep:
  no caller in `cli`, `core`, `transfer`, `engine`, `daemon`, or any
  binary entry point invokes this method. The setter is dead until a
  bridge is written.
- `crates/transfer/src/receiver/mod.rs:369` -
  `if let Some(ref converter) = self.config.connection.iconv { reader = reader.with_iconv(converter.clone()); }`.
  The `Option` is always `None`, so the branch is never taken in
  production.
- `crates/transfer/src/generator/mod.rs:564` - mirror of the above for
  the file-list writer path. Same dead branch.

### Protocol-side: iconv-aware encode/decode is ready and waiting

Both wire-encoding sites have a fully implemented iconv hook that activates
when a converter is present. Both observe `None` in production.

- `crates/protocol/src/flist/read/name.rs:101` -
  `apply_encoding_conversion()` runs `FilenameConverter::remote_to_local`
  on the decoded name when the reader has a converter set.
- `crates/protocol/src/flist/write/encoding.rs:303` -
  `apply_encoding_conversion()` runs `FilenameConverter::local_to_remote`
  on the encoded name when the writer has a converter set.
- `crates/protocol/src/iconv/mod.rs:33` - `FilenameConverter`,
  `EncodingConverter`, `EncodingPair`, `converter_from_locale` are
  re-exported. `FilenameConverter::new(local, remote)`,
  `FilenameConverter::identity()`, and `FilenameConverter::new_lenient()`
  constructors are all present. The type system is complete.

### Daemon-side: refuse-list only, no module directive

- `crates/daemon/src/daemon/sections/module_parsing.rs:33-34` -
  `"iconv"` and `"no-iconv"` appear only in `REFUSABLE_OPTIONS`. The
  daemon recognises that a client may *attempt* to pass `--iconv`, and
  can refuse it via `refuse options`, but there is no
  `iconv = <charset>` server-side module directive. Compare upstream
  `clientserver.c::send_listing` and `loadparm.c::iconv_charset` per-module.

### Filter-side: zero awareness

- `crates/filters/` - repo-wide grep for `iconv|FilenameConverter|EncodingConverter`
  returns zero hits. Filter-rule path matching operates on raw remote-encoded
  bytes when iconv is in use, so user-supplied include/exclude patterns
  silently fail to match transcoded names.

### Cargo gating: feature is on by default and compiles correctly

- `crates/protocol/Cargo.toml:27` - `iconv = ["encoding_rs"]`.
- `crates/core/Cargo.toml:65` -
  `default = ["zstd", "lz4", "xattr", "iconv"]`.
- `crates/core/Cargo.toml:82` - `iconv = ["protocol/iconv"]`.
- Workspace `Cargo.toml:29,55` propagates `iconv` into both crates.

The feature is enabled by default. The dead-end is not a feature-gate
problem; it is a missing function call.

## Required components

### Already present

- **`FilenameConverter`** type at `crates/protocol/src/iconv/mod.rs`
  (constructors: `new`, `identity`, `new_lenient`).
- **`EncodingConverter` / `EncodingPair`** for the higher-level API.
- **`ConnectionConfig.iconv: Option<FilenameConverter>`** field.
- **`ServerConfigBuilder::iconv()`** setter.
- **Reader/Writer plumbing** in receiver and generator
  (`with_iconv(converter.clone())`).
- **Wire-level encode/decode hooks** in
  `flist/read/name.rs` and `flist/write/encoding.rs`.

### Missing

1. **Bridge function**: `IconvSetting -> Option<FilenameConverter>`.
   Logical home is `crates/core/src/client/config/iconv.rs` next to
   `IconvSetting::cli_value()`, e.g. `IconvSetting::resolve_converter(&self) -> Option<FilenameConverter>`.
   - `IconvSetting::Unspecified` -> `None`.
   - `IconvSetting::Disabled` -> `None`.
   - `IconvSetting::LocaleDefault` -> `Some(converter_from_locale())`.
   - `IconvSetting::Explicit { local, remote }` ->
     `Some(FilenameConverter::new(&local, &remote)?)`.
2. **Bridge call site**: invoke
   `ServerConfigBuilder::iconv(setting.resolve_converter())` from the
   CLI plumbing where the connection config is built (immediate
   neighbour of `crates/cli/src/frontend/execution/drive/config.rs:252`)
   and from any other code path that constructs a `ConnectionConfig`.
3. **Filter-rule transcoding hook**: pre-transcode include/exclude pattern
   inputs (or post-transcode candidate names) inside `crates/filters/`
   so filter matching sees consistent local-charset bytes.
4. **Daemon module directive**: extend
   `crates/daemon/src/daemon/sections/module_parsing.rs` to recognise
   `iconv = <charset>` per-module and apply it server-side, mirroring
   upstream `loadparm.c`.
5. **Hard error when feature is off**: when `--iconv` is supplied and the
   `iconv` cargo feature is disabled, fail at config build with a clear
   error rather than silently no-opping. (Today the `EncodingConverter`
   stub at `crates/protocol/src/iconv/mod.rs:300` only rejects non-UTF-8
   encodings, but the gap above means we never even reach that path.)
6. **Round-trip golden tests** with non-ASCII filenames and a non-ASCII
   symlink target through `--iconv=utf-8,latin1`, plus a parity test
   against upstream rsync 3.4.1.

## Remediation plan

The fix decomposes naturally into the existing task chain. Each task is
independently mergeable; they share no breaking interfaces, only
incremental wiring.

| Task | Scope |
|---|---|
| #1909 | This audit: identify the parse-but-never-applied dead-end. |
| #1910 | Locate or design the `FilenameConverter` trait. (Already exists in `crates/protocol/src/iconv/mod.rs` - confirm and document.) |
| #1911 | Wire `IconvSetting -> FilenameConverter` at config build (the bridge function plus the `ServerConfigBuilder::iconv()` call). |
| #1912 | Apply `FilenameConverter` on sender file-list emit (verify `generator/mod.rs:564` activates once #1911 lands). |
| #1913 | Apply `FilenameConverter` on receiver file-list ingest (verify `receiver/mod.rs:369` activates once #1911 lands). |
| #1914 | Apply `FilenameConverter` in filter-rule path matching (`crates/filters/`). |
| #1915 | Reject `--iconv` with a hard error when the iconv feature is compiled out. |
| #1916 | Add interop test against upstream rsync 3.4.1 with `--iconv=utf-8,latin1` over SSH and daemon transports. |
| #1917 | Daemon module config: parse `iconv = <charset>` directive and propagate into the per-connection `ConnectionConfig`. |
| #1918 | This document. |
| #1919 | Golden byte tests for converted filename wire encoding (both directions). |

The companion document `iconv-pipeline.md` enumerates additional gaps
(symlink targets, `--files-from`, secluded args, `CF_SYMLINK_ICONV`
advertisement, log-line transcoding) that become visible only after
#1911 is complete. Those are tracked in that document's findings 1, 2,
3, 5, 6 and are not duplicated here.

## Upstream references

Source tree: `target/interop/upstream-src/rsync-3.4.1/`.

- `io.c::iconv_buf` - core `iconvbufs()` helper invoked everywhere
  rsync transcodes a buffer (file lists, `--files-from`, log lines,
  `MSG_*` text, deleted-path forwarding).
- `options.c::recv_iconv_settings` (and `parse_iconv` /
  `setup_iconv` neighbours) - parses `--iconv=LOCAL,REMOTE` on the
  receiver side and configures `ic_send` / `ic_recv` global converter
  pairs. This is the producer side oc-rsync currently lacks.
- `flist.c::iconv_for_local` (file-list path entry transcode) and
  `flist.c::recv_file_entry` / `send_file_entry` iconv call-outs
  (`flist.c:738-754`, `1127-1150`, `1579-1603`, `1605-1621`) -
  the canonical reference for receiver-side and sender-side
  filename and symlink-target transcoding semantics, including the
  `sender_symlink_iconv` gate from `compat.c:763-767`.
- `compat.c:716-718` - upstream gates `CF_SYMLINK_ICONV` advertisement
  on whether iconv is configured, which is exactly the producer-side
  signal oc-rsync needs to drive its capability-string emission and
  is also missing today (see `iconv-pipeline.md` Finding 5).
- `loadparm.c::iconv_charset` and `clientserver.c` module-config
  reading - the upstream daemon's per-module `charset` /
  `iconv_charset` directive that has no oc-rsync equivalent.

## Summary

`--iconv` is currently a wire-only option in oc-rsync: the local process
parses it, validates it, stores it on `ClientConfig`, and forwards it to
the remote peer in the SSH/daemon arg vector. The remote peer transcodes
correctly; the local process does not. The bridge from
`core::client::config::iconv::IconvSetting` to
`protocol::iconv::FilenameConverter` is the single missing function in an
otherwise complete pipeline. Until task #1911 lands, every other piece
of the iconv stack (reader/writer hooks, wire encode/decode, capability
negotiation, daemon plumbing, filter matching) sees `None` and silently
short-circuits to a verbatim byte copy.
