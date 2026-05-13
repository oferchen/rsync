# Compatibility-Flags Full Matrix Audit (#2106)

Comprehensive audit of every `CF_*` compatibility flag defined in upstream
rsync 3.4.1 and its implementation status in oc-rsync. Covers flag
inventory, `build_our_flags()` correctness, consumer-side honour logic,
option interaction matrix, protocol version gating, and risk assessment.

## Sources consulted

- Upstream rsync 3.4.1: `compat.c` (lines 117-125 for CF_* defines,
  572-830 for `setup_protocol`, 710-783 for compat-flags build/consume
  block), `rsync.h`, `options.c`, `flist.c`, `receiver.c`, `xattrs.c`.
- `crates/protocol/src/compatibility/{flags.rs,known.rs,iter.rs}` -
  CF_* constants, enum, iteration.
- `crates/transfer/src/setup/{mod.rs,capability.rs,compat.rs,
  restrictions.rs,types.rs}` - negotiation and build logic.
- `crates/transfer/src/lib.rs` - post-setup flag consumption.
- `crates/transfer/src/shared/checksum.rs` - seed ordering.
- `crates/transfer/src/receiver/{file_list.rs,wire.rs,transfer.rs,
  transfer/pipeline.rs}` - receiver-side consumers.
- `crates/transfer/src/generator/{mod.rs,protocol_io.rs,itemize.rs,
  transfer.rs}` - generator-side consumers.
- `crates/transfer/src/transfer_ops/mod.rs` - transfer config.
- `crates/protocol/src/flist/{read/flags.rs,write/mod.rs}` - flist
  flag encoding.

## 1. Flag Inventory

All 9 bits defined in upstream `compat.c:117-125`. Exchanged as a
server-written varint at protocol >= 30; client reads. No exchange at
protocol < 30.

| Bit | Upstream macro | Value | oc-rsync constant | Cap char | Intro |
|----:|----------------|------:|-------------------|:--------:|:-----:|
| 0 | `CF_INC_RECURSE` | 0x01 | `INC_RECURSE` | `i` | 30 |
| 1 | `CF_SYMLINK_TIMES` | 0x02 | `SYMLINK_TIMES` | `L` | 30 |
| 2 | `CF_SYMLINK_ICONV` | 0x04 | `SYMLINK_ICONV` | `s` | 30 |
| 3 | `CF_SAFE_FLIST` | 0x08 | `SAFE_FILE_LIST` | `f` | 30 |
| 4 | `CF_AVOID_XATTR_OPTIM` | 0x10 | `AVOID_XATTR_OPTIMIZATION` | `x` | 30 |
| 5 | `CF_CHKSUM_SEED_FIX` | 0x20 | `CHECKSUM_SEED_FIX` | `C` | 30 |
| 6 | `CF_INPLACE_PARTIAL_DIR` | 0x40 | `INPLACE_PARTIAL_DIR` | `I` | 30 |
| 7 | `CF_VARINT_FLIST_FLAGS` | 0x80 | `VARINT_FLIST_FLAGS` | `v` | 30 |
| 8 | `CF_ID0_NAMES` | 0x100 | `ID0_NAMES` | `u` | 31 |

Source: `crates/protocol/src/compatibility/flags.rs:34-50`,
`crates/protocol/src/compatibility/known.rs:16-43`.

## 2. Per-Flag Implementation Status

### 2.1 CF_INC_RECURSE (bit 0, char `i`)

**Defines constant:** Yes - `CompatibilityFlags::INC_RECURSE` (`flags.rs:34`).

**Sets in `build_our_flags()`:** Yes.
- SSH/client mode: set when `config.allow_inc_recurse` is true
  (`setup/mod.rs:233-235`).
- Daemon server mode: set via `CAPABILITY_MAPPINGS` table when
  `client_info` contains `'i'` and `allow_inc_recurse` is true
  (`capability.rs:42-48`).

**Reads and honours from peer:** Yes.
- Client clears the bit on read when local `allow_inc_recurse` is
  false (`setup/mod.rs:121-123`), matching upstream `compat.c:720`.
