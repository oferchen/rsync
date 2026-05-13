# Compatibility-Flags Matrix Conformance

Task: #2106

Comprehensive audit of how oc-rsync handles compatibility flags compared
to upstream rsync 3.4.1. Covers the full lifecycle: CLI flag parsing,
mutual-exclusion validation, capability string advertisement,
protocol-30+ `compat_flags` bitfield exchange, and post-negotiation flag
interactions.

Related audits: `compat-flags-audit.md` (wire bitfield deep-dive),
`compat-flags-matrix.md` (CLI option-set validation only),
`flist-flag-matrix-audit.md` (flist xflags).

## 1. Capability String (`-e.xxx`)

Upstream `options.c:3003-3047 maybe_add_e_option()` builds the capability
string sent as part of the server args. Each character advertises a
feature the client supports.

| Char | Upstream | oc-rsync | Status |
|------|----------|----------|--------|
| `.` | Always emitted as version placeholder (line 3020). | `capability.rs:196` - skips leading dot when parsing; `build_capability_string()` emits `-e.` prefix. | Conformant |
| `i` | INC_RECURSE - only when `allow_inc_recurse` is true (line 3021). | `CAPABILITY_MAPPINGS[0]` - gated by `requires_inc_recurse: true`. `build_capability_string(false)` omits it. | Conformant |
| `L` | SYMLINK_TIMES - `#ifdef CAN_SET_SYMLINK_TIMES` (line 3024). | `CAPABILITY_MAPPINGS[1]` - `#[cfg(unix)] platform_ok: true`, `#[cfg(not(unix))] platform_ok: false`. | Conformant |
| `s` | SYMLINK_ICONV - `#ifdef ICONV_OPTION` (line 3027). | `CAPABILITY_MAPPINGS[2]` - `requires_iconv: true`, runtime gate via `iconv_capability_compiled_in()` checking `cfg!(feature = "iconv")`. | Conformant |
| `f` | SAFE_FLIST - always (line 3029). | `CAPABILITY_MAPPINGS[3]` - unconditional. | Conformant |
| `x` | AVOID_XATTR_OPTIM - always (line 3030). | `CAPABILITY_MAPPINGS[4]` - unconditional. | Conformant |
| `C` | CHKSUM_SEED_FIX - always (line 3031). | `CAPABILITY_MAPPINGS[5]` - unconditional. | Conformant |
| `I` | INPLACE_PARTIAL_DIR - always (line 3032). | `CAPABILITY_MAPPINGS[6]` - unconditional. | Conformant |
| `v` | VARINT_FLIST_FLAGS + checksum negotiation - always (line 3033). | `CAPABILITY_MAPPINGS[7]` - unconditional. | Conformant |
| `u` | ID0_NAMES - always (line 3034). | `CAPABILITY_MAPPINGS[8]` - unconditional. | Conformant |
| `V` | Deprecated pre-release varint. Upstream warns "avoid using 'V'" (line 3036). | `client_has_pre_release_v_flag()` detects it; `write_compat_flags()` falls back to `write_byte()`. | Conformant |

Mapping order matches upstream. Table-driven via `CAPABILITY_MAPPINGS` in
`crates/transfer/src/setup/capability.rs`.

## 2. Compat Flags Bitfield (Protocol >= 30)

Upstream `compat.c:117-125` defines `CF_*` macros. The server writes the
bitfield via `write_varint()` (or `write_byte()` for pre-release `'V'`
clients); the client reads via `read_varint()`.

