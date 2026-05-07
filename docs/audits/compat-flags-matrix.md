# Compatibility Flags Matrix vs Upstream rsync 3.4.1

Task #2106. Audit of the protocol-30+ compatibility-flag bitfield
(`compat_flags`), the `-e.<chars>` capability string, and the CLI options
that drive both. This document supersedes the prior internal-pipeline
matrix; that material is summarised in the appendix below.

Sources:

- Upstream `target/interop/upstream-src/rsync-3.4.1/compat.c` lines
  117-125 (`CF_*` bit definitions), 161-179 (`set_allow_inc_recurse`),
  710-783 (compat-flag exchange and post-exchange variable wiring).
- Upstream `target/interop/upstream-src/rsync-3.4.1/options.c` lines
  113 (`allow_inc_recurse = 1`), 614-617 (`--inc-recursive` /
  `--no-inc-recursive` popt entries), 3003-3047 (`maybe_add_e_option`
  capability string).
- `crates/protocol/src/compatibility/flags.rs`
  (`CompatibilityFlags` bitfield, varint codec).
- `crates/protocol/src/compatibility/known.rs`
  (`KnownCompatibilityFlag` enum, canonical `CF_*` names).
- `crates/transfer/src/setup/capability.rs`
  (`CAPABILITY_MAPPINGS`, `build_capability_string`,
  `build_compat_flags_from_client_info`, `parse_client_info`,
  `client_has_pre_release_v_flag`).
- `crates/transfer/src/setup/compat.rs` (`write_compat_flags`,
  `exchange_compat_flags_direct`).
- `crates/transfer/src/setup/negotiator.rs` (`RsyncNegotiator`).
- `crates/transfer/src/lib.rs` lines 413-470 (`allow_inc_recurse`
  computation, post-exchange application of `CF_INPLACE_PARTIAL_DIR`
  and `CF_AVOID_XATTR_OPTIM`).
- `crates/core/src/client/remote/invocation/builder.rs` lines 169-194
  and 458-576 (single-letter flag string + capability string for SSH).
- `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs`
  line 155 (capability string for daemon transfers).

## 1. Wire field

`compat_flags` is a `u32` bitfield exchanged after protocol negotiation
on protocol >= 30. Upstream encodes it with `write_varint` (signed-i32
varint) at `compat.c:738` and reads it with `read_varint` at
`compat.c:740`. A pre-release client that advertises `'V'` instead of
`'v'` triggers a single-byte `write_byte` path (`compat.c:733-736`)
which we mirror in `crates/transfer/src/setup/compat.rs::write_compat_flags`.

oc-rsync defines the bitfield in
`crates/protocol/src/compatibility/flags.rs`:

| Constant                                       | Bit      | Upstream macro            |
|------------------------------------------------|----------|---------------------------|
| `CompatibilityFlags::INC_RECURSE`              | `1 << 0` | `CF_INC_RECURSE`          |
| `CompatibilityFlags::SYMLINK_TIMES`            | `1 << 1` | `CF_SYMLINK_TIMES`        |
| `CompatibilityFlags::SYMLINK_ICONV`            | `1 << 2` | `CF_SYMLINK_ICONV`        |
| `CompatibilityFlags::SAFE_FILE_LIST`           | `1 << 3` | `CF_SAFE_FLIST`           |
| `CompatibilityFlags::AVOID_XATTR_OPTIMIZATION` | `1 << 4` | `CF_AVOID_XATTR_OPTIM`    |
| `CompatibilityFlags::CHECKSUM_SEED_FIX`        | `1 << 5` | `CF_CHKSUM_SEED_FIX`      |
| `CompatibilityFlags::INPLACE_PARTIAL_DIR`      | `1 << 6` | `CF_INPLACE_PARTIAL_DIR`  |
| `CompatibilityFlags::VARINT_FLIST_FLAGS`       | `1 << 7` | `CF_VARINT_FLIST_FLAGS`   |
| `CompatibilityFlags::ID0_NAMES`                | `1 << 8` | `CF_ID0_NAMES`            |

`CompatibilityFlags::ALL_KNOWN` masks all nine bits; bits outside that
mask round-trip via `from_bits` so future upstream additions are
preserved on the wire even when oc-rsync cannot interpret them.