- Generator checks `compat_flags.contains(INC_RECURSE)` to enable
  incremental file list partitioning (`generator/mod.rs:506-509`).
- Generator skips uid/gid id-list sending when INC_RECURSE is active
  (`generator/protocol_io.rs:45-52`), matching upstream `flist.c:2513`.
- Receiver skips id-list reading when INC_RECURSE is active
  (`receiver/file_list.rs:60-65`).
- Generator transfer loop handles per-segment flist processing and
  `NDX_FLIST_EOF` generation when INC_RECURSE is active
  (`generator/transfer.rs:88-155`).

**Upstream match:** Yes - all paths verified.

**Risk:** LOW.

### 2.2 CF_SYMLINK_TIMES (bit 1, char `L`)

**Defines constant:** Yes - `CompatibilityFlags::SYMLINK_TIMES` (`flags.rs:36`).

**Sets in `build_our_flags()`:** Yes.
- SSH/client mode: set under `#[cfg(unix)]` (`setup/mod.rs:219-222`),
  mirroring upstream `#ifdef CAN_SET_SYMLINK_TIMES` (`compat.c:713-714`).
- Daemon server mode: `CAPABILITY_MAPPINGS` row with `platform_ok`
  gated on `cfg(unix)` (`capability.rs:50-59`).

**Reads and honours from peer:** Yes.
- Generator's `itemize_context()` reads the bit to set
  `receiver_symlink_times`, controlling the `t`/`T` display at
  position 4 in itemize output (`generator/mod.rs:524-527`).
- Upstream reference: `compat.c:756-761` sets `receiver_symlink_times`
  from the bit (or `client_info` for server-sender direction).

**Upstream match:** Yes.

**Risk:** LOW.

### 2.3 CF_SYMLINK_ICONV (bit 2, char `s`)

**Defines constant:** Yes - `CompatibilityFlags::SYMLINK_ICONV` (`flags.rs:38`).

**Sets in `build_our_flags()`:** Yes.
- SSH/client mode: set under `#[cfg(all(unix, feature = "iconv"))]`
  (`setup/mod.rs:228-231`), mirroring upstream `#ifdef ICONV_OPTION`
  (`compat.c:716-718`).
- Daemon server mode: `CAPABILITY_MAPPINGS` row with
  `requires_iconv: true` (`capability.rs:65-71`); skipped at runtime
  when `iconv_capability_compiled_in()` returns false
  (`capability.rs:127-129`).

**Reads and honours from peer:** Partially. The `iconv` feature is not
yet fully implemented. The flag is correctly gated at the
advertisement layer, but the actual symlink payload transcoding
(`sender_symlink_iconv` in upstream) is not wired.

**Upstream match:** Correct at the flag exchange layer. The runtime
behaviour (actual iconv transcoding of symlink payloads) is not yet
implemented.

**Risk:** LOW - the flag is only advertised when the `iconv` cargo
feature is enabled, which is off by default. No interop failure can
result from the current state because peers without the feature do not
advertise `s`.

### 2.4 CF_SAFE_FLIST (bit 3, char `f`)

**Defines constant:** Yes - `CompatibilityFlags::SAFE_FILE_LIST` (`flags.rs:40`).

**Sets in `build_our_flags()`:** Yes.
- SSH/client mode: set unconditionally (`setup/mod.rs:213`).
- Daemon server mode: set via `CAPABILITY_MAPPINGS` when `client_info`
  contains `'f'` (`capability.rs:72-78`).

**Reads and honours from peer:** Yes.
- File list writer enables safe mode when the bit is present or
  protocol >= 31 (`flist/write/mod.rs:161-162`), matching upstream
  `use_safe_inc_flist = (compat_flags & CF_SAFE_FLIST) ||
  protocol_version >= 31` (`compat.c:775`).
- File list reader mirrors the same logic (`flist/read/flags.rs:43-47`).
- Generator writes io_error with end marker in SAFE_FILE_LIST mode
  (`generator/protocol_io.rs:243`).
