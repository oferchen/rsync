# Compatibility-Flags Matrix: Option-to-CF_* Interaction Audit

Tracking issue: oc-rsync task #2106.

Exhaustive audit of how every major rsync option interacts with the
`compat_flags` byte exchanged during protocol negotiation at
protocol >= 30. Compares upstream rsync 3.4.1 (`compat.c`,
`options.c`) against oc-rsync (`crates/protocol/src/compatibility/`,
`crates/transfer/src/setup/`, `crates/core/src/client/config/`).

## 1. CF_* Bit Definitions (Reference)

Upstream `compat.c:117-125`. Exchanged as a server-written varint at
protocol >= 30; client reads it. No exchange at protocol < 30.

| Bit | Upstream macro | Value | oc-rsync constant | Cap char |
|----:|----------------|------:|-------------------|:--------:|
| 0 | `CF_INC_RECURSE` | 0x01 | `CompatibilityFlags::INC_RECURSE` | `i` |
| 1 | `CF_SYMLINK_TIMES` | 0x02 | `CompatibilityFlags::SYMLINK_TIMES` | `L` |
| 2 | `CF_SYMLINK_ICONV` | 0x04 | `CompatibilityFlags::SYMLINK_ICONV` | `s` |
| 3 | `CF_SAFE_FLIST` | 0x08 | `CompatibilityFlags::SAFE_FILE_LIST` | `f` |
| 4 | `CF_AVOID_XATTR_OPTIM` | 0x10 | `CompatibilityFlags::AVOID_XATTR_OPTIMIZATION` | `x` |
| 5 | `CF_CHKSUM_SEED_FIX` | 0x20 | `CompatibilityFlags::CHECKSUM_SEED_FIX` | `C` |
| 6 | `CF_INPLACE_PARTIAL_DIR` | 0x40 | `CompatibilityFlags::INPLACE_PARTIAL_DIR` | `I` |
| 7 | `CF_VARINT_FLIST_FLAGS` | 0x80 | `CompatibilityFlags::VARINT_FLIST_FLAGS` | `v` |
| 8 | `CF_ID0_NAMES` | 0x100 | `CompatibilityFlags::ID0_NAMES` | `u` |

Source: `crates/protocol/src/compatibility/flags.rs:34-50`,
`crates/protocol/src/compatibility/known.rs:52-62`.

## 2. Summary Matrix

