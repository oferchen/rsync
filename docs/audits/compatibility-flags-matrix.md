# Compatibility-Flags Matrix: CLI Options vs CF_* Bits (#2106)

Conformance audit mapping 14 CLI options to their protocol compatibility
flag (`CF_*`) interactions, protocol version requirements, and
mutual-exclusion constraints. Compares upstream rsync 3.4.1
(`target/interop/upstream-src/rsync-3.4.1/compat.c`, `options.c`) against
oc-rsync (`crates/protocol/src/compatibility/`, `crates/transfer/src/setup/`,
`crates/core/src/client/`).

## 1. Background

Upstream rsync defines 9 `CF_*` compatibility bits exchanged as a varint
(server writes, client reads) at protocol >= 30. These bits are independent
of individual CLI options - they advertise *peer capabilities*. However,
several CLI options change behaviour depending on which `CF_*` bits are
negotiated, require a minimum protocol version, or interact with the
`allow_inc_recurse` guard that gates `CF_INC_RECURSE`.

This audit traces each option through three layers:

1. **CLI option parsing** - `options.c` / `crates/cli/`, `crates/core/src/client/config/`
2. **Protocol restrictions** - `compat.c:641-709` / `crates/transfer/src/setup/restrictions.rs`
3. **Compat-flag interactions** - `compat.c:710-783` / `crates/transfer/src/setup/mod.rs`

## 2. CF_* Bit Definitions (Reference)

All bits defined in upstream `compat.c:117-125`. Exchanged only at
protocol >= 30.

| Bit | Upstream macro | oc-rsync constant | Cap char |
|----:|----------------|-------------------|:--------:|
| 0 | `CF_INC_RECURSE` | `CompatibilityFlags::INC_RECURSE` | `i` |
| 1 | `CF_SYMLINK_TIMES` | `CompatibilityFlags::SYMLINK_TIMES` | `L` |
| 2 | `CF_SYMLINK_ICONV` | `CompatibilityFlags::SYMLINK_ICONV` | `s` |
| 3 | `CF_SAFE_FLIST` | `CompatibilityFlags::SAFE_FILE_LIST` | `f` |
| 4 | `CF_AVOID_XATTR_OPTIM` | `CompatibilityFlags::AVOID_XATTR_OPTIMIZATION` | `x` |
| 5 | `CF_CHKSUM_SEED_FIX` | `CompatibilityFlags::CHECKSUM_SEED_FIX` | `C` |
| 6 | `CF_INPLACE_PARTIAL_DIR` | `CompatibilityFlags::INPLACE_PARTIAL_DIR` | `I` |
| 7 | `CF_VARINT_FLIST_FLAGS` | `CompatibilityFlags::VARINT_FLIST_FLAGS` | `v` |
| 8 | `CF_ID0_NAMES` | `CompatibilityFlags::ID0_NAMES` | `u` |

Source: `crates/protocol/src/compatibility/flags.rs:34-50`,
`crates/protocol/src/compatibility/known.rs:52-62`.

## 3. Option-to-Compat-Flag Interaction Matrix

### Legend

- **Direct** - The option directly gates or is gated by a `CF_*` bit.
- **Indirect** - The option affects `allow_inc_recurse`, which gates `CF_INC_RECURSE`.
- **Restriction** - The option triggers a protocol-version minimum enforced
  in `compat.c:641-709` / `restrictions.rs`.
- **Negotiation** - The option participates in vstring negotiation gated by
  `CF_VARINT_FLIST_FLAGS`.
- **None** - The option has no interaction with any `CF_*` bit.

| CLI option | CF_* interaction | Proto min | Mutual exclusions | Conformance |
|------------|-----------------|:---------:|-------------------|:-----------:|
| `--checksum` | Direct: `CF_CHKSUM_SEED_FIX` (bit 5) | 28 | none with CF_* bits | MATCH |
| `--inplace` | Direct: `CF_INPLACE_PARTIAL_DIR` (bit 6) | 29 (with basis dirs) | `--partial-dir`, `--delay-updates` | MATCH |
| `--partial` | None | 28 | none | MATCH |
| `--whole-file` | None | 28 | `--append` (options-level) | MATCH |
| `--append` | Indirect: disables `allow_inc_recurse` | 28 (forced mode 2 at <30) | `--whole-file`, `--partial-dir`, `--delay-updates` | MATCH |
| `--compress` | Negotiation: via `CF_VARINT_FLIST_FLAGS` (bit 7) | 28 | none | MATCH |
| `--delete` | Indirect: `--delete-before`/`--delete-after` disable `allow_inc_recurse` | 28 | none with CF_* bits | MATCH |
| `--hard-links` | None | 28 | none | MATCH |
| `--acls` | Restriction: requires proto >= 30 | 30 | none with CF_* bits | MATCH |
| `--xattrs` | Direct: `CF_AVOID_XATTR_OPTIM` (bit 4) + Restriction: proto >= 30 | 30 | none | MATCH |
| `--fake-super` | None | 28 | none | MATCH |
| `--numeric-ids` | None | 28 | none | MATCH |
| `--relative` | None | 28 | none | MATCH |
| `--files-from` | Indirect: via iconv + `CF_SYMLINK_ICONV` (bit 2) | 28 | none with CF_* bits | MATCH |