- Receiver reads io_error separately for protocol < 30, relies on
  MSG_IO_ERROR or SAFE_FILE_LIST for protocol >= 30
  (`receiver/file_list.rs:67-77`).

**Upstream match:** Yes.

**Risk:** LOW.

### 2.5 CF_AVOID_XATTR_OPTIM (bit 4, char `x`)

**Defines constant:** Yes - `CompatibilityFlags::AVOID_XATTR_OPTIMIZATION`
(`flags.rs:42`).

**Sets in `build_our_flags()`:** Yes.
- SSH/client mode: set unconditionally (`setup/mod.rs:214`).
- Daemon server mode: set via `CAPABILITY_MAPPINGS` when `client_info`
  contains `'x'` (`capability.rs:80-86`).

**Reads and honours from peer:** SEMANTIC INVERSION FOUND.

Upstream sets `want_xattr_optim = protocol_version >= 31 &&
!(compat_flags & CF_AVOID_XATTR_OPTIM)` (`compat.c:746`). The variable
is `true` when the optimization SHOULD be used (peer does NOT set the
AVOID flag) and `false` when it should be avoided (AVOID flag IS set).

oc-rsync sets `want_xattr_optim` to the result of
`compat_flags.contains(AVOID_XATTR_OPTIMIZATION)` in two places:
- `receiver/transfer/pipeline.rs:89-91`
- `receiver/transfer/pipeline.rs:418-420`

This is semantically inverted: oc-rsync sets `want_xattr_optim = true`
when `CF_AVOID_XATTR_OPTIM` IS present, but upstream sets it to `true`
when the bit is NOT present.

The consumer in `receiver/wire.rs:294-298` uses the same conditional
structure as upstream:
```
!(want_xattr_optim && ITEM_XNAME_FOLLOWS && ITEM_LOCAL_CHANGE)
```

With the inverted value, oc-rsync will skip xattr data reads for
hardlinked local-change items when CF_AVOID_XATTR_OPTIM IS set (the
opposite of upstream behaviour).

Additionally, upstream gates the optimization on `protocol_version >= 31`.
oc-rsync omits this version check entirely. Since oc-rsync targets
protocol 32, this is less critical but worth noting.

The detection logic in `lib.rs:453-456` (warning when remote daemon
lacks xattr support) correctly uses the negative form
(`!flags.contains(...)`), so that path is correct.

**Upstream match:** DIVERGENCE - semantic inversion of `want_xattr_optim`
and missing protocol >= 31 gate.

**Risk:** MEDIUM - manifests only when `--xattrs` and `--hard-links` are
both active, and only for files that are hardlinked local changes. Both
sides currently always advertise `'x'`, so the AVOID bit is always set,
making `want_xattr_optim` always `true` in oc-rsync (should be `false`
in upstream). Practical impact: oc-rsync may incorrectly skip reading
xattr abbreviation data for hardlinked local-change items during
`--xattrs --hard-links` transfers. This would cause a wire desync with
upstream rsync when those specific conditions are met.

### 2.6 CF_CHKSUM_SEED_FIX (bit 5, char `C`)

**Defines constant:** Yes - `CompatibilityFlags::CHECKSUM_SEED_FIX`
(`flags.rs:44`).

**Sets in `build_our_flags()`:** Yes.
- SSH/client mode: set unconditionally (`setup/mod.rs:211`).
- Daemon server mode: set via `CAPABILITY_MAPPINGS` when `client_info`
  contains `'C'` (`capability.rs:88-94`).

**Reads and honours from peer:** Yes.
- `ChecksumFactory::from_negotiation()` reads the bit to set
  `use_proper_seed_order` (`shared/checksum.rs:90-91`).
- When true: MD5 seed is hashed before data (proper order).
- When false: MD5 seed is hashed after data (legacy order).
- Matches upstream `proper_seed_order = compat_flags &
  CF_CHKSUM_SEED_FIX ? 1 : 0` (`compat.c:747`).

**Upstream match:** Yes.

**Risk:** LOW.

### 2.7 CF_INPLACE_PARTIAL_DIR (bit 6, char `I`)

**Defines constant:** Yes - `CompatibilityFlags::INPLACE_PARTIAL_DIR`
(`flags.rs:46`).