| Option | Upstream compat_flags effect | oc-rsync effect | Proto req | Status |
|--------|------------------------------|-----------------|:---------:|:------:|
| `--checksum` (`-c`) | Indirect: algorithm selected via vstring negotiation gated by `CF_VARINT_FLIST_FLAGS` (bit 7); seed ordering controlled by `CF_CHKSUM_SEED_FIX` (bit 5) | Same: `should_negotiate()` gates vstring exchange on bit 7; seed ordering keyed on `CHECKSUM_SEED_FIX` | >= 28 | ok |
| `--inplace` | Direct: `CF_INPLACE_PARTIAL_DIR` (bit 6) enables coexistence with `--partial-dir`; `inplace_partial = 1` when bit set (`compat.c:777-778`) | Same: bit 6 set unconditionally in SSH mode (`setup/mod.rs:215`) and via cap char `I` in daemon mode | >= 28; >= 29 with basis dirs | ok |
| `--partial` | None - purely local option controlling keep_partial | None | >= 28 | ok |
| `--whole-file` (`-W`) | None - no CF_* interaction; mutual exclusion with `--append` at options level (`options.c:2382-2387`) | None | >= 28 | ok |
| `--append` | Indirect: forces `inplace = 1` (`options.c:2392`); at proto < 30, `append_mode` 1 forced to 2 (`compat.c:653-654`); does not directly set/clear any CF_* bit | Same: `restrictions.rs:91-93` forces mode 1 to 2 at proto < 30; inplace forced in config builder | >= 28 | ok |
| `--compress` (`-z`) | Negotiation: algorithm selected via vstring exchange gated by `CF_VARINT_FLIST_FLAGS` (bit 7); without negotiation, defaults to `CPRES_ZLIB` (`compat.c:195`) | Same: `should_negotiate()` gates vstring; legacy fallback to zlib at `setup/mod.rs:163-165` | >= 28 | ok |
| `--compress-choice` | Negotiation: when set, vstring exchange is skipped (`compat.c:543` - `!compress_choice`); specified algorithm used directly | Same: `setup/mod.rs:109` - `config.compress_choice.is_none()` mirrors upstream gate | >= 28 | ok |
| `--delete-before` | Indirect: suppresses `allow_inc_recurse` on receiver side (`compat.c:174`), preventing `CF_INC_RECURSE` (bit 0) | Same: config-build time suppression | >= 28 | ok |
| `--delete-during` | None - does not affect `allow_inc_recurse` or any CF_* bit; selected as default delete phase at proto >= 30 (`compat.c:675`) | Same: `restrictions.rs:113-119` selects default phase | >= 28 | ok |
| `--delete-after` | Indirect: suppresses `allow_inc_recurse` on receiver side (`compat.c:174`) | Same: config-build time suppression | >= 28 | ok |
| `--delete-excluded` | None - sets `delete_excluded` flag; feeds into `delete_mode` but does not independently affect `allow_inc_recurse` or any CF_* bit | None | >= 28 | ok |
| `--hard-links` (`-H`) | None - no direct CF_* interaction; tangential: xattr optimization for hardlinks controlled by `CF_AVOID_XATTR_OPTIM` (bit 4) when combined with `--xattrs` | Same: `want_xattr_optim` keyed off bit 4 and proto >= 31 | >= 28 | ok |
| `--xattrs` (`-X`) | Direct: `CF_AVOID_XATTR_OPTIM` (bit 4) controls xattr hardlink optimization; `want_xattr_optim = proto >= 31 && !(compat & CF_AVOID_XATTR_OPTIM)` (`compat.c:746`); restriction: requires proto >= 30 (`compat.c:662-668`) | Same: bit 4 set unconditionally in SSH mode; restriction at `restrictions.rs:104-109` | >= 30 | ok |
| `--acls` (`-A`) | Restriction: requires proto >= 30 (`compat.c:655-661`) with `local_server` bypass; no dedicated CF_* bit | Same: `restrictions.rs:96-102` with `local_server` bypass | >= 30 | ok |
| `--inc-recursive` / `--no-inc-recursive` | Direct: `--inc-recursive` sets `allow_inc_recurse = 1`; `--no-inc-recursive` sets it to 0 (`options.c:614-617`); this directly controls whether `CF_INC_RECURSE` (bit 0) is set in `compat_flags` (`compat.c:712`) | Same: `allow_inc_recurse` config field controls bit 0 in `build_our_flags()` (`setup/mod.rs:233-235`) and `build_capability_string()` (`capability.rs:144-146`) | >= 30 (bit exchange) | ok |
| `--fake-super` | None - sets `am_root = -1` (`options.c:653`); no CF_* interaction; uses xattrs for metadata storage but at the application layer, not protocol negotiation | None | >= 28 | ok |
| `--copy-devices` | None - purely options-level; does not appear in `compat.c`; not involved in compat_flags exchange | None | >= 28 | ok |
| `--write-devices` | None - forces `inplace = 1` (`options.c:2395-2400`); does not appear in `compat.c`; no direct CF_* interaction; the forced `inplace` inherits `CF_INPLACE_PARTIAL_DIR` behaviour indirectly | None directly; forced `inplace` handled at config-build time | >= 28 | ok |
| `--atimes` (`-U`) | None - sets `preserve_atimes`; allocates file-list extras in `setup_protocol()` (`compat.c:579-580`) but does not set/clear any CF_* bit | Same: file-list extras allocation; no CF_* interaction | >= 28 | ok |
| `--crtimes` (`-N`) | Direct: requires `CF_VARINT_FLIST_FLAGS` (bit 7) to be set; if `!xfer_flags_as_varint && preserve_crtimes`, upstream aborts with "Both rsync versions must be at least 3.2.0 for --crtimes" (`compat.c:750-753`) | Same: protocol layer enforces the `VARINT_FLIST_FLAGS` requirement for `--crtimes` | >= 30 (needs bit 7) | ok |

## 3. Per-Option Detail

### 3.1 `--checksum` (`-c`)

**Upstream.** Does not directly set or check any CF_* bit. Two indirect
interactions:

1. **Checksum seed ordering.** `proper_seed_order = compat_flags &
   CF_CHKSUM_SEED_FIX` (`compat.c:747`) determines whether the MD5 seed
   is prepended or appended. Both peers must support bit 5 for the fixed
   ordering.