## 4. Per-Option Detail

### 4.1 `--checksum` (`-c`)

**Upstream.** The `--checksum` flag does not set or check any `CF_*` bit
directly. It changes the file-comparison strategy from quick-check
(mtime+size) to always-checksum. The *checksum algorithm* used for
file-level comparison is determined by `CF_CHKSUM_SEED_FIX` (bit 5)
and the vstring negotiation gated by `CF_VARINT_FLIST_FLAGS` (bit 7):

- `proper_seed_order = compat_flags & CF_CHKSUM_SEED_FIX` (`compat.c:747`)
  controls whether the MD5 seed is prepended or appended to the data stream.
  When both peers support `CF_CHKSUM_SEED_FIX`, the post-3.0.0 fixed
  ordering is used.
- The checksum algorithm itself (MD4/MD5/XXH3) is selected through vstring
  negotiation (`negotiate_the_strings`, `compat.c:534-570`), gated by
  `do_negotiated_strings` which requires `CF_VARINT_FLIST_FLAGS`.

**oc-rsync.** The `--checksum` flag is stored as `config.checksum()` and
adds the `c` flag to the server argument string
(`core/src/client/remote/flags.rs:67`). Checksum seed ordering is keyed off
`CompatibilityFlags::CHECKSUM_SEED_FIX` in the checksums crate. Algorithm
negotiation follows the same `CF_VARINT_FLIST_FLAGS` gate via
`should_negotiate()` (`transfer/src/setup/mod.rs:251-268`).

**Conformance: MATCH.** No divergence.

### 4.2 `--inplace`

**Upstream.** The `--inplace` flag interacts with `CF_INPLACE_PARTIAL_DIR`
(bit 6). When the compat flags include `CF_INPLACE_PARTIAL_DIR`
(`compat.c:777-778`), `inplace_partial = 1` is set, allowing `--partial-dir`
to coexist with `--inplace` at the protocol level. Without this bit,
`--inplace` and `--partial-dir` are mutually exclusive
(`options.c:2406-2414`).

At protocol < 29, `--inplace` combined with basis directories
(`--compare-dest`/`--copy-dest`/`--link-dest`) is rejected
(`compat.c:687-693`).

**oc-rsync.** The `CF_INPLACE_PARTIAL_DIR` bit is set unconditionally in
SSH client mode (`setup/mod.rs:215`) and via capability char `I` in daemon
mode (`capability.rs:96-102`). Mutual exclusion of `--inplace` vs
`--partial-dir` / `--delay-updates` is enforced at config-build time
(`core/src/client/config/builder/mod.rs:287-292`). Protocol < 29
restriction is in `restrictions.rs:132-139`.

**Conformance: MATCH.** The `CF_INPLACE_PARTIAL_DIR` handling and
protocol-version restriction align with upstream.

### 4.3 `--partial`

**Upstream.** The `--partial` flag (`keep_partial`) has no `CF_*` bit
interaction. It is a purely local option controlling whether partially
transferred files are kept. The only protocol-level involvement is through
`--partial-dir`, which is a filter rule and interacts with `--inplace` (see
section 4.2). `--partial` itself does not require any minimum protocol
version and does not affect compat flags.

**oc-rsync.** `--partial` is stored as `keep_partial` in config. No
compat-flag interaction.

**Conformance: MATCH.**

### 4.4 `--whole-file` (`-W`)

**Upstream.** The `--whole-file` flag does not interact with any `CF_*` bit.
It is a tri-state (`whole_file = -1/0/1`) that controls whether the delta
algorithm is bypassed. The only mutual exclusion is with `--append`
(`options.c:2382-2387`): `--append` rejects `whole_file > 0`. It is sent
to the server as the `W` flag in the short-option string.

**oc-rsync.** Stored as `config.whole_file()`. The `W` flag is suppressed
when `append` is active (`core/src/client/remote/flags.rs:96-98`). The
mutual exclusion is not enforced as an error at config-build time (gap
documented in `compat-flags-matrix.md` section 4, gap 1).