**Sets in `build_our_flags()`:** Yes.
- SSH/client mode: set unconditionally (`setup/mod.rs:215`).
- Daemon server mode: set via `CAPABILITY_MAPPINGS` when `client_info`
  contains `'I'` (`capability.rs:96-102`).

**Reads and honours from peer:** Yes.
- After compat exchange, `lib.rs:442-447` checks the bit and sets
  `config.write.inplace_partial = true` when a partial directory is
  configured. Matches upstream `if (compat_flags &
  CF_INPLACE_PARTIAL_DIR) inplace_partial = 1` (`compat.c:777-778`).
- `WriteConfig::inplace_partial` is consumed in the receiver transfer
  pipeline to decide per-file write strategy
  (`transfer_ops/mod.rs:31-41`).

**Upstream match:** Yes.

**Risk:** LOW.

### 2.8 CF_VARINT_FLIST_FLAGS (bit 7, char `v`)

**Defines constant:** Yes - `CompatibilityFlags::VARINT_FLIST_FLAGS`
(`flags.rs:48`).

**Sets in `build_our_flags()`:** Yes.
- SSH/client mode: set unconditionally (`setup/mod.rs:212`).
- Daemon server mode: set via `CAPABILITY_MAPPINGS` when `client_info`
  contains `'v'` (`capability.rs:103-109`).
- Pre-release `'V'` path: implicitly ORed in when `client_info`
  contains `'V'` (`setup/compat.rs:43-47`).

**Reads and honours from peer:** Yes.
- `should_negotiate()` (`setup/mod.rs:251-268`) gates vstring
  checksum/compression negotiation on this bit.
- File list writer uses `use_varint_flags` from the bit
  (`flist/write/mod.rs:160`).
- File list reader uses `use_varint_flags` from the bit
  (`flist/read/flags.rs:37-39`).
- Pre-release `'V'` encoding handled in `write_compat_flags`
  (`setup/compat.rs:37-52`).

**Upstream match:** Yes - all three observable consequences verified:
string negotiation gate, file-list flag width, and encoding asymmetry.

**Risk:** LOW.

### 2.9 CF_ID0_NAMES (bit 8, char `u`)

**Defines constant:** Yes - `CompatibilityFlags::ID0_NAMES`
(`flags.rs:50`).

**Sets in `build_our_flags()`:** Yes.
- SSH/client mode: set unconditionally (`setup/mod.rs:216`).
- Daemon server mode: set via `CAPABILITY_MAPPINGS` when `client_info`
  contains `'u'` (`capability.rs:111-117`).

**Reads and honours from peer:** Yes.
- Generator's `send_id_lists()` uses the bit to include an extra name
  for id=0 after the terminator (`generator/protocol_io.rs:54-56`).
- Receiver's `receive_id_lists()` reads the extra id=0 name when the
  bit is set (`receiver/file_list.rs:260,307`).
- Matches upstream `xmit_id0_names = compat_flags & CF_ID0_NAMES ? 1
  : 0` (`compat.c:749`).

**Upstream match:** Yes.

**Risk:** LOW.

## 3. Option Interaction Matrix

How key CLI options interact with compatibility flags.

### Legend

- **Direct** - option directly sets/checks a CF_* bit.
- **Indirect** - option affects `allow_inc_recurse`, gating CF_INC_RECURSE.
- **Negotiation** - option participates in vstring exchange gated by
  CF_VARINT_FLIST_FLAGS.
- **Restriction** - option triggers a protocol-version minimum.
- **None** - no CF_* interaction.