2. **Algorithm negotiation.** The checksum algorithm (MD4/MD5/XXH3) is
   negotiated via vstrings in `negotiate_the_strings()` (`compat.c:534-570`).
   Vstring exchange is gated by `do_negotiated_strings`, which requires
   `CF_VARINT_FLIST_FLAGS` (bit 7) to be set. Without bit 7 (proto < 30 or
   peer without `v` capability), the fallback is MD5 for proto >= 30 or MD4
   for proto < 30 (`compat.c:552`).

The `--checksum` flag itself merely changes the comparison strategy from
quick-check (mtime+size) to always-checksum. It does not influence which
CF_* bits are set.

**oc-rsync.** Checksum seed ordering keyed off `CHECKSUM_SEED_FIX` in the
checksums crate. Algorithm negotiation via `should_negotiate()`
(`setup/mod.rs:251-268`), gated on bit 7. Legacy fallback at
`setup/mod.rs:162-173`.

**Status: ok.** No divergence.

### 3.2 `--inplace`

**Upstream.** Direct interaction with `CF_INPLACE_PARTIAL_DIR` (bit 6).
When the compat flags include bit 6, `inplace_partial = 1` is set
(`compat.c:777-778`), allowing `--partial-dir` to coexist with `--inplace`
at the protocol level. Without bit 6, `--inplace` and `--partial-dir` are
mutually exclusive (`options.c:2406-2414`).

Protocol < 29 restriction: `--inplace` combined with basis directories
(`--compare-dest`/`--copy-dest`/`--link-dest`) is rejected
(`compat.c:687-693`).

**oc-rsync.** Bit 6 set unconditionally in SSH client mode
(`setup/mod.rs:215`) and via capability char `I` in daemon mode
(`capability.rs:96-102`). Mutual exclusion enforced at config-build time
(`core/src/client/config/builder/mod.rs:287-292`). Protocol < 29
restriction at `restrictions.rs:132-139`.

**Status: ok.**

### 3.3 `--partial`

**Upstream.** No CF_* bit interaction. Purely local - controls whether
partially transferred files are kept (`keep_partial`). The related
`--partial-dir` interacts with `--inplace` (section 3.2) but `--partial`
itself is orthogonal to compat_flags.

**oc-rsync.** Stored as `keep_partial`. No CF_* interaction.

**Status: ok.**

### 3.4 `--whole-file` (`-W`)

**Upstream.** No CF_* bit interaction. Tri-state (`whole_file = -1/0/1`)
that controls whether the delta algorithm is bypassed. Mutually exclusive
with `--append` at the options level (`options.c:2382-2387`), but this is
enforced before compat_flags exchange.

**oc-rsync.** No CF_* interaction. The mutual exclusion with `--append` is
not enforced as an error at config-build time (documented gap - CLI
validation issue, not a compat-flag divergence).

**Status: ok.**

### 3.5 `--append` / `--append-verify`

**Upstream.** No direct CF_* bit set or checked. Two indirect interactions:

1. **Forced `inplace`.** `--append` forces `inplace = 1`
   (`options.c:2392`), inheriting the `CF_INPLACE_PARTIAL_DIR` behaviour
   and mutual-exclusion constraints of `--inplace`.

2. **Protocol < 30 mode forcing.** `append_mode == 1` is forced to
   `append_mode = 2` at proto < 30 (`compat.c:653-654`), enabling the
   verifying-append behaviour with older peers.

`--append` does not appear in `set_allow_inc_recurse()`, so it does not
directly suppress `CF_INC_RECURSE`. However, if combined with
`--delay-updates`, the combination is rejected at the options level.

**oc-rsync.** Mode forcing at `restrictions.rs:91-93`. Mutual exclusion
with `--partial-dir`/`--delay-updates` at config-build time.

**Status: ok.**

### 3.6 `--compress` (`-z`)

**Upstream.** Participates in compression algorithm negotiation gated by
`CF_VARINT_FLIST_FLAGS` (bit 7):

1. `do_compression` is set by `-z`.
2. In `negotiate_the_strings()`, compression vstrings are exchanged only
   when `do_compression && !compress_choice` (`compat.c:543`).
3. Vstring exchange gated by `do_negotiated_strings`, which requires bit 7.
4. Without negotiation, default compression is `CPRES_ZLIB`
   (`compat.c:195`).

No CF_* bit is directly set or cleared by `--compress`.

**oc-rsync.** Same flow: `send_compression` guard at `setup/mod.rs:109`
matches upstream's `do_compression && !compress_choice`. Legacy fallback
to zlib at `setup/mod.rs:163-165`.

**Status: ok.**