| Bit | Flag | Upstream | oc-rsync | Status |
|-----|------|----------|----------|--------|
| 0 | `CF_INC_RECURSE` | Set when `allow_inc_recurse` and client has `'i'` (compat.c:712). | `CompatibilityFlags::INC_RECURSE = 1 << 0`. Built from `CAPABILITY_MAPPINGS`. | Conformant |
| 1 | `CF_SYMLINK_TIMES` | Set when `CAN_SET_SYMLINK_TIMES` compiled in (compat.c:713-714). | `CompatibilityFlags::SYMLINK_TIMES = 1 << 1`. Platform-gated. | Conformant |
| 2 | `CF_SYMLINK_ICONV` | Set when `ICONV_OPTION` compiled in and client has `'s'` (compat.c:716-718). | `CompatibilityFlags::SYMLINK_ICONV = 1 << 2`. Feature-gated via `iconv` cargo feature. | Conformant |
| 3 | `CF_SAFE_FLIST` | Set when client has `'f'` (compat.c:719-720). | `CompatibilityFlags::SAFE_FILE_LIST = 1 << 3`. | Conformant |
| 4 | `CF_AVOID_XATTR_OPTIM` | Set when client has `'x'` (compat.c:721-722). | `CompatibilityFlags::AVOID_XATTR_OPTIMIZATION = 1 << 4`. | Conformant |
| 5 | `CF_CHKSUM_SEED_FIX` | Set when client has `'C'` (compat.c:723-724). | `CompatibilityFlags::CHECKSUM_SEED_FIX = 1 << 5`. | Conformant |
| 6 | `CF_INPLACE_PARTIAL_DIR` | Set when client has `'I'` (compat.c:725-726). | `CompatibilityFlags::INPLACE_PARTIAL_DIR = 1 << 6`. | Conformant |
| 7 | `CF_VARINT_FLIST_FLAGS` | Set when client has `'v'` (or `'V'` implicitly) (compat.c:727-736). | `CompatibilityFlags::VARINT_FLIST_FLAGS = 1 << 7`. Pre-release `'V'` handled. | Conformant |
| 8 | `CF_ID0_NAMES` | Set when client has `'u'` (compat.c:728-729). | `CompatibilityFlags::ID0_NAMES = 1 << 8`. | Conformant |

### Server-side write path

Upstream `compat.c:737-741`: `write_byte()` for `'V'` client, else
`write_varint()`.

oc-rsync `compat.rs::write_compat_flags()`: mirrors this exactly with
`client_has_pre_release_v_flag()` check and `write_all(&[bits as u8])`
vs `protocol::write_varint()`.

### Client-side read path

Upstream `compat.c:739`: `read_varint()` on client (compatible with
single-byte encoding when high bit is clear).

oc-rsync `RsyncNegotiator::read_compat_flags()`: calls
`protocol::read_varint()` and wraps in `CompatibilityFlags::from_bits()`.

## 3. Post-Exchange Flag Derivations

After `compat_flags` exchange, upstream `compat.c:744-778` derives
several runtime flags.

| Derivation | Upstream | oc-rsync | Status |
|------------|----------|----------|--------|
| `inc_recurse` | `compat_flags & CF_INC_RECURSE ? 1 : 0` (line 745) | Used when `INC_RECURSE` bit is set in compat_flags. Client clears if `!allow_inc_recurse` (`setup/mod.rs:121-123`). | Conformant |
| `want_xattr_optim` | `protocol_version >= 31 && !(compat_flags & CF_AVOID_XATTR_OPTIM)` (line 746) | Not directly exposed as a named flag; the `AVOID_XATTR_OPTIMIZATION` bit disables the optimization path. | Conformant |
| `proper_seed_order` | `compat_flags & CF_CHKSUM_SEED_FIX ? 1 : 0` (line 747) | `CHECKSUM_SEED_FIX` flag is carried through. | Conformant |
| `xfer_flags_as_varint` | `compat_flags & CF_VARINT_FLIST_FLAGS ? 1 : 0` (line 748) | `VARINT_FLIST_FLAGS` controls flist encoding (varint vs legacy byte flags). | Conformant |
| `xmit_id0_names` | `compat_flags & CF_ID0_NAMES ? 1 : 0` (line 749) | `ID0_NAMES` flag is carried through for uid/gid 0 name transmission. | Conformant |
| `do_negotiated_strings` | Set when `CF_VARINT_FLIST_FLAGS` is present (line 742). | `should_negotiate()` checks for the bit. | Conformant |
| `inplace_partial` | `compat_flags & CF_INPLACE_PARTIAL_DIR ? 1 : 0` (line 778) | `inplace_partial` field in `WriteConfig` set from `CF_INPLACE_PARTIAL_DIR`. | Conformant |
| `use_safe_inc_flist` | `CF_SAFE_FLIST \|\| protocol_version >= 31` (line 775) | Implicitly safe for all proto 31+ transfers. | Conformant |
| `receiver_symlink_times` | Server: `strchr(client_info, 'L')`. Client: `CF_SYMLINK_TIMES` (lines 754-758). | Platform-gated. Unix always sets symlink times support. | Conformant |
| `--crtimes` requires varint | `if (!xfer_flags_as_varint && preserve_crtimes)` -> error "Both rsync versions must be at least 3.2.0" (lines 750-753). | Not explicitly enforced as a dedicated check. The `crtimes` flag is only functional when varint flist encoding is active, which is always the case for proto 32 peers. | **Gap (minor)** |