The negotiation is unidirectional: only the server writes `compat_flags`
(`compat.c:711-738`), the client reads. The single source of truth on
our side is `transfer::setup::capability::CAPABILITY_MAPPINGS`, which
mirrors `compat.c:712-734` row-by-row. Both
`build_capability_string()` (what we advertise as `-e.<chars>`) and
`build_compat_flags_from_client_info()` (what we set on the server when
processing a client's `-e` argument) iterate the same table.

## 2. Per-flag propagation

For each `CF_*` bit the entries below describe the client-side
trigger, the server-side rule that turns the bit on, the runtime
effect, and the matching oc-rsync code path.

### CF_INC_RECURSE (`'i'`)

- Advertised when `allow_inc_recurse == 1`. Upstream initialises this
  to `1` at `options.c:113` and clears it in `set_allow_inc_recurse`
  (`compat.c:161-178`) when `--no-inc-recursive`/`--no-i-r` is set,
  when recursion is off, when `--qsort` is in effect, or when a
  receiver also asks for `--delete-before/-after`, `--delay-updates`,
  or `--prune-empty-dirs`.
- Server sets `compat_flags |= CF_INC_RECURSE` only if it advertised
  `allow_inc_recurse` and the client-info string contains `'i'`
  (`compat.c:712`).
- Effect: `inc_recurse = 1`, the file list is exchanged in
  incremental segments.
- oc-rsync mapping: row `{ char: 'i', requires_inc_recurse: true }`
  in `CAPABILITY_MAPPINGS`. The `allow_inc_recurse` predicate is
  computed in `crates/transfer/src/lib.rs:416-419` and matches
  upstream's gate (recursion required, `qsort` excluded, receiver
  excludes `delete`/`prune_empty_dirs`).

### CF_SYMLINK_TIMES (`'L'`)

- Advertised whenever upstream is built with `CAN_SET_SYMLINK_TIMES`
  (`options.c:3024`); the bit is then set unconditionally on the
  server (`compat.c:713-715`).
- Effect: senders may receive symlink mtimes; receivers honour
  `lutimes`/`utimensat` for symlinks.
- oc-rsync mapping: row `{ char: 'L', platform_ok: cfg!(unix) }` in
  `CAPABILITY_MAPPINGS`. Windows clears the row (`platform_ok =
  false`) so neither the capability char nor the bit is exchanged
  there.

### CF_SYMLINK_ICONV (`'s'`)

- Advertised iff upstream was built with `ICONV_OPTION` (gates the
  `'s'` capability char at `options.c:3027` and the bit at
  `compat.c:716-718`).
- Effect: the sender re-encodes symlink targets via the negotiated
  `--iconv` charset.
- oc-rsync mapping: row `{ char: 's', requires_iconv: true }` in
  `CAPABILITY_MAPPINGS`, gated by
  `iconv_capability_compiled_in()` which evaluates
  `cfg!(feature = "iconv")`. When the cargo feature is off, neither
  the capability char nor the bit is exchanged.

### CF_SAFE_FLIST (`'f'`)

- Advertised unconditionally by upstream (`options.c:3029`).
- Server sets the bit when the client advertised `'f'`
  (`compat.c:719-720`).
- Effect: receivers tolerate non-fatal I/O errors during file-list
  generation and continue. Implicitly true when
  `protocol_version >= 31` (`compat.c:775`).
- oc-rsync mapping: row `{ char: 'f' }`, always advertised on
  protocol >= 30 transfers.

### CF_AVOID_XATTR_OPTIM (`'x'`)

- Advertised unconditionally by upstream (`options.c:3030`).
- Server sets the bit when the client advertised `'x'`
  (`compat.c:721-722`).
- Effect: gates `want_xattr_optim` on protocol 31+
  (`compat.c:746`); when missing, both peers fall back to xattr-free
  behaviour. oc-rsync also uses absence of this bit on the server side
  as the signal that the remote daemon was built without xattr
  support, downgrading `--xattrs` with a warning rather than aborting
  (`crates/transfer/src/lib.rs:449-463`).
- oc-rsync mapping: row `{ char: 'x' }`, always advertised.

### CF_CHKSUM_SEED_FIX (`'C'`)

- Advertised unconditionally by upstream (`options.c:3031`).
- Server sets the bit when the client advertised `'C'`
  (`compat.c:723-724`).
- Effect: drives `proper_seed_order = 1` (`compat.c:747`), which
  selects the corrected MD5 seed feed order on protocol 30+.
- oc-rsync mapping: row `{ char: 'C' }`, always advertised.

### CF_INPLACE_PARTIAL_DIR (`'I'`)

- Advertised unconditionally by upstream (`options.c:3032`).
- Server sets the bit when the client advertised `'I'`
  (`compat.c:725-726`).