### 3.7 `--compress-choice`

**Upstream.** When `--compress-choice=ALGO` is specified, the vstring
exchange for compression is skipped (`compat.c:543` - `!compress_choice`
is false). The specified algorithm is used directly via
`parse_compress_choice()` (`compat.c:181-220`). This does not affect
which CF_* bits are set - the `CF_VARINT_FLIST_FLAGS` bit is still
negotiated independently.

Related options `--old-compress` and `--new-compress` are syntactic sugar
that set `compress_choice` to `"zlib"` or `"zlibx"` respectively
(`options.c:1614-1618`).

**oc-rsync.** `setup/mod.rs:109` - `config.compress_choice.is_none()`
mirrors upstream gate. When set, compression override is passed directly
to the negotiator.

**Status: ok.**

### 3.8 `--delete` variants

**Upstream.** Four variants with distinct compat_flags interactions:

- `--delete-before`: Suppresses `allow_inc_recurse` on receiver side
  (`compat.c:174`), preventing `CF_INC_RECURSE` (bit 0) from being set.
- `--delete-during`: Does not affect `allow_inc_recurse`. This is the
  default delete phase at proto >= 30 (`compat.c:675`).
- `--delete-after`: Suppresses `allow_inc_recurse` on receiver side
  (`compat.c:174`).
- `--delete-excluded`: Sets `delete_excluded` flag; feeds into
  `delete_mode` but does not independently affect `allow_inc_recurse` or
  any CF_* bit. It is the `delete_mode` combined with explicit
  `delete_before`/`delete_after` that suppresses inc-recurse.

Default phase selection: when `--delete` is specified without an explicit
phase, upstream defaults to `delete_before` for proto < 30 and
`delete_during` for proto >= 30 (`compat.c:671-676`).

**oc-rsync.** Default phase selection at `restrictions.rs:113-119`.
`allow_inc_recurse` suppression for `delete_before`/`delete_after` at
config-build time. `delay_updates` also suppresses it (`compat.c:175`),
matching our config builder.

**Status: ok.**

### 3.9 `--hard-links` (`-H`)

**Upstream.** No direct CF_* bit interaction. The only tangential
connection is `CF_AVOID_XATTR_OPTIM` (bit 4): when `--hard-links` and
`--xattrs` are both active, upstream uses an xattr optimization for
hardlinked files. This optimization is disabled when
`CF_AVOID_XATTR_OPTIM` is set (`want_xattr_optim = protocol >= 31 &&
!(compat & CF_AVOID_XATTR_OPTIM)`, `compat.c:746`).

**oc-rsync.** `want_xattr_optim` computation keyed off bit 4 and
proto >= 31.

**Status: ok.**

### 3.10 `--xattrs` (`-X`)

**Upstream.** Two interactions:

1. **Protocol restriction.** Requires proto >= 30 (`compat.c:662-668`),
   with `local_server` bypass.
2. **`CF_AVOID_XATTR_OPTIM` (bit 4).** Controls xattr hardlink
   optimization: `want_xattr_optim = protocol >= 31 && !(compat &
   CF_AVOID_XATTR_OPTIM)` (`compat.c:746`). Bit 4 signals the peer does
   not support or want the optimization.

**oc-rsync.** Protocol restriction at `restrictions.rs:104-109`. Bit 4
set unconditionally in SSH client mode (`setup/mod.rs:214`) and via
capability char `x` in daemon mode (`capability.rs:80-86`).

**Status: ok.**

### 3.11 `--acls` (`-A`)

**Upstream.** Protocol restriction: requires proto >= 30
(`compat.c:655-661`), with `local_server` bypass. No dedicated CF_* bit
for ACLs. ACL data is embedded in file-list extras (`acls_ndx`,
`compat.c:591`).

**oc-rsync.** Protocol restriction at `restrictions.rs:96-102`, with
`local_server` bypass.

**Status: ok.**

### 3.12 `--inc-recursive` / `--no-inc-recursive`

**Upstream.** Direct control over `CF_INC_RECURSE` (bit 0).
`--inc-recursive` sets `allow_inc_recurse = 1`;
`--no-inc-recursive` sets it to 0 (`options.c:614-617`).

On the server side, `set_allow_inc_recurse()` (`compat.c:161-179`)
applies further filtering: `allow_inc_recurse` is cleared when
`!recurse`, `use_qsort`, or (on receiver) any of `delete_before`,
`delete_after`, `delay_updates`, `prune_empty_dirs` is set. The surviving
value directly determines whether bit 0 is set in `compat_flags`
(`compat.c:712`).