| Option | CF_* interaction | Type | Proto min | Match upstream |
|--------|-----------------|:----:|:---------:|:--------------:|
| `--checksum` (`-c`) | `CF_CHKSUM_SEED_FIX` (bit 5), `CF_VARINT_FLIST_FLAGS` (bit 7) | Negotiation | 28 | YES |
| `--inplace` | `CF_INPLACE_PARTIAL_DIR` (bit 6) | Direct | 29 w/basis | YES |
| `--partial` | none | None | 28 | YES |
| `--partial-dir` | `CF_INPLACE_PARTIAL_DIR` (bit 6) | Direct | 28 | YES |
| `--whole-file` (`-W`) | none | None | 28 | YES |
| `--append` | `CF_INPLACE_PARTIAL_DIR` (bit 6) via forced inplace | Indirect | 28 | YES |
| `--delay-updates` | `CF_INC_RECURSE` (bit 0) suppression | Indirect | 28 | YES |
| `--compress` (`-z`) | `CF_VARINT_FLIST_FLAGS` (bit 7) | Negotiation | 28 | YES |
| `--compress-choice` | `CF_VARINT_FLIST_FLAGS` (bit 7), vstring skipped | Negotiation | 28 | YES |
| `--xattrs` (`-X`) | `CF_AVOID_XATTR_OPTIM` (bit 4) | Direct | 30 | **PARTIAL** |
| `--hard-links` (`-H`) | `CF_AVOID_XATTR_OPTIM` (bit 4) tangential | None | 28 | **PARTIAL** |
| `--acls` (`-A`) | none (restriction only) | Restriction | 30 | YES |
| `--delete-before` | `CF_INC_RECURSE` (bit 0) suppression | Indirect | 28 | YES |
| `--delete-during` | none | None | 28 | YES |
| `--delete-after` | `CF_INC_RECURSE` (bit 0) suppression | Indirect | 28 | YES |
| `--crtimes` (`-N`) | requires `CF_VARINT_FLIST_FLAGS` (bit 7) | Direct | 30 | YES |
| `--inc-recursive` | `CF_INC_RECURSE` (bit 0) | Direct | 30 | YES |
| `--no-inc-recursive` | `CF_INC_RECURSE` (bit 0) cleared | Direct | 30 | YES |
| `--fake-super` | none | None | 28 | YES |
| `--numeric-ids` | `CF_ID0_NAMES` (bit 8) tangential | None | 28 | YES |

### Detail: `--xattrs` + `--hard-links` divergence

The `CF_AVOID_XATTR_OPTIM` flag interaction has a semantic inversion
in the receiver pipeline (section 2.5). Upstream computes:
```c
want_xattr_optim = protocol_version >= 31 && !(compat_flags & CF_AVOID_XATTR_OPTIM);
```

oc-rsync computes:
```rust
want_xattr_optim = compat_flags.contains(AVOID_XATTR_OPTIMIZATION);
```

The flag at the wire and advertisement layer is correct. The
divergence is in the consumer-side logic where the received bit is
interpreted with inverted polarity.

### Detail: `--checksum` path

The `--checksum` flag does not directly set any CF_* bit. Two indirect
interactions work correctly:

1. **Seed ordering.** `ChecksumFactory` reads `CF_CHKSUM_SEED_FIX` to
   select legacy (seed after data) or proper (seed before data) MD5
   ordering (`shared/checksum.rs:90-91`).

2. **Algorithm negotiation.** Checksum algorithm is negotiated via
   vstrings gated by `CF_VARINT_FLIST_FLAGS`. Without the bit, fallback
   is MD5 for protocol >= 30 or MD4 for protocol < 30
   (`setup/mod.rs:162-173`).

### Detail: `--compress` path

The `--compress` flag participates in compression algorithm negotiation
gated by `CF_VARINT_FLIST_FLAGS`:

1. Vstring exchange occurs only when `do_compression && !compress_choice`
   (`setup/mod.rs:109`), matching upstream `compat.c:543`.
2. Without negotiation, default is zlib (`setup/mod.rs:163-165`).
3. `--compress-choice=ALGO` skips vstring exchange; specified algorithm
   used directly.

### Detail: `--inplace` + `--partial-dir`

When `CF_INPLACE_PARTIAL_DIR` (bit 6) is negotiated:
- `inplace_partial = true` is set after compat exchange (`lib.rs:442-447`).
- Files whose basis comes from `--partial-dir` are written in-place
  (`transfer_ops/mod.rs:31-41`).
- Other files still use temp+rename.
- Matches upstream `compat.c:777-778` and `receiver.c:797`.