## 4. `set_allow_inc_recurse()` Conditions

Upstream `compat.c:161-179` disables `allow_inc_recurse` under these
conditions:

| Condition | Upstream | oc-rsync | Status |
|-----------|----------|----------|--------|
| `!recurse` | Clears `allow_inc_recurse` (line 171). | `inc_recursive_send` defaults to `false`; only set when `--inc-recursive` passed and `--recurse` active. | Conformant |
| `use_qsort` | Clears `allow_inc_recurse` (line 171). | `qsort` builder method available; `build_capability_string()` takes `allow_inc_recurse` parameter. | Conformant |
| `!am_sender && (delete_before \|\| delete_after \|\| delay_updates \|\| prune_empty_dirs)` | Clears for receiver direction only (lines 173-176). | `build_capability_string(!is_sender)` omits `'i'` when sender direction; receiver logic inherits these constraints. | Conformant |
| `am_server && strchr(client_info, 'i') == NULL` | Server clears if client does not advertise `'i'` (lines 177-178). | `build_compat_flags_from_client_info()` only sets `INC_RECURSE` when client_info contains `'i'` and `allow_inc_recurse` is true. | Conformant |

## 5. Protocol Version Restrictions

Upstream `compat.c:641-709` validates feature-protocol compatibility
after version negotiation.

| Feature | Min Proto | Upstream | oc-rsync | Status |
|---------|-----------|----------|----------|--------|
| `--acls` (`-A`) | 30 | Error if < 30 and not local (lines 655-661). | `apply_protocol_restrictions()` rejects; allows local. | Conformant |
| `--xattrs` (`-X`) | 30 | Error if < 30 and not local (lines 662-668). | Same as above. | Conformant |
| `--fuzzy` (`-y`) | 29 | Error if < 29 (lines 679-685). | `restrictions.rs` rejects. | Conformant |
| `--compare-dest/--copy-dest/--link-dest` + `--inplace` | 29 | Error if < 29 (lines 687-693). | `restrictions.rs` rejects `basis_dir_count > 0 && inplace`. | Conformant |
| Multiple `--compare-dest/--copy-dest/--link-dest` | 29 | Error if < 29 (lines 695-701). | `restrictions.rs` rejects `basis_dir_count > 1`. | Conformant |
| `--prune-empty-dirs` (`-m`) | 29 | Error if < 29 (lines 703-709). | `restrictions.rs` rejects. | Conformant |
| `append_mode = 1` -> 2 | < 30 | Forced to 2 (lines 653-654). | `RestrictionAdjustments::append_mode = Some(2)`. | Conformant |
| `--delete` default phase | < 30: before; >= 30: during | Lines 671-676. | `RestrictionAdjustments::delete_before = Some(true/false)`. | Conformant |

## 6. CLI Mutual-Exclusion Rules

Upstream `options.c:2382-2444` enforces these combinations.