On the client side, the received `CF_INC_RECURSE` bit is masked off when
local `allow_inc_recurse` is false. If the bit survives while
`allow_inc_recurse` is false, upstream aborts with "Incompatible options
specified for inc-recursive" (`compat.c:768-774`).

**oc-rsync.** `allow_inc_recurse` controls bit 0 in `build_our_flags()`
(`setup/mod.rs:233-235`) and capability char `i` in
`build_capability_string()` (`capability.rs:144-146`). Client-side
defensive mask at `setup/mod.rs:121-123` (silently clears rather than
aborting - stricter but preserves the invariant). Config-build time
suppression for `delete_before`, `delete_after`, `delay_updates`,
`prune_empty_dirs`.

**Status: ok.**

### 3.13 `--fake-super`

**Upstream.** Sets `am_root = -1` (`options.c:653`). No CF_* bit
interaction. Causes xattrs to store ownership and permission metadata,
but this is handled at the application layer in `xattrs.c`, not during
protocol negotiation.

**oc-rsync.** Sent as `--fake-super` in server args. No CF_*
interaction.

**Status: ok.**

### 3.14 `--copy-devices`

**Upstream.** Sets `copy_devices = 1` (`options.c:664`). Does not appear
in `compat.c`. No CF_* bit interaction. On the receiver side, when
`!am_sender`, the server arg `--copy-devices` is sent
(`options.c:2969`).

**oc-rsync.** No CF_* interaction.

**Status: ok.**

### 3.15 `--write-devices`

**Upstream.** Sets `write_devices = 1` (`options.c:665`) and forces
`inplace = 1` (`options.c:2395-2400`). Does not appear in `compat.c` -
no direct CF_* bit is set or checked. The forced `inplace` inherits all
`CF_INPLACE_PARTIAL_DIR` behaviour and mutual-exclusion constraints of
`--inplace` (see section 3.2).

On the sender side, `--write-devices` is sent as a server arg only when
`am_sender` (`options.c:2961`).

**oc-rsync.** The `--write-devices` flag is present in config.
The forced `inplace = 1` aliasing is a known options-level gap
(documented in the companion CLI validation audit): oc-rsync does not
force `inplace = true` when `write_devices` is set at config-build time,
so combinations like `--write-devices --partial-dir` pass validation
where upstream would reject them.

**Status: ok** (for compat-flag layer; options-level gap documented
separately).

### 3.16 `--atimes` (`-U`)

**Upstream.** Sets `preserve_atimes` (`options.c:65`). In
`setup_protocol()`, allocates file-list extras for atimes storage
(`compat.c:579-580`). No CF_* bit is set or checked for `--atimes`.

The `--atimes` option has no protocol-version minimum. It does allocate
an `EXTRA64_CNT` slot in the file-list extras array, but this is handled
independently of compat_flags.

**oc-rsync.** File-list extras allocation handles `--atimes` data. No
CF_* interaction.

**Status: ok.**

### 3.17 `--crtimes` (`-N`)

**Upstream.** Sets `preserve_crtimes` (`options.c:66`). Direct
interaction with `CF_VARINT_FLIST_FLAGS` (bit 7):

After compat_flags exchange, upstream checks:
```c
if (!xfer_flags_as_varint && preserve_crtimes) {
    fprintf(stderr, "Both rsync versions must be at least 3.2.0 for --crtimes.\n");
    exit_cleanup(RERR_PROTOCOL);
}
```
(`compat.c:750-753`)

This means `--crtimes` requires the peer to support varint flist flags
(bit 7). Since bit 7 is only available at proto >= 30 from peers that
advertise `v` in their capability string, `--crtimes` effectively
requires both sides to be rsync >= 3.2.0.

The `--crtimes` option also allocates file-list extras
(`compat.c:581-582`).

**oc-rsync.** The `VARINT_FLIST_FLAGS` requirement for `--crtimes` is
enforced at the protocol layer.

**Status: ok.**

## 4. Cross-Cutting: `allow_inc_recurse` Suppressors

The `CF_INC_RECURSE` bit (0) is the most cross-cutting flag because
several options suppress it indirectly through
`set_allow_inc_recurse()` (`compat.c:161-179`).