- Effect: when the receiver has `--partial-dir` configured, basis
  files coming from the partial directory are opened with the
  per-file `inplace = 1` shortcut (`compat.c:777-778`,
  `receiver.c:797`).
- oc-rsync mapping: row `{ char: 'I' }`. Post-exchange, we apply the
  same one-line gate at
  `crates/transfer/src/lib.rs:442-447`: if the negotiated flags
  contain `INPLACE_PARTIAL_DIR` *and* `config.has_partial_dir`, we set
  `config.write.inplace_partial = true`.

### CF_VARINT_FLIST_FLAGS (`'v'` / pre-release `'V'`)

- Advertised unconditionally by upstream as `'v'`
  (`options.c:3033`); pre-release builds used `'V'`, which we still
  recognise for backward compatibility.
- Server sets the bit when the client advertised `'v'`
  (`compat.c:729-732`) and additionally enables
  `do_negotiated_strings`. The pre-release `'V'` path forces the bit
  via single-byte write (`compat.c:733-736`).
- Effect: file-list xfer-flags are written as varints
  (`xfer_flags_as_varint = 1`, `compat.c:748`); the same bit also
  gates the `negotiate_capabilities` vstring exchange.
- oc-rsync mapping: row `{ char: 'v' }`. The `'V'` recognition lives
  in `transfer::setup::capability::client_has_pre_release_v_flag` and
  `transfer::setup::compat::write_compat_flags`, which forces the bit
  and switches to the single-byte encoding when the client advertises
  `'V'`.

### CF_ID0_NAMES (`'u'`)

- Advertised unconditionally by upstream (`options.c:3034`).
- Server sets the bit when the client advertised `'u'`
  (`compat.c:727-728`); drives `xmit_id0_names = 1`
  (`compat.c:749`).
- Effect: the file list transmits the user/group names for uid 0 and
  gid 0 even when `--numeric-ids` is not in use.
- oc-rsync mapping: row `{ char: 'u' }`, always advertised.

## 3. CLI option to compat-flag table

The "compat flag" column lists the `CF_*` bit (or post-exchange
runtime variable) that the option controls. `-` means the option
does not affect the compat-flag bitfield - it is forwarded as a
separate single-letter flag in `build_flag_string` or as a
`--long-form` argument in `append_long_form_args`.

| CLI option              | Wire surface                       | Compat flag / variable touched      | Notes                                                                                                |
|-------------------------|------------------------------------|-------------------------------------|------------------------------------------------------------------------------------------------------|
| `--checksum` / `-c`     | `'c'` in single-letter flags       | -                                   | builder.rs:510-512. Independent of `compat_flags`; checksum negotiation rides on `'v'`.              |
| `--checksum-choice`     | `--checksum-choice=ALGO` long-form | -                                   | Drives the vstring exchange gated by `CF_VARINT_FLIST_FLAGS`, not a bit itself.                      |
| `--checksum-seed=N`     | `--checksum-seed=N` long-form      | influences `CF_CHKSUM_SEED_FIX` use | The bit is always advertised; this option only changes the seed value.                               |
| `--inplace`             | `--inplace` long-form              | `CF_INPLACE_PARTIAL_DIR` (consumer) | builder.rs:308. Bit is always advertised; receiver only honours it when `--partial-dir` set.         |
| `--partial`             | `'P'` in single-letter flags       | -                                   | builder.rs:556. Implies `--partial-dir=.~tmp~` only when `--partial-dir` is unset.                   |
| `--partial-dir=DIR`     | `--partial-dir=DIR` long-form      | `CF_INPLACE_PARTIAL_DIR` (consumer) | Sets `config.has_partial_dir`, which the post-exchange gate consumes.                                |
| `--whole-file` / `-W`   | `'W'` in single-letter flags       | -                                   | builder.rs:540. `--no-whole-file` is silent (matches upstream).                                      |
| `--append`              | `--append` long-form               | -                                   | builder.rs:312.                                                                                      |
| `--append-verify`       | `--append-verify` long-form        | -                                   | builder.rs:314.                                                                                      |
| `--hard-links` / `-H`   | `'H'` in single-letter flags       | -                                   | builder.rs:513.                                                                                      |
| `--acls` / `-A`         | `'A'` in single-letter flags       | -                                   | Gated on `feature = "acl"`. No dedicated `CF_*` flag exists; daemon refusal is via config.           |
| `--xattrs` / `-X`       | `'X'` in single-letter flags       | `CF_AVOID_XATTR_OPTIM` (consumer)   | Gated on `feature = "xattr"`. Absent bit downgrades `--xattrs` with a warning.                       |
| `--inc-recursive`       | `'i'` in `-e.<chars>`              | `CF_INC_RECURSE`                    | Sets `inc_recursive_send = true`; `allow_inc_recurse` predicate also requires `--recursive`.         |
| `--no-inc-recursive`    | `'i'` suppressed in `-e.<chars>`   | `CF_INC_RECURSE` cleared            | Mirrors `compat.c:177` server-side override.                                                         |
| `--iconv=CHARSET`       | `'s'` in `-e.<chars>`              | `CF_SYMLINK_ICONV`                  | Capability requires `feature = "iconv"`.                                                             |
| `--numeric-ids`         | `--numeric-ids` long-form          | -                                   | Independent of `CF_ID0_NAMES`; numeric IDs disable name lookup but the bit still advertises uid/gid 0 names. |
| `--delete[-…]`          | `--delete-before/during/after/delay` | `CF_INC_RECURSE` (gate)           | `delete_before/after`/`delay_updates` clear `allow_inc_recurse` on the receiver path.                |
| `--prune-empty-dirs`    | `'m'` in single-letter flags       | `CF_INC_RECURSE` (gate)             | Receiver-side option that clears `allow_inc_recurse` (`compat.c:175`).                               |
| `--qsort`               | (server-only)                      | `CF_INC_RECURSE` (gate)             | `compat.c:171`. Mirrored in `transfer/src/lib.rs:417`.                                               |
| `--protocol=N`          | first-line `@RSYNCD:`              | suppresses `compat_flags` exchange  | Below 30 the whole compat phase is skipped (`compat.c:710`), so no bits are exchanged.               |