## 4. Protocol Version Gating

### Flags exchange gating

The entire compat-flags exchange is gated on protocol >= 30
(`protocol.uses_binary_negotiation()` at `setup/mod.rs:104`). At
protocol < 30, no flags are exchanged and `compat_flags` is `None`.

### Per-flag version sensitivity

| Flag | Introduced | Version-specific behaviour | oc-rsync handling |
|------|:----------:|---------------------------|-------------------|
| `CF_INC_RECURSE` | 30 | Only exchanged at >= 30 | Correct - exchange gate |
| `CF_SYMLINK_TIMES` | 30 | Only exchanged at >= 30 | Correct - exchange gate |
| `CF_SYMLINK_ICONV` | 30 | Only exchanged at >= 30 | Correct - exchange gate |
| `CF_SAFE_FLIST` | 30 | Forced on at >= 31 (`compat.c:775`) | Correct - `safe_file_list_always_enabled()` |
| `CF_AVOID_XATTR_OPTIM` | 30 | Consumer gated on >= 31 upstream | **MISSING** - no >= 31 gate in oc-rsync |
| `CF_CHKSUM_SEED_FIX` | 30 | Only exchanged at >= 30 | Correct - exchange gate |
| `CF_INPLACE_PARTIAL_DIR` | 30 | Only exchanged at >= 30 | Correct - exchange gate |
| `CF_VARINT_FLIST_FLAGS` | 30 | Controls negotiation and flist encoding | Correct |
| `CF_ID0_NAMES` | 31+ | Introduced in rsync 3.2.4 | Correct - always advertised at >= 30 |

### Protocol restriction enforcement

All restrictions from upstream `compat.c:641-709` are enforced in
`restrictions.rs`:

| Restriction | Proto | Upstream cite | oc-rsync cite | Status |
|-------------|:-----:|---------------|---------------|:------:|
| `--acls` requires 30+ | < 30 | `compat.c:655-661` | `restrictions.rs:96-102` | ok |
| `--xattrs` requires 30+ | < 30 | `compat.c:662-668` | `restrictions.rs:104-109` | ok |
| `--fuzzy` requires 29+ | < 29 | `compat.c:679-685` | `restrictions.rs:124-129` | ok |
| `--inplace` + basis requires 29+ | < 29 | `compat.c:687-693` | `restrictions.rs:132-139` | ok |
| Multiple basis dirs requires 29+ | < 29 | `compat.c:695-701` | `restrictions.rs:143-151` | ok |
| `--prune-empty-dirs` requires 29+ | < 29 | `compat.c:703-709` | `restrictions.rs:154-161` | ok |
| `append_mode` 1 forced to 2 | < 30 | `compat.c:653-654` | `restrictions.rs:91-93` | ok |
| Default delete phase | < 30 | `compat.c:671-676` | `restrictions.rs:113-119` | ok |
| `--crtimes` requires varint flags | >= 30 | `compat.c:750-753` | protocol layer | ok |

## 5. Cross-Cutting: `allow_inc_recurse` Suppressors

The `CF_INC_RECURSE` bit is the most cross-cutting flag. Upstream's
`set_allow_inc_recurse()` (`compat.c:161-179`) clears
`allow_inc_recurse` under these conditions:

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

## 6. Risk Assessment Summary

| Flag | Defines | Sets | Reads | Consumers | Risk | Notes |
|------|:-------:|:----:|:-----:|:---------:|:----:|-------|
| `CF_INC_RECURSE` | ok | ok | ok | ok | LOW | Fully wired receiver-side; sender enabled |
| `CF_SYMLINK_TIMES` | ok | ok | ok | ok | LOW | |
| `CF_SYMLINK_ICONV` | ok | ok | ok | N/A | LOW | iconv feature not yet implemented; no interop risk |
| `CF_SAFE_FLIST` | ok | ok | ok | ok | LOW | Forced on at >= 31 matches upstream |
| `CF_AVOID_XATTR_OPTIM` | ok | ok | ok | **BUG** | **MEDIUM** | Semantic inversion + missing proto >= 31 gate |
| `CF_CHKSUM_SEED_FIX` | ok | ok | ok | ok | LOW | |
| `CF_INPLACE_PARTIAL_DIR` | ok | ok | ok | ok | LOW | |
| `CF_VARINT_FLIST_FLAGS` | ok | ok | ok | ok | LOW | Pre-release 'V' handled correctly |
| `CF_ID0_NAMES` | ok | ok | ok | ok | LOW | |