| Rule | Flag combo | Upstream behavior | oc-rsync | Status |
|------|-----------|-------------------|----------|--------|
| R1 | `--append` + `--whole-file` | Error: "cannot be used with" (line 2383-2386). | **Not enforced.** `flags.rs:96-98` silently suppresses wire `-W` when append is active. No error raised. | **Gap** |
| R2 | `--inplace` + `--partial-dir` | Error (lines 2408-2412). | `ClientConfigBuilder::validate()` rejects. | Conformant |
| R3 | `--append` + `--partial-dir` | Error. `--append` sets `inplace=1` (line 2392), then R2 fires. | `validate()` handles via `is_inplace = self.inplace \|\| self.append`. | Conformant |
| R4 | `--inplace` + `--delay-updates` | Error. `--delay-updates` silently sets `partial_dir = ".~tmp~"` (line 2403), then R2 fires. Upstream message says `--partial-dir`. | `validate()` rejects but message says `--delay-updates`. | **Divergence (wording)** |
| R5 | `--append` + `--delay-updates` | Same as R4 via append->inplace alias. | Same as R4. | **Divergence (wording)** |
| R6 | `--inplace` on platform without `ftruncate` | Error (lines 2422-2427). | Not checked - all supported platforms have `ftruncate`. | N/A |
| R7 | `--write-devices` -> `inplace=1` | Implicit (lines 2395-2401). All inplace conflicts apply transitively. | **Not enforced.** `write_devices(true)` does not set `inplace=true`. | **Gap** |
| R8 | `--append-verify` -> `append_mode=2` | Same rules as `--append`. | `append_verify(true)` -> `append=true`. All append rules apply. | Conformant |
| R9 | `--append` + `--whole-file` > 0 | Upstream prints error and exits (line 2383-2386). | Not detected at config-build time. | **Gap** (same as R1) |

### `--inplace` clears `keep_partial`

Upstream `options.c:2421`: `keep_partial = 0` when `inplace` is set.
oc-rsync: the `partial` flag in `ClientConfig` is independent of
`inplace`. However, when both are set, the inplace write path effectively
supersedes partial behavior, so there is no functional divergence.

## 7. Whole-File Auto-Detection

| Scenario | Upstream | oc-rsync | Status |
|----------|----------|----------|--------|
| Local transfer, no explicit flag | `whole_file = 1` (`main.c:643-644`). | `whole_file: Option<bool>` with `None` = auto-detect; documented as "whole-file for local, delta for remote" in `performance.rs:94`. | Conformant (design) |
| `--checksum-choice=none` | `xfer_sum_nni->num == CSUM_NONE` -> `whole_file = 1` (`checksum.c:197-198`). | Not explicitly enforced; if CSUM_NONE is negotiated, delta transfer still runs but produces whole-file tokens. | **Gap (minor)** |
| `append_mode > 0` or `whole_file < 0` | `whole_file = 0` (`generator.c:2271-2272`). Remote default. | Append mode suppresses wire `-W` flag. Default for remote is delta. | Conformant |
| `--whole-file` + `--sparse` + `--inplace` workaround | Upstream sends `--no-W` to work around older rsync bugs (options.c:2940-2941). | `flags.rs:98`: suppresses `W` when append is active. Does not send `--no-W` for the sparse+inplace workaround. | **Gap (minor)** |

## 8. Delete Mode Flag Logic

| Aspect | Upstream | oc-rsync | Status |
|--------|----------|----------|--------|
| `--delete` with no phase | Defaults to `delete_during` for proto >= 30, `delete_before` for < 30 (compat.c:671-676). | `apply_protocol_restrictions()` returns `RestrictionAdjustments::delete_before`. | Conformant |
| `--delete-excluded` | Sets `delete_mode = delete_excluded = 1` (options.c:2828-2829). | `ClientConfigBuilder::delete_excluded()` sets both. | Conformant |
| `--delete-before` | Explicit phase selection. | `DeleteMode::Before`. | Conformant |
| `--delete-during` | Explicit phase selection. | `DeleteMode::During`. | Conformant |
| `--delete-delay` | `delete_during = 2`. | `DeleteMode::Delay`. | Conformant |
| `--delete-after` | Explicit phase selection. | `DeleteMode::After`. | Conformant |
| Receiver + `delete_before` disables inc_recurse | `set_allow_inc_recurse()` clears for `!am_sender && delete_before`. | `build_capability_string(!is_sender)` does not pass `allow_inc_recurse` when receiver has delete_before. | Conformant |

## 9. Compression Flag Logic