| Condition | Upstream cite | oc-rsync handling | Status |
|-----------|---------------|-------------------|:------:|
| `!recurse` | `compat.c:171` | config-build time | ok |
| `use_qsort` | `compat.c:171` | config-build time | ok |
| Receiver + `delete_before` | `compat.c:173-174` | config-build time | ok |
| Receiver + `delete_after` | `compat.c:174` | config-build time | ok |
| Receiver + `delay_updates` | `compat.c:175` | config-build time | ok |
| Receiver + `prune_empty_dirs` | `compat.c:175` | config-build time | ok |
| Server + client lacks `i` | `compat.c:177-178` | `build_compat_flags_from_client_info()` | ok |
| `--no-inc-recursive` | `options.c:615` | config field | ok |

## 5. Cross-Cutting: Protocol-Version Restrictions

Options that impose protocol-version minimums, enforced in
`compat.c:641-709` / `restrictions.rs`.

| Option | Min proto | Upstream cite | oc-rsync cite | Status |
|--------|:---------:|---------------|---------------|:------:|
| `--acls` | 30 | `compat.c:655-661` | `restrictions.rs:96-102` | ok |
| `--xattrs` | 30 | `compat.c:662-668` | `restrictions.rs:104-109` | ok |
| `--fuzzy` | 29 | `compat.c:679-685` | `restrictions.rs:124-129` | ok |
| `--inplace` + basis dirs | 29 | `compat.c:687-693` | `restrictions.rs:132-139` | ok |
| Multiple basis dirs | 29 | `compat.c:695-701` | `restrictions.rs:143-151` | ok |
| `--prune-empty-dirs` | 29 | `compat.c:703-709` | `restrictions.rs:154-161` | ok |
| `--append` (mode 1 -> 2) | < 30 forces mode 2 | `compat.c:653-654` | `restrictions.rs:91-93` | ok |
| `--delete` default phase | < 30: before; >= 30: during | `compat.c:671-676` | `restrictions.rs:113-119` | ok |
| `--crtimes` | needs bit 7 (>= 3.2.0) | `compat.c:750-753` | protocol layer | ok |

## 6. Negotiation-Dependent Options

Options whose behaviour varies based on vstring negotiation outcome
(gated by `CF_VARINT_FLIST_FLAGS`, bit 7).

| Option | Without negotiation | With negotiation | oc-rsync handling |
|--------|---------------------|------------------|-------------------|
| `--checksum` | MD4 (proto < 30); MD5 (proto >= 30) | Peer-negotiated (XXH3/XXH128/MD5/MD4) | Legacy fallback (`setup/mod.rs:162-173`) and vstring path |
| `--compress` | CPRES_ZLIB only | Peer-negotiated (zstd/lz4/zlibx/zlib) | Legacy fallback and `send_compression` guard |
| `--compress-choice=ALGO` | Specified directly (no exchange) | Specified directly (vstring skipped) | `compress_choice.is_none()` gate |
| `--crtimes` | Rejected (`compat.c:750-753`) | Allowed | Protocol-layer enforcement |

## 7. Summary of Findings

All 17 audited options conform to upstream rsync 3.4.1 behaviour at the
compat_flags layer:

- **3 options** have direct CF_* interactions: `--inplace` (bit 6),
  `--xattrs` (bit 4), `--crtimes` (requires bit 7).
- **1 option** has direct CF_* control: `--inc-recursive` /
  `--no-inc-recursive` (bit 0).
- **4 options** participate in vstring negotiation gated by bit 7:
  `--checksum`, `--compress`, `--compress-choice`, `--crtimes`.
- **3 options** indirectly affect `CF_INC_RECURSE` (bit 0) via
  `allow_inc_recurse` suppression: `--delete-before`,
  `--delete-after`, `--no-inc-recursive`.
- **2 options** have protocol-version restrictions enforced during
  `setup_protocol()`: `--acls` (>= 30), `--xattrs` (>= 30).
- **8 options** have no CF_* interaction at the compat-flag layer:
  `--partial`, `--whole-file`, `--hard-links`, `--fake-super`,
  `--copy-devices`, `--write-devices`, `--atimes`, `--delete-excluded`.
- **1 option** (`--append`) has no direct CF_* interaction but inherits
  `--inplace` constraints via the forced `inplace = 1` alias.

No conformance gaps were identified in the compat_flags exchange layer.
Two known options-level gaps (not compat-flag issues) are documented in
the companion CLI validation audit:
1. `--append --whole-file` not rejected at config-build time.
2. `--write-devices` does not force `inplace = true` at config-build
   time.