### HIGH-risk findings

None.

### MEDIUM-risk findings

**CF_AVOID_XATTR_OPTIM semantic inversion (receiver/transfer/pipeline.rs).**

Two issues:

1. **Inverted polarity.** The `want_xattr_optim` field in
   `FileRequestConfig` (`transfer_ops/mod.rs:127-135`) is set to
   `compat_flags.contains(AVOID_XATTR_OPTIMIZATION)`. Upstream sets it
   to `!(compat_flags & CF_AVOID_XATTR_OPTIM)`. The variable is then
   used in `receiver/wire.rs:294-298` with the same conditional
   structure as upstream, producing inverted behaviour.

   Affected paths:
   - `receiver/transfer/pipeline.rs:89-91` (concurrent pipeline)
   - `receiver/transfer/pipeline.rs:418-420` (sequential pipeline)
   - `receiver/transfer.rs:217-219` (redo transfer)

   **Effect:** When both peers advertise `'x'` (which oc-rsync always
   does), `CF_AVOID_XATTR_OPTIM` is set, upstream computes
   `want_xattr_optim = false` (never skip xattr reads), but oc-rsync
   computes `want_xattr_optim = true` (may skip xattr reads for
   hardlinked local-change items). This can cause a wire desync when
   `--xattrs` and `--hard-links` are both active.

   **Fix:** Negate the contains check:
   ```rust
   want_xattr_optim: self.compat_flags.is_some_and(|f| {
       !f.contains(protocol::CompatibilityFlags::AVOID_XATTR_OPTIMIZATION)
   }),
   ```
   And add the protocol >= 31 gate:
   ```rust
   want_xattr_optim: self.protocol.as_u8() >= 31
       && self.compat_flags.is_some_and(|f| {
           !f.contains(protocol::CompatibilityFlags::AVOID_XATTR_OPTIMIZATION)
       }),
   ```

2. **Missing protocol >= 31 gate.** Upstream requires
   `protocol_version >= 31` for `want_xattr_optim`. oc-rsync does not
   check the protocol version. Since oc-rsync targets protocol 32 and
   does not support < 28, this is low additional risk but should be
   corrected for completeness.

### LOW-risk findings

- `CF_SYMLINK_ICONV` runtime behaviour (iconv transcoding) is not yet
  implemented, but the flag is correctly gated behind the disabled
  `iconv` cargo feature.

## 7. Wire Encoding Verification

The compat flags wire format is verified by golden byte tests in
`crates/protocol/tests/`:

- `golden_compat_flags_full_modern` pins INC_RECURSE | SYMLINK_TIMES |
  SAFE_FILE_LIST | CHECKSUM_SEED_FIX | VARINT_FLIST_FLAGS | ID0_NAMES
  (0x1AB) to bytes `[0x81, 0xAB]`.
- `golden_compat_flags_all_known` pins all 9 flags (0x1FF) to bytes
  `[0x81, 0xFF]`.
- Compatibility flag round-trip tests verify varint encode/decode
  preserves all bits including unknown future bits.
- Pre-release `'V'` encoding path tested separately.

## 8. Actionable Items

| Priority | Issue | Files to fix | Risk |
|:--------:|-------|--------------|:----:|
| P1 | Invert `want_xattr_optim` polarity and add proto >= 31 gate | `receiver/transfer/pipeline.rs`, `receiver/transfer.rs` | MEDIUM |
| P2 | Add `want_xattr_optim` inversion to interop test coverage | `tools/ci/run_interop.sh` (xattrs + hardlinks scenario) | LOW |
| P3 | Wire iconv transcoding when `iconv` feature is stabilised | `crates/transfer/` | LOW |