**Conformance: MATCH** for compat-flag interaction (none). The mutual
exclusion gap with `--append` is a CLI validation issue, not a
compat-flag divergence.

### 4.5 `--append` / `--append-verify`

**Upstream.** The `--append` flag has an indirect interaction with
`CF_INC_RECURSE` (bit 0). In `set_allow_inc_recurse()` (`compat.c:161-179`),
`delete_before`, `delete_after`, `delay_updates`, and `prune_empty_dirs`
disable `allow_inc_recurse` on the receiver side. While `--append` itself is
not in that list, `--append` forces `inplace = 1` (`options.c:2392`), and
if `--append` is combined with `--delay-updates`, the result is rejected
(`options.c:2406-2414`) before reaching compat-flag exchange.

Protocol restriction: at protocol < 30, `append_mode == 1` is forced to
`append_mode = 2` (`compat.c:653`), meaning the verifying-append behaviour
is always used with older peers.

**oc-rsync.** `append_mode` forcing at protocol < 30 is in
`restrictions.rs:91-93`. Mutual exclusion with `--partial-dir` /
`--delay-updates` is enforced at config-build time.

**Conformance: MATCH.**

### 4.6 `--compress` (`-z`)

**Upstream.** The `--compress` flag participates in compression algorithm
negotiation gated by `CF_VARINT_FLIST_FLAGS` (bit 7). The flow:

1. `do_compression` is set by `-z` / `--compress` / `--compress-choice`.
2. In `negotiate_the_strings()` (`compat.c:534-564`), compression vstrings
   are exchanged only when `do_compression && !compress_choice`
   (`compat.c:543`).
3. The vstring exchange itself is gated by `do_negotiated_strings`, which
   requires `CF_VARINT_FLIST_FLAGS` (`compat.c:530-531`).
4. Without negotiation (protocol < 30 or peer without `v`), the default
   compression is `CPRES_ZLIB` (`compat.c:195`).

**oc-rsync.** Compression negotiation follows the same path:
`ProtocolSetupConfig::do_compression` and `compress_choice` control
vstring exchange. The `send_compression` guard at `setup/mod.rs:109`
matches upstream's `do_compression && !compress_choice`. Legacy fallback
to zlib at `setup/mod.rs:163-165` matches upstream's default.

**Conformance: MATCH.**

### 4.7 `--delete`

**Upstream.** The `--delete` family has two interactions:

1. **Indirect `CF_INC_RECURSE` interaction.** On the receiver side,
   `delete_before` or `delete_after` disables `allow_inc_recurse`
   (`compat.c:173-176`). `delete_during` does *not* disable it.
2. **Default phase selection.** When `--delete` is specified without an
   explicit phase (`--delete-before`, `--delete-during`, `--delete-after`),
   the default phase depends on protocol version: `delete_before` for
   protocol < 30, `delete_during` for protocol >= 30 (`compat.c:671-676`).

No `CF_*` bit is directly set or checked by the delete flags.

**oc-rsync.** Default phase selection is in `restrictions.rs:113-119`.
The `allow_inc_recurse` suppression for `delete_before`/`delete_after` is
handled at config-build time in `core/src/client/config/builder/`.

**Conformance: MATCH.**

### 4.8 `--hard-links` (`-H`)

**Upstream.** The `--hard-links` flag does not interact with any `CF_*`
bit. It is sent as the `H` flag in the server args. Hard-link detection
and preservation is handled entirely within the file-list and generator
phases, with no protocol-version minimum and no compat-flag dependency.

The only tangential connection is `CF_AVOID_XATTR_OPTIM` (bit 4): when
`--hard-links` and `--xattrs` are both active, upstream uses an xattr
optimization for hardlinked files that is disabled when `CF_AVOID_XATTR_OPTIM`
is set (`want_xattr_optim = protocol >= 31 && !(compat & CF_AVOID_XATTR_OPTIM)`,
`compat.c:746`).

**oc-rsync.** The `want_xattr_optim` computation is at
`transfer/src/receiver/transfer/pipeline.rs:89-91` and
`transfer/src/receiver/wire.rs:293-296`, keyed off
`CompatibilityFlags::AVOID_XATTR_OPTIMIZATION` and protocol >= 31.

**Conformance: MATCH.**

### 4.9 `--acls` (`-A`)

**Upstream.** The `--acls` flag has a protocol restriction at
`compat.c:655-661`: it requires protocol >= 30 unless `local_server` is
true. There is no dedicated `CF_*` bit for ACLs. The ACL data is embedded
in the file-list extras (`acls_ndx`, `compat.c:591`) and handled by
`lib/sysacls.c` / `acls.c`.

