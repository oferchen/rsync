# Audit: `--iconv` feature design and wire-protocol surface

**Last verified:** 2026-05-01.

**Tracking:** no open issue matches `gh issue list --search "iconv"`. The
closest in-repo reference is the interop harness comment "#884: --iconv
charset conversion" in `tools/ci/run_interop.sh:2980-3087`. Earlier work
is in PR #1840 (`docs/audits/iconv-pipeline.md`) and PR #1996.

**Sources.** Upstream rsync 3.4.1 is expected at
`target/interop/upstream-src/rsync-3.4.1/`; that tree is absent from
this checkout, so file/line references below come from
`docs/audits/iconv-pipeline.md` (PR #1840, written with the tarball
unpacked). The fetch command in `/Users/ofer/devel/CLAUDE.md` rehydrates
it. Behavioural prose is cross-checked against
`docs/oc-rsync.1.md:688-693` and the upstream `rsync.1` section
"USING --iconv FOR CHARACTER SET CONVERSIONS".

This audit complements `docs/audits/iconv-pipeline.md` (gap inventory)
and covers feature design: option semantics, library choice, gating, and
interop scope.

## What `--iconv` does in upstream rsync

`--iconv=LOCAL,REMOTE` translates filenames (and several other byte
streams) between two charsets across the wire. The local side sees
`LOCAL`-encoded paths; the remote side sees `REMOTE`-encoded paths.
Special forms:

- `--iconv=.` uses the locale charset on both ends.
- `--iconv=-` or `--no-iconv` disables conversion.
- `--iconv=LOCAL` defaults the remote to the peer's locale.

Daemons may set a default via the `charset` keyword in
`oc-rsyncd.conf`; the user can override with `--no-iconv`.

Upstream call sites (per `docs/audits/iconv-pipeline.md`):

- `options.c` parses `--iconv` into `iconv_opt` (`LOCAL,REMOTE`).
- `compat.c:716-718` advertises `CF_SYMLINK_ICONV` only when iconv is
  compiled in; `compat.c:763-767` derives `sender_symlink_iconv` from
  the peer's `CF_SYMLINK_ICONV` bit AND the local `iconv_opt`.
- `flist.c:1579-1603` runs `iconvbufs(ic_send, ...)` on outgoing
  filenames; `flist.c:738-754` runs `iconvbufs(ic_recv, ...)` on
  incoming ones. `flist.c:1605-1621` and `1127-1150` extend the same
  to symlink targets, gated on `sender_symlink_iconv`.
- `io.c:416-452` runs `iconvbufs(ic_send, ...)` on `--files-from`
  strings (`filesfrom_convert`). `io.c:1240-1289` runs the receive
  direction in `read_line(..., RL_CONVERT)`.
- `io.c:983-1031` transcodes `MSG_*` text payloads.
- `rsync.c:283-320` transcodes each `--secluded-args` /
  `--protect-args` element.
- `log.c:251-371` transcodes log output via `ic_chck` or `ic_recv`.

Xattr names and values are not transcoded (`xattrs.c` makes no iconv
calls).

## Wire protocol implications

`--iconv` is **not** a wire-protocol extension. It rides on protocol 30+
behaviour that already exists, by:

1. Setting one bit in the compatibility-flags exchange:
   `CF_SYMLINK_ICONV` (bit 2). Each side OR's the bit only when its own
   `iconv_opt` is set; both must set it for symlink-target transcoding
   to engage. Wire encoding is the existing
   `CompatibilityFlags::SYMLINK_ICONV` byte (see
   `crates/protocol/src/compatibility/flags.rs` and the byte-level
   golden `crates/protocol/tests/compatibility_flags.rs:70-77`).
2. Translating filename, symlink-target, files-from, secluded-arg, and
   `MSG_*` byte streams in place. Wire framing is unchanged; only the
   bytes inside the existing length-prefixed payloads differ.
3. No new tags, frame types, or version bumps.

Order during a session:

1. Protocol version exchange (`@RSYNCD:` for daemon, version line for
   SSH). Iconv is not negotiated here.
2. Compatibility-flags exchange (protocol 30+). Each side advertises
   `CF_SYMLINK_ICONV` only when local iconv is active.