The single-letter flag string is built in
`crates/core/src/client/remote/invocation/builder.rs::build_flag_string`
and the long-form arguments in `append_long_form_args` (same file,
`options.c::server_options` order). The capability string itself is
appended unconditionally on protocol >= 30 by
`build_capability_string(self.config.inc_recursive_send())` at
builder.rs:184-186 and by
`build_capability_string(config.inc_recursive_send())` in
`daemon_transfer/orchestration/arguments.rs:155`.

## 4. Current capability string

`build_capability_string` in
`crates/transfer/src/setup/capability.rs:138-153` walks
`CAPABILITY_MAPPINGS` and emits the characters in upstream order. With
all features compiled and `allow_inc_recurse = true` the resulting
string is:

```
-e.iLsfxCIvu
```

When the SSH client default applies (`inc_recursive_send = false`,
which is the post-#3744 default in
`crates/core/src/client/config/builder/mod.rs:381`) the `'i'` is
suppressed and oc-rsync emits:

```
-e.LsfxCIvu
```

This matches the string referenced in `AGENTS.md` and
`crates/core/src/client/remote/invocation/builder.rs:182-186`. On
Windows the `'L'` is omitted (`platform_ok = false`); without the
`iconv` cargo feature the `'s'` is omitted; the `'i'` is suppressed
whenever `inc_recursive_send` is `false`.

## 5. Gaps

The compat-flag pipeline is consistent for the bits we currently
support, but the propagation chain has the following gaps relative to
upstream rsync 3.4.1:

1. **`CF_INC_RECURSE` advertised only when `inc_recursive_send = true`.**
   `crates/core/src/client/config/builder/mod.rs:381` defaults
   `inc_recursive_send` to `false`, undoing PR #3557 in PR #3744 to
   work around the v0.6.1 push regression on incremental-recursion
   senders. Upstream defaults `allow_inc_recurse = 1`
   (`options.c:113`) and clears it only in
   `set_allow_inc_recurse`. Until the sender-side regression is
   resolved, oc-rsync will not advertise `'i'` on push transfers
   without the user passing `--inc-recursive`, which silently downgrades
   wire compatibility (no `CF_INC_RECURSE`, no incremental file list).
   Tracked: PR #3557 / PR #3744 history.

2. **`--qsort` does not exist on the CLI.** Upstream's
   `set_allow_inc_recurse` reads the global `use_qsort`
   (`compat.c:171`). oc-rsync threads `config.qsort` through
   `crates/transfer/src/lib.rs:417`, but nothing in
   `crates/cli/src/frontend/command_builder/sections/build_base_command`
   sets it. As a result the `qsort` arm of the gate is dead code from
   the CLI side - we never get the upstream behaviour where
   `--qsort` clears `CF_INC_RECURSE`.