| Aspect | Upstream | oc-rsync | Status |
|--------|----------|----------|--------|
| `-z` default algorithm | CPRES_ZLIB (options.c:2704). | `flags.rs:61-66` sends `z` only when algorithm is default. | Conformant |
| `--compress-choice=ALGO` | Sends `--compress-choice ALGO` long-form (options.c:2800-2805). | Sends long-form arg; skips vstring exchange when `compress_choice.is_some()` (`setup/mod.rs:109`). | Conformant |
| `--new-compress` | `compress_choice = "zlibx"` (options.c:1995-1996, 2801). | Mapped to `CompressionAlgorithm::Zlibx`. | Conformant |
| `--old-compress` | `compress_choice = "zlib"` + sends `--old-compress` (options.c:2802-2803). | Mapped to `CompressionAlgorithm::Zlib`. | Conformant |
| Negotiation exchange | Both sides send vstring lists via `write_vstring()`; first mutual match wins (compat.c:534-570). | `negotiate_capabilities_with_override()` performs bidirectional exchange. | Conformant |
| `RSYNC_COMPRESS_LIST` env | Restricts available algorithms (compat.c:408-423). | Not implemented. | **Gap** |

## 10. Checksum Flag Logic

| Aspect | Upstream | oc-rsync | Status |
|--------|----------|----------|--------|
| `--checksum` (`-c`) | Sets `always_checksum = 1`. Uses file-list checksums for quick-check instead of mtime+size. No mutual exclusions with other flags. | `checksum(true)` in builder; flag pushed as `c` in server args (`flags.rs:67-69`). | Conformant |
| `--checksum-choice=ALGO` | Sets `checksum_choice`, parsed early for `--whole-file` forcing (options.c:1981-1987). Sent as `--checksum-choice ALGO` to server (options.c:2797-2798). | `checksum_choice` field in config; sent as long-form arg. | Conformant |
| Negotiation exchange | Both sides send vstring lists; first mutual match wins (compat.c:540-554). | `negotiate_capabilities_with_override()` handles checksum negotiation. | Conformant |
| `RSYNC_CHECKSUM_LIST` env | Restricts available algorithms (compat.c:408-423). | Not implemented. | **Gap** |
| `--checksum-choice=none` -> whole_file | `xfer_sum_nni->num == CSUM_NONE` forces `whole_file = 1` (checksum.c:197-198). | Not explicitly enforced as a validation step. | **Gap (minor)** |

## 11. Hard-Links Flag Logic

| Aspect | Upstream | oc-rsync | Status |
|--------|----------|----------|--------|
| `-H` | Sets `preserve_hard_links = 1`. Double `-HH` sets to 2 (cross-device). | `hard_links(true)` in builder; flag pushed as `H` in server args (`flags.rs:70-71`). | Conformant |
| Hard-links + inc_recurse | `need_unsorted_flist` set when hard-links active with inc_recurse (options.c, compat.c:788). | Unsorted flist index used for hard-link matching during inc_recurse. | Conformant |

## 12. ACL and Xattr Flags

| Aspect | Upstream | oc-rsync | Status |
|--------|----------|----------|--------|
| `-A` (ACLs) | Requires proto >= 30 for remote; `#ifdef SUPPORT_ACLS` (options.c:2677-2679). | `#[cfg(all(any(unix, windows), feature = "acl"))]` gate; proto restriction enforced. | Conformant |
| `-X` (xattrs) | Requires proto >= 30 for remote; double `-XX` for cross-device (options.c:2681-2686). | `#[cfg(all(unix, feature = "xattr"))]` gate; proto restriction enforced. | Conformant |
| Xattr optimization | `want_xattr_optim` set when proto >= 31 and `!CF_AVOID_XATTR_OPTIM` (compat.c:746). | `AVOID_XATTR_OPTIMIZATION` flag propagated. | Conformant |

## 13. Batch Mode Interactions

| Aspect | Upstream | oc-rsync | Status |
|--------|----------|----------|--------|
| `write_batch` | Forces checksum to `md5` (proto >= 30) or `md4` and compress to `zlib` (compat.c:413-414). | Batch mode support exists in `engine::batch`. Checksum/compress override not explicitly verified against this constraint. | **Gap (minor)** |
| `read_batch` | `do_negotiated_strings = 0` after read (compat.c:785-786). Incompatible inc_recurse in batch -> error (compat.c:768-774). | Batch read path does not re-negotiate strings. Inc_recurse incompatibility check present. | Conformant |
| `'V'` + `write_batch` | Pre-release `'V'` client: `CF_VARINT_FLIST_FLAGS` only set when `!write_batch` (compat.c:738). | `write_compat_flags()` comment notes "write_batch is never true here". The daemon path does not write batches during compat exchange. | Conformant |