**oc-rsync.** Protocol restriction at `restrictions.rs:96-102` mirrors
upstream exactly, including the `local_server` bypass.

**Conformance: MATCH.**

### 4.10 `--xattrs` (`-X`)

**Upstream.** Two interactions:

1. **Protocol restriction.** `--xattrs` requires protocol >= 30
   (`compat.c:662-668`), with `local_server` bypass.
2. **`CF_AVOID_XATTR_OPTIM` (bit 4).** Controls the xattr hardlink
   optimization: `want_xattr_optim = protocol >= 31 && !(compat &
   CF_AVOID_XATTR_OPTIM)` (`compat.c:746`). When a peer sets this bit,
   it signals that the optimization should be *avoided* (the peer does not
   support or want it).

**oc-rsync.** Protocol restriction at `restrictions.rs:104-109`. The
`CF_AVOID_XATTR_OPTIM` bit is set unconditionally in SSH client mode
(`setup/mod.rs:214`) and via capability char `x` in daemon mode
(`capability.rs:80-86`). Consumer logic at
`transfer/src/receiver/wire.rs:293-296`.

**Conformance: MATCH.**

### 4.11 `--fake-super`

**Upstream.** `--fake-super` has no `CF_*` bit interaction and no protocol
version restriction. It is sent as the `--fake-super` long-form arg to the
server. When active, it causes xattrs to be used for storing ownership and
permission metadata, but this is handled at the metadata layer, not the
compat-flag layer.

**oc-rsync.** Sent as `--fake-super` in server args
(`core/src/client/remote/invocation/builder.rs:353-354`). No compat-flag
interaction.

**Conformance: MATCH.**

### 4.12 `--numeric-ids`

**Upstream.** `--numeric-ids` has no `CF_*` bit interaction. It is sent as
the `--numeric-ids` long-form arg. The related capability is
`CF_ID0_NAMES` (bit 8), which controls whether uid/gid 0 names are
transmitted. However, `--numeric-ids` and `CF_ID0_NAMES` are independent:
`--numeric-ids` tells the receiver to not map names to local ids;
`CF_ID0_NAMES` controls whether root's name is included in the id map at
all. Both can be active simultaneously without conflict.

**oc-rsync.** Sent as `--numeric-ids` long-form
(`core/src/client/remote/invocation/tests.rs:1114-1125`). `CF_ID0_NAMES`
is advertised unconditionally in SSH client mode (`setup/mod.rs:216`)
and via `u` in daemon mode (`capability.rs:111-117`).

**Conformance: MATCH.**

### 4.13 `--relative` (`-R`)

**Upstream.** `--relative` has no `CF_*` bit interaction and no protocol
version restriction. It is sent as the `R` flag in the short-option string.
The flag affects file-list path handling (preserving path components) but
does not touch compat-flag negotiation.

**oc-rsync.** Sent as `R` flag (`core/src/client/remote/flags.rs:107`).
`--files-from` implies `--relative` (`core/src/client/remote/flags.rs:28-31`,
`core/src/client/remote/invocation/builder.rs:462-466`), matching upstream
`options.c:2188`.

**Conformance: MATCH.**

### 4.14 `--files-from`

**Upstream.** `--files-from` has an indirect `CF_*` interaction through
iconv: when `protect_args && files_from`, the `filesfrom_convert` flag
is set based on `CF_SYMLINK_ICONV` (bit 2) and whether `ic_send`/`ic_recv`
are available (`compat.c:799-806`). This controls whether file names read
from `--files-from` undergo character-set conversion.

`--files-from` also has a side effect on incremental recursion: upstream
`options.c:2188` forces `relative_paths = 1` and `recurse = 0` when
`--files-from` is active. The `recurse = 0` suppresses `allow_inc_recurse`
in `set_allow_inc_recurse()` (`compat.c:171`), so `CF_INC_RECURSE`
is never set when `--files-from` is in use.

**oc-rsync.** The `--files-from` implies `--relative` path
(`core/src/client/remote/invocation/builder.rs:462-466`). The `iconv`
interaction is gated on the `iconv` cargo feature, matching upstream's
`#ifdef ICONV_OPTION`.

**Conformance: MATCH.**

## 5. Cross-Cutting Interaction: `allow_inc_recurse`

The `CF_INC_RECURSE` bit (0) is the most cross-cutting compatibility flag
because several options suppress it indirectly through `set_allow_inc_recurse()`
(`compat.c:161-179`).