3. Strings transcoded inline at the call sites listed above, every
   time they cross the wire.

## Current oc-rsync state

Already wired:

- CLI parsing: `crates/cli/src/frontend/arguments/parser/mod.rs:315-316`
  reads `--iconv` and `--no-iconv`.
  `crates/cli/src/frontend/execution/options/iconv.rs::resolve_iconv_setting`
  returns an `IconvSetting`.
- Setting: `crates/core/src/client/config/iconv.rs::IconvSetting`
  (`Unspecified | Disabled | LocaleDefault | Explicit{local, remote}`)
  with `cli_value()` for forwarding.
- Conversion engine: `crates/protocol/src/iconv/{mod,converter,pair}.rs`
  exposes `FilenameConverter` / `EncodingConverter` backed by
  `encoding_rs`, with a UTF-8-only stub when the `iconv` Cargo feature
  is off. Tests cover round-trip, lossy detection, and aliases.
- File-list hooks: `crates/protocol/src/flist/read/name.rs::apply_encoding_conversion`
  and `crates/protocol/src/flist/write/encoding.rs::apply_encoding_conversion`.
- Connection plumbing:
  `crates/transfer/src/config/builder.rs:183-186::iconv()` stores an
  `Option<FilenameConverter>` on `ConnectionConfig`;
  `crates/transfer/src/{generator,receiver}/mod.rs` calls
  `with_iconv(converter.clone())`.
- Compatibility flag: `KnownCompatibilityFlag::SymlinkIconv`
  (`crates/protocol/src/compatibility/known.rs:21-23`) with bit 1 << 2
  and round-trip tests.
- Cargo features: workspace `iconv = ["core/iconv"]` (default on),
  `core/iconv = ["protocol/iconv"]`,
  `protocol/iconv = ["encoding_rs"]`.

Missing (per `docs/audits/iconv-pipeline.md`):

- No production code converts an `IconvSetting` into a
  `FilenameConverter`. `transfer::ServerConfigBuilder::iconv(...)` is
  never called outside tests, so end-to-end the option is a no-op
  (Finding 4, critical).
- Symlink targets are not transcoded in either direction; no
  `sender_symlink_iconv` plumbing (Findings 1 and 2).
- `--files-from` forwarding, `--secluded-args`, and `--protect-args`
  bypass iconv (Finding 3).
- `crates/transfer/src/setup/capability.rs` always emits `'s'` in the
  `-e.xxx` capability string regardless of `iconv_opt` (Finding 5).
- Logging-sink output is not transcoded (Finding 6).

## Proposed implementation plan

The pipeline audit lists per-call-site fixes. This section adds the
cross-cutting design choices.

**Feature gating.** Keep the layered Cargo feature
(`workspace::iconv -> core::iconv -> protocol::iconv -> encoding_rs`).
With the feature off, `FilenameConverter::new` already errors for any
non-UTF-8 charset, and `to_local`/`to_remote` are byte-wise identity. A
user invocation with `--iconv` and the feature off must emit a single
`rsync_error!(1, ...)` at config build time, not a silent no-op. Add
that diagnostic in
`crates/cli/src/frontend/execution/drive/config.rs` next to the
existing `IconvSetting` consumer.

**Crate placement.** Conversion primitives stay in `protocol::iconv`
(no unsafe, depends only on `encoding_rs`). The
`IconvSetting -> FilenameConverter` adapter belongs in `core` next to
the existing setting (`crates/core/src/client/config/iconv.rs`).
`transfer` consumes only the resolved `Option<FilenameConverter>`.

**Library choice.**

| Library | Pros | Cons |
|---|---|---|
| `encoding_rs` (current) | Pure Rust, MIT/Apache, used by Firefox, no C dep, WHATWG label aliases, builds cross-platform. Already a dependency. | Smaller charset coverage than libc iconv (no EBCDIC, fewer rare CJK variants). Lossy semantics match WHATWG, not GNU iconv. |
| System `iconv` via `libc` | Maximum coverage, byte-identical to upstream rsync on the same host. | C dependency, varies between glibc / musl / BSD / Windows (no system iconv on Windows by default), `unsafe` FFI. Conflicts with the unsafe-code policy outside `fast_io`. |
| `iconv` crate (libc wrapper) | Same coverage as system iconv with safer API. | Still ties to a system C library; Windows needs `win-iconv`; larger interop surface. |