## 14. Summary of Gaps

| # | Gap | Severity | Upstream reference | Recommendation |
|---|-----|----------|--------------------|----------------|
| G1 | `--append --whole-file` not rejected | Medium | options.c:2383-2386 | Add validation in `ClientConfigBuilder::validate()`: `if self.append && self.whole_file == Some(true) { return Err(...) }` |
| G2 | `--write-devices` does not imply `inplace=true` | Medium | options.c:2395-2401 | Set `self.inplace = true` in `preservation.rs::write_devices(true)`, or add transitive check in `validate()`. |
| G3 | R4/R5 error message wording | Low | options.c:2411-2412 | Upstream says `--partial-dir` because `delay_updates` silently aliases to `partial_dir = ".~tmp~"`. Consider aligning wording or documenting the intentional deviation. |
| G4 | `RSYNC_COMPRESS_LIST` env not honored | Low | compat.c:408-423 | Implement env var parsing to restrict compression algorithm list. |
| G5 | `RSYNC_CHECKSUM_LIST` env not honored | Low | compat.c:408-423 | Implement env var parsing to restrict checksum algorithm list. |
| G6 | `--checksum-choice=none` does not force `whole_file` | Low | checksum.c:197-198 | Add logic: when negotiated checksum is CSUM_NONE, set whole_file = true. |
| G7 | `--crtimes` without varint flist flags not rejected | Low | compat.c:750-753 | Add explicit check: if `preserve_crtimes && !xfer_flags_as_varint`, emit error. Currently safe because proto 32 peers always have varint. |
| G8 | `--sparse --inplace` workaround `--no-W` not sent | Low | options.c:2940-2941 | When sender, sparse, inplace, and `!whole_file`, send `--no-W` to remote. Only matters for interop with older rsync versions. |
| G9 | Batch write checksum/compress override not enforced | Low | compat.c:413-414 | Verify that batch mode forces MD5 (proto >= 30) / MD4 and zlib compression choices. |

## 15. Test Coverage Assessment

| Area | Tests present | Location |
|------|--------------|----------|
| Capability string building | Yes | `transfer/src/setup/tests.rs` |
| Client info parsing | Yes | `transfer/src/setup/tests.rs` |
| Compat flags exchange | Yes | `transfer/src/setup/tests.rs` |
| Pre-release `'V'` handling | Yes | `transfer/src/setup/tests.rs` |
| Protocol restrictions (proto < 29, < 30) | Yes | `transfer/src/setup/restrictions.rs` (13 tests) |
| Mutual exclusion (R2-R5, R8) | Yes | `core/src/client/config/builder/tests.rs` |
| Mutual exclusion (R1) | No | Missing - `--append --whole-file` accepted |
| Mutual exclusion (R7) | No | Missing - `--write-devices --partial-dir` accepted |
| Delete mode default selection | Yes | `restrictions.rs` tests |
| Server flag string building | Yes | `core/src/client/remote/flags.rs` tests |
| Transfer config builder conflicts | Yes | `transfer/src/config/builder_tests.rs` |

## 16. Recommendations

1. **High priority**: Fix G1 and G2. These are silent acceptance of flag
   combinations that upstream rejects with an error exit. Both are
   straightforward validation additions.

2. **Medium priority**: Fix G4 and G5. The `RSYNC_COMPRESS_LIST` and
   `RSYNC_CHECKSUM_LIST` environment variables are used in CI and
   deployment scripts to constrain algorithm choices. Without them,
   oc-rsync may negotiate an algorithm the admin intended to exclude.

3. **Low priority**: G3 (wording), G6, G7, G8, G9. These are edge cases
   that are unlikely to cause interop failures with modern rsync peers
   (proto 31+). G7 and G8 only matter for very old or mixed-version
   deployments. G6 only matters when `--checksum-choice=none` is
   explicitly selected, which is uncommon.

4. **Test gaps**: Add negative tests for R1 and R7 to the existing
   `cli_validation_matrix` test suite once the fixes land.