3. **`CF_SYMLINK_ICONV` advertised on protocol exchange but no iconv
   transcoding pipeline.** The capability bit and `'s'` capability
   character are gated on `cfg!(feature = "iconv")`. The current build
   does not enable the feature in any default profile (no `iconv`
   feature in `crates/transfer/Cargo.toml` default-features), so
   `'s'` is never advertised and the bit is never set. `--iconv` is
   accepted by the CLI but the corresponding wire behaviour
   (`sender_symlink_iconv`, `compat.c:763-767`) is never engaged.
   Tracked separately: `docs/audits/iconv-parse-deadend.md`.

4. **No dedicated ACL compat flag.** Upstream has no `CF_ACL` bit;
   ACL refusal flows through the daemon's `refuse options` machinery.
   oc-rsync's pipeline mirrors this, but
   `crates/transfer/src/lib.rs:467-470` documents the absence rather
   than enforcing it. Clients pushing `--acls` to a daemon built
   without `feature = "acl"` see the warning from
   `clear_unsupported_features`, not the upstream "unknown option"
   refusal. Documentation/behaviour mismatch only - no compat-flag
   plumbing change is needed.

5. **`--checksum-seed=0` is not distinguished from "unset" on the
   wire.** `crates/transfer/src/setup/negotiator.rs:188-200`
   regenerates the seed when the value is `Some(0)` or `None`,
   matching upstream `options.c:835`. CLI parsing accepts `0` as a
   valid seed but stores it as `Some(0)`, which then triggers the
   regeneration path. Users passing `--checksum-seed=0` therefore get
   a fresh per-run seed rather than a literal zero seed. Upstream has
   the same quirk, so this is parity-preserving but worth noting.

6. **Pre-release `'V'` capability has no interop fixture.**
   `client_has_pre_release_v_flag` and the single-byte
   `write_compat_flags` path are exercised only by unit tests in
   `crates/transfer/src/setup/tests.rs`. There is no harness that
   validates the byte-encoded flag exchange against an actual
   pre-release client, because no such peer ships any longer. The
   risk is bounded but worth flagging if pre-release interop ever
   resurfaces.

7. **Unknown-bit propagation is preserved but unobservable.**
   `CompatibilityFlags::from_bits` retains bits outside
   `KNOWN_MASK`, but no diagnostic surfaces them. Future upstream
   additions will round-trip silently. A `--debug=compat` token
   could log `flags.unknown_bits()` when non-zero. Out of scope for
   this audit.

No regression in negotiation correctness was found: the `CF_*` bits we
advertise match upstream order, the post-exchange variables
(`CF_INPLACE_PARTIAL_DIR`, `CF_AVOID_XATTR_OPTIM`,
`CF_CHKSUM_SEED_FIX`, `CF_VARINT_FLIST_FLAGS`, `CF_ID0_NAMES`) are
consumed at the right call sites, and the pre-release `'V'` encoding
is honoured. The only material wire-compatibility shortfall is the
default-off `inc_recursive_send`, which the project tracks under the
sender-side INC_RECURSE work.

## Appendix: internal CLI-to-runtime pipeline (from prior audit)

The previously published version of this document focused on the
internal three-layer CLI -> `ClientConfig` -> `engine`/`transfer`
pipeline. The full matrix lives in commit history (PR #3802); the
findings still relevant to compat-flag propagation are:

- `--whole-file` is the only audited tri-state that stays
  `Option<bool>` all the way into `ClientConfig` so the runtime can
  distinguish "unset" from "explicit `--no-whole-file`". Compat-flag
  semantics use this signal indirectly when building the single-letter
  flag string (builder.rs:540 only emits `'W'` when the user passed
  the positive form).
- `--append` implies `--inplace`, which forces `config.inplace = true`
  and allows the receiver-side gate at `transfer/src/lib.rs:442-447`
  to interact correctly with `CF_INPLACE_PARTIAL_DIR` when
  `--partial-dir` is also set.
- `--partial-dir` implies `--partial`. The `has_partial_dir` field
  consumed by the `CF_INPLACE_PARTIAL_DIR` post-exchange gate is set
  through this implication rather than directly from `--partial`.
- `--size-only` and `--checksum` are mutually exclusive at config
  build time
  (`crates/engine/src/local_copy/options/builder/validation.rs:10-12`).
  Neither participates in the compat-flag bitfield directly, but
  `--checksum` does drive the single-letter `'c'` bit on the wire
  which interacts with the negotiated checksum algorithm exchange
  gated by `CF_VARINT_FLIST_FLAGS`.