| Condition | Upstream gate | oc-rsync gate | Status |
|-----------|---------------|---------------|:------:|
| `!recurse` | `compat.c:171` | config-build time | MATCH |
| `use_qsort` | `compat.c:171` | config-build time | MATCH |
| Receiver + `delete_before` | `compat.c:173-174` | config-build time | MATCH |
| Receiver + `delete_after` | `compat.c:174` | config-build time | MATCH |
| Receiver + `delay_updates` | `compat.c:175` | config-build time | MATCH |
| Receiver + `prune_empty_dirs` | `compat.c:175` | config-build time | MATCH |
| Server + client lacks `i` | `compat.c:177-178` | `build_compat_flags_from_client_info` (`capability.rs:222-250`) | MATCH |
| `--files-from` (forces `recurse=0`) | `options.c:2188` + `compat.c:171` | invocation builder (`builder.rs:462`) | MATCH |

## 6. Cross-Cutting Interaction: Protocol-Version Restrictions

Options that impose protocol-version minimums, enforced in
`compat.c:641-709` / `restrictions.rs`.

| Option | Min proto | Upstream cite | oc-rsync cite | Status |
|--------|:---------:|---------------|---------------|:------:|
| `--acls` | 30 | `compat.c:655-661` | `restrictions.rs:96-102` | MATCH |
| `--xattrs` | 30 | `compat.c:662-668` | `restrictions.rs:104-109` | MATCH |
| `--fuzzy` | 29 | `compat.c:679-685` | `restrictions.rs:124-129` | MATCH |
| `--inplace` + basis dirs | 29 | `compat.c:687-693` | `restrictions.rs:132-139` | MATCH |
| Multiple basis dirs | 29 | `compat.c:695-701` | `restrictions.rs:143-151` | MATCH |
| `--prune-empty-dirs` | 29 | `compat.c:703-709` | `restrictions.rs:154-161` | MATCH |
| `--append` (mode 1->2) | <30 forces mode 2 | `compat.c:653-654` | `restrictions.rs:91-93` | MATCH |
| `--delete` default phase | <30: before; >=30: during | `compat.c:671-676` | `restrictions.rs:113-119` | MATCH |
| `--crtimes` | requires `CF_VARINT_FLIST_FLAGS` | `compat.c:750-753` | protocol layer | MATCH |

## 7. Negotiation-Dependent Options

Options whose behaviour varies based on the outcome of vstring negotiation
(gated by `CF_VARINT_FLIST_FLAGS`, bit 7).

| Option | Without negotiation (proto <30 or no `v`) | With negotiation (proto >=30 + `v`) | oc-rsync handling |
|--------|-------------------------------------------|-------------------------------------|-------------------|
| `--checksum` | MD4 for proto <30; MD5 for proto >=30 | Peer-negotiated (XXH3/XXH128/MD5/MD4) | `setup/mod.rs:162-173` (legacy) and `negotiate_capabilities_with_override` (modern) |
| `--compress` | CPRES_ZLIB only | Peer-negotiated (zstd/lz4/zlibx/zlib) | `setup/mod.rs:109` (`send_compression` guard) and legacy fallback at `setup/mod.rs:163-165` |
| `--compress-choice=ALGO` | Specified algorithm used directly | Specified algorithm used directly (vstring skipped) | `setup/mod.rs:109` - `compress_choice.is_none()` mirrors upstream `compat.c:543` |

## 8. Summary of Findings

All 14 audited options conform to upstream rsync 3.4.1 behaviour with
respect to `CF_*` compatibility flag interactions. The mapping is:

- **3 options** have direct `CF_*` bit interactions: `--checksum` (via
  `CF_CHKSUM_SEED_FIX`), `--inplace` (via `CF_INPLACE_PARTIAL_DIR`),
  `--xattrs` (via `CF_AVOID_XATTR_OPTIM`).
- **3 options** have indirect `CF_INC_RECURSE` interactions through
  `allow_inc_recurse` suppression: `--delete` (before/after phases),
  `--append` (via option validation), `--files-from` (via `recurse=0`).
- **2 options** participate in vstring negotiation gated by
  `CF_VARINT_FLIST_FLAGS`: `--checksum` (algorithm selection), `--compress`
  (algorithm selection).
- **2 options** have protocol-version restrictions enforced in
  `setup_protocol()`: `--acls` (>=30), `--xattrs` (>=30).
- **6 options** have no `CF_*` interaction: `--partial`, `--whole-file`,
  `--hard-links`, `--fake-super`, `--numeric-ids`, `--relative`.

No conformance gaps were identified in the compat-flag layer. Known CLI
validation gaps (e.g., `--append --whole-file` not rejected at config-build
time) are documented in the companion `compat-flags-matrix.md` audit and
are options-level issues, not compat-flag divergences.