Recommendation: stay on `encoding_rs`. Coverage is sufficient for the
charsets that matter (UTF-8, ISO-8859 family, Windows-125x, EUC-*,
Shift_JIS, GB18030, Big5, KOI8-R), the unsafe-code policy is honoured,
and CI cross-compiles without extra system deps. Document the coverage
gap in `docs/oc-rsync.1.md` so users hitting an obscure charset get a
clear error rather than mojibake.

**Capability advertisement.** Gate the `'s'` capability character on
`IconvSetting` being active rather than the unconditional
`CAPABILITY_MAPPINGS` entry, and OR `CF_SYMLINK_ICONV` into the local
flags only when iconv is active. Mirror `compat.c:716-718` exactly.

## Interop test plan

Targeted scenarios under `tools/ci/run_interop.sh::test_iconv`
(partially present) and as nextest cases under
`crates/protocol/tests/`:

1. Identity round-trip (`--iconv=UTF-8,UTF-8`) against upstream 3.0.9,
   3.1.3, and 3.4.1, push and pull, daemon and SSH transports. Already
   stubbed at `run_interop.sh:3015-3060`.
2. Cross-charset (`--iconv=UTF-8,ISO-8859-1`) push and pull,
   round-tripping a non-ASCII filename and a non-ASCII symlink target.
   Verify the wire bytes with a golden trace.
3. Files-from forwarding with non-ASCII paths under `--secluded-args`,
   confirming both directions transcode.
4. `--no-iconv` and `--iconv=-` against an upstream daemon with a
   `charset` setting; confirm we override correctly.
5. Lossy-conversion error: `--iconv=UTF-8,ASCII` with a path
   containing `é`; assert non-zero exit, role `[sender]`, exit code 23.
6. Compatibility-flag wire test: with iconv off,
   `CF_SYMLINK_ICONV` is **not** advertised; with iconv on, it is.
   Compare a captured trace against upstream byte-for-byte.

## Edge cases and risks

- **Non-UTF-8 paths on Linux.** `OsStr::as_encoded_bytes()` may yield
  non-UTF-8 (filesystem byte stream). Upstream treats paths as opaque
  bytes. Match that: surface `EncodingError::ConversionFailed` with
  the offending bytes attached.
- **Windows path semantics.** Windows file APIs are UTF-16. Iconv runs
  after path-flattening to bytes; for non-UTF-8 remote charsets the
  round-trip becomes UTF-16 -> UTF-8 -> remote bytes. Document that
  `--iconv` on Windows assumes the local charset is UTF-8.
- **Lossy conversions.** `encoding_rs::Encoding::encode` returns
  `had_errors=true` rather than substituting a replacement. Propagate
  as an error (matches upstream, which fails the file rather than
  mapping to `?`).
- **Files-from with embedded NULs.** Upstream
  `read_line(..., RL_CONVERT)` splits on `\0` and transcodes per
  record. Apply the same record boundary before invoking the
  converter.
- **`MSG_DELETED` forwarding.** Upstream uses `ic_send` because the
  path was native on the receiver. We do not currently relay
  `MSG_DELETED` from receiver to generator, so this hook is a no-op
  until that path lands.
- **Daemon `charset` keyword.** Mirror upstream by letting the daemon
  set a default the client can override with `--no-iconv`. Add a
  config-file test under `crates/daemon/`.

## Out of scope

- Wire-protocol extensions or new `MSG_*` frames for charset
  negotiation. `CF_SYMLINK_ICONV` plus inline transcoding suffices.
- Switching to system `iconv`. Tradeoffs above; stay on
  `encoding_rs`.
- Locale detection beyond `LC_CTYPE` UTF-8 fallback. Upstream itself is
  conservative.
- Per-file charset overrides. Not in upstream.
- Re-running the per-site gap fixes; those follow-ups are owned by
  `docs/audits/iconv-pipeline.md`. This audit only adds design
  context.
