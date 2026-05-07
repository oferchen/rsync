# Compatibility Flag (`CF_*`) Audit vs Upstream rsync 3.4.1

This audit reconciles every `CF_*` compatibility bit defined in upstream rsync
3.4.1 (`compat.c`, `rsync.h`) with our implementation in
`crates/protocol/src/compatibility/` and `crates/transfer/src/setup/`. It
enumerates the wire bits, the `-e.<chars>` capability characters that gate
them, the protocol version each was introduced in, and how our code negotiates
and emits them. A side-by-side comparison of `build_our_flags` against the
upstream `compat.c:710-743` block follows, along with the golden-byte tests
that pin the wire encoding.

## Sources Consulted

- `target/interop/upstream-src/rsync-3.4.1/compat.c` (lines 117-125 for `CF_*`
  defines, 572-830 for `setup_protocol`, 710-743 for the flag-build block).
- `target/interop/upstream-src/rsync-3.4.1/rsync.h` (lines 113-149 for
  `PROTOCOL_VERSION`, `MIN_PROTOCOL_VERSION`, `OLD_PROTOCOL_VERSION`,
  `MAX_PROTOCOL_VERSION`, `SUBPROTOCOL_VERSION`).
- `crates/protocol/src/compatibility/{flags.rs,known.rs,iter.rs,mod.rs}`.
- `crates/transfer/src/setup/{capability.rs,compat.rs,mod.rs,restrictions.rs}`.
- `crates/protocol/tests/golden_handshakes.rs`,
  `crates/protocol/tests/compatibility_flags.rs`,
  `crates/protocol/tests/protocol_v32_compat.rs`.

## 1. `CF_*` Bit-by-Bit Matrix

Upstream defines the bits in `compat.c:117-125`. The exchange itself is gated
on `protocol_version >= 30` (`compat.c:710`); pre-30 peers never see a compat
byte. Bits are written by the server only; the client reads them
(`compat.c:736-741`). All known bits live in the low 9 positions; bit 7
forces varint encoding even though every known value fits in a single byte.

| Bit | Macro | Char | Intro proto | Upstream semantics | Our wire constant | Our handling |
|----:|-------|:----:|:-----------:|--------------------|-------------------|--------------|
| 0 | `CF_INC_RECURSE` | `i` | 30 | Sender and receiver both support incremental file-list recursion. Server only sets bit when `allow_inc_recurse` survives the `set_allow_inc_recurse()` filters (`compat.c:161-179`, `compat.c:712`). | `CompatibilityFlags::INC_RECURSE` (`flags.rs:34`) | Capability table row at `setup/capability.rs:42-48` (`requires_inc_recurse: true`). `build_our_flags` sets the bit only when `config.allow_inc_recurse` is true (`setup/mod.rs:233-235`). Client clears the bit on read when not allowed (`setup/mod.rs:121-123`). |
| 1 | `CF_SYMLINK_TIMES` | `L` | 30 | Receiver can apply mtime to symlinks (`CAN_SET_SYMLINK_TIMES`). Upstream sets it unconditionally on Unix (`compat.c:713-715`). | `CompatibilityFlags::SYMLINK_TIMES` (`flags.rs:36`) | Capability row at `capability.rs:50-59` is `platform_ok=cfg(unix)`. SSH client mode sets the bit under `#[cfg(unix)]` (`setup/mod.rs:219-222`). Daemon server mirrors via the table-driven path. |
| 2 | `CF_SYMLINK_ICONV` | `s` | 30 | Symlink contents need iconv translation. Gated on `#ifdef ICONV_OPTION` upstream (`compat.c:716-718`). | `CompatibilityFlags::SYMLINK_ICONV` (`flags.rs:38`) | Capability row at `capability.rs:65-71` is `requires_iconv: true`; advertised and accepted only when the `iconv` cargo feature is on (`iconv_capability_compiled_in()` at `capability.rs:127-129`). SSH client mode mirrors with `#[cfg(all(unix, feature = "iconv"))]` (`setup/mod.rs:228-231`). |
| 3 | `CF_SAFE_FLIST` | `f` | 30 | Receiver supports the safe (recoverable) flist transmission scheme. Forced on for protocol >= 31 (`compat.c:775`). | `CompatibilityFlags::SAFE_FILE_LIST` (`flags.rs:40`) | Capability row at `capability.rs:72-78`. SSH client mode enables it unconditionally (`setup/mod.rs:213`). The protocol >= 31 forcing happens at the protocol layer that consumes the flags. |
| 4 | `CF_AVOID_XATTR_OPTIM` | `x` | 30 | Disables the xattr-on-hardlink fast path. Upstream pairs the bit with `want_xattr_optim = protocol >= 31 && !(compat & CF_AVOID_XATTR_OPTIM)` (`compat.c:746`). | `CompatibilityFlags::AVOID_XATTR_OPTIMIZATION` (`flags.rs:42`) | Capability row at `capability.rs:80-86`. SSH client mode advertises it (`setup/mod.rs:214`). Consumers in `engine`/`metadata` use `compat_flags.contains(...)` to pick the slow path. |
| 5 | `CF_CHKSUM_SEED_FIX` | `C` | 30 | Switches MD5 checksum seed ordering to the post-3.0.0 fix. Upstream stores it as `proper_seed_order` (`compat.c:747`). | `CompatibilityFlags::CHECKSUM_SEED_FIX` (`flags.rs:44`) | Capability row at `capability.rs:88-94`. SSH client mode always sets it (`setup/mod.rs:211`). The checksums crate keys block-checksum seeding off this flag. |
| 6 | `CF_INPLACE_PARTIAL_DIR` | `I` | 30 | When set together with `--inplace`, allows a `--partial-dir` to be respected. Upstream stores it as `inplace_partial` (`compat.c:777-778`). | `CompatibilityFlags::INPLACE_PARTIAL_DIR` (`flags.rs:46`) | Capability row at `capability.rs:96-102`. SSH client mode enables it (`setup/mod.rs:215`). Validation of `--inplace` versus `--partial-dir` happens at config-build time (see `crates/transfer/src/setup/restrictions.rs`). |
| 7 | `CF_VARINT_FLIST_FLAGS` | `v` | 30 (modernised in 3.2.x) | Switches the file-list xfer flag word from a u8 to a varint and enables string-based algorithm negotiation (`do_negotiated_strings`, `compat.c:730-732,742`). | `CompatibilityFlags::VARINT_FLIST_FLAGS` (`flags.rs:48`) | Capability row at `capability.rs:103-109`. SSH client mode always advertises it (`setup/mod.rs:212`). `should_negotiate()` (`setup/mod.rs:251-268`) consults this bit on both sides to gate the vstring exchange. |
| 8 | `CF_ID0_NAMES` | `u` | 31+ (3.2.4) | Sender will transmit names for uid 0 / gid 0 instead of the historical optimisation that omitted them (`compat.c:727-728,749`). | `CompatibilityFlags::ID0_NAMES` (`flags.rs:50`) | Capability row at `capability.rs:111-117`. SSH client mode always advertises it (`setup/mod.rs:216`). Consumed in the file-list crate when serialising user/group strings. |
| 9-31 | (unassigned) | - | - | Future bits are tolerated but ignored upstream (`compat.c:740-743` reads as varint). | `CompatibilityFlags::has_unknown_bits()` / `unknown_bits()` (`flags.rs:84-101`) | `without_unknown_bits()` (`flags.rs:111-114`) clears unknown bits when our daemon needs to forward a sanitised value. The varint reader preserves them so diagnostics keep the original word. |

### Pre-release `'V'` Quirk

Upstream `compat.c:733-738` recognises a deprecated pre-release `'V'`: when the
client advertises `'V'`, the server implicitly ORs in `CF_VARINT_FLIST_FLAGS`
and writes the flag word as a single byte (`write_byte`) rather than a varint
(`write_varint`). We mirror this in `setup/compat.rs:write_compat_flags`
(`compat.rs:37-52`) using `client_has_pre_release_v_flag()`. The pre-release
`'V'` does not appear in `CAPABILITY_MAPPINGS` because we never advertise it
ourselves; we only honour it when a peer sends it.

## 2. Capability-String Character Table (`-e.<chars>`)

Upstream constructs the capability string in `options.c:maybe_add_e_option()`
and parses it from `client_info` in `compat.c:712-732`. We construct it in
`build_capability_string()` (`setup/capability.rs:138-153`) from the
single-source-of-truth `CAPABILITY_MAPPINGS` table at `capability.rs:40-118`.
Order matches upstream for documentation consistency.

| Char | Flag | Compile-time gate | Runtime gate | Source row |
|:----:|------|-------------------|--------------|------------|
| `i` | `CF_INC_RECURSE` | always | `allow_inc_recurse` true | `capability.rs:42-48` |
| `L` | `CF_SYMLINK_TIMES` | `cfg(unix)` (mirrors upstream `CAN_SET_SYMLINK_TIMES`) | none | `capability.rs:50-59` |
| `s` | `CF_SYMLINK_ICONV` | `feature = "iconv"` (mirrors upstream `#ifdef ICONV_OPTION`) | none | `capability.rs:65-71` |
| `f` | `CF_SAFE_FLIST` | always | none | `capability.rs:72-78` |
| `x` | `CF_AVOID_XATTR_OPTIM` | always | none | `capability.rs:80-86` |
| `C` | `CF_CHKSUM_SEED_FIX` | always | none | `capability.rs:88-94` |
| `I` | `CF_INPLACE_PARTIAL_DIR` | always | none | `capability.rs:96-102` |
| `v` | `CF_VARINT_FLIST_FLAGS` | always | none | `capability.rs:103-109` |
| `u` | `CF_ID0_NAMES` | always | none | `capability.rs:111-117` |
| `V` | (legacy `CF_VARINT_FLIST_FLAGS`) | never advertised | accepted from peer only | `capability.rs:263-265` |

The Unix-side advertisement string we emit matches upstream's typical 3.4.1
output exactly: `-e.LsfxCIvu` (or `-e.LfxCIvu` on builds without `iconv`).
Adding `'i'` for receiver-direction transfers produces `-e.iLsfxCIvu`, which
is the form upstream sends for inc-recurse-eligible pulls.

The parser at `parse_client_info()` (`capability.rs:183-208`) handles all
three syntaxes upstream emits: `-e foo`, `-efoo`, and combined short option
forms such as `-vvde.LsfxCIvu` where the leading `.` is a version placeholder
(`options.c` writes the dot when `protocol_version != PROTOCOL_VERSION`).

## 3. `INC_RECURSE` (`'i'`) Specifics

`set_allow_inc_recurse()` upstream (`compat.c:161-179`) clears
`allow_inc_recurse` when:

- `!recurse || use_qsort`, or
- on the receiver side, any of `delete_before`, `delete_after`,
  `delay_updates`, `prune_empty_dirs` is in effect, or
- in server mode, `client_info` does not contain `'i'`.

In our code, the same conditions are validated at config-build time in
`crates/core/src/client/config/builder/` (mutual-exclusion checks for
`--delay-updates`, `--prune-empty-dirs`, etc.) and the surviving
`allow_inc_recurse` boolean is fed into:

1. `build_capability_string(allow_inc_recurse)` (`capability.rs:138`) - when
   false, the `'i'` row is skipped so we never advertise the capability.
2. `build_compat_flags_from_client_info(client_info, allow_inc_recurse)`
   (`capability.rs:222-250`) - daemon server side never sets the bit if the
   peer's `client_info` is missing `'i'`.
3. `build_our_flags(...)` (`setup/mod.rs:197-239`) - SSH/client side ORs in
   `CF_INC_RECURSE` only when `allow_inc_recurse` is true.
4. The client-side read path masks `INC_RECURSE` off when our local
   `allow_inc_recurse` is false even if the server set it (`mod.rs:121-123`),
   matching upstream's batch-time defensive check at `compat.c:768-774`.

A note from project memory: receiver-side INC_RECURSE is fully implemented and
tested; sender-side push transfers historically passed `!is_sender` to gate
the `'i'` advertisement. The sender direction was enabled by default in
60e83fd96 / 39d47722b once interop validation passed; the receiver-side
guarding above is unaffected.

## 4. `VARINT_FLIST_FLAGS` (`'v'`) Specifics

Upstream `compat.c:729-743`:

```c
if (strchr(client_info, 'v') != NULL) {
    do_negotiated_strings = 1;
    compat_flags |= CF_VARINT_FLIST_FLAGS;
}
if (strchr(client_info, 'V') != NULL) { /* pre-release 'V' */
    if (!write_batch)
        compat_flags |= CF_VARINT_FLIST_FLAGS;
    write_byte(f_out, compat_flags);
} else
    write_varint(f_out, compat_flags);
```

Three observable consequences:

1. **String negotiation gate.** `do_negotiated_strings` controls whether the
   subsequent checksum/compression vstrings are exchanged. Our equivalent is
   `should_negotiate()` (`setup/mod.rs:251-268`): server side checks
   `client_info.contains('v') || has_pre_release_v_flag(info)`; client side
   checks `peer_flags.contains(CF_VARINT_FLIST_FLAGS)` on the bit it just read.
2. **File-list flag width.** `xfer_flags_as_varint = compat_flags &
   CF_VARINT_FLIST_FLAGS ? 1 : 0` (`compat.c:748`) decides whether the
   per-entry flag word is a single byte or a varint. The protocol crate's
   file-list reader keys off `compat_flags.contains(VARINT_FLIST_FLAGS)`.
3. **Encoding asymmetry for pre-release `'V'`.** A peer that sent `'V'`
   forces single-byte emission. `write_compat_flags` at `setup/compat.rs:37-52`
   writes `&[flags.bits() as u8]` in that path versus `write_varint` for the
   regular case. The high bit of the stored flag word is bit 7
   (`CF_VARINT_FLIST_FLAGS = 0x80`); a single `0x80` byte would normally be a
   varint continuation byte, so this special case is what lets a peer that
   only knows pre-release encoding still parse our flag word.

The wire encoding is exercised by `golden_compat_flags_full_modern`
(`golden_handshakes.rs:622-642`) which pins `INC_RECURSE | SYMLINK_TIMES |
SAFE_FILE_LIST | CHECKSUM_SEED_FIX | VARINT_FLIST_FLAGS | ID0_NAMES` (`0x1AB`)
to the byte sequence `[0x81, 0xAB]`, and by `golden_compat_flags_all_known`
which pins `0x1FF` to `[0x81, 0xFF]`.

## 5. `build_our_flags` vs Upstream Side-by-Side

Upstream (`compat.c:711-732`, server side only):

```c
if (am_server) {
    compat_flags = allow_inc_recurse ? CF_INC_RECURSE : 0;
#ifdef CAN_SET_SYMLINK_TIMES
    compat_flags |= CF_SYMLINK_TIMES;
#endif
#ifdef ICONV_OPTION
    compat_flags |= CF_SYMLINK_ICONV;
#endif
    if (strchr(client_info, 'f') != NULL)
        compat_flags |= CF_SAFE_FLIST;
    if (strchr(client_info, 'x') != NULL)
        compat_flags |= CF_AVOID_XATTR_OPTIM;
    if (strchr(client_info, 'C') != NULL)
        compat_flags |= CF_CHKSUM_SEED_FIX;
    if (strchr(client_info, 'I') != NULL)
        compat_flags |= CF_INPLACE_PARTIAL_DIR;
    if (strchr(client_info, 'u') != NULL)
        compat_flags |= CF_ID0_NAMES;
    if (strchr(client_info, 'v') != NULL) {
        do_negotiated_strings = 1;
        compat_flags |= CF_VARINT_FLIST_FLAGS;
    }
    ...
}
```

Ours (`crates/transfer/src/setup/mod.rs:197-239`):

```rust
fn build_our_flags<'a>(
    config: &ProtocolSetupConfig<'a>,
    negotiator: &dyn ProtocolNegotiator,
) -> (CompatibilityFlags, Option<std::borrow::Cow<'a, str>>) {
    if let Some(args) = config.client_args {
        // Daemon server mode: parse client capabilities from -e option
        // upstream: compat.c:712-732
        let client_info = negotiator.parse_client_info(args);
        let flags = negotiator.build_flags_from_client_info(&client_info, config.allow_inc_recurse);
        (flags, Some(client_info))
    } else {
        // SSH/client mode
        let mut flags = CompatibilityFlags::CHECKSUM_SEED_FIX
            | CompatibilityFlags::VARINT_FLIST_FLAGS
            | CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
            | CompatibilityFlags::INPLACE_PARTIAL_DIR
            | CompatibilityFlags::ID0_NAMES;
        #[cfg(unix)]
        {
            flags |= CompatibilityFlags::SYMLINK_TIMES;
        }
        #[cfg(all(unix, feature = "iconv"))]
        {
            flags |= CompatibilityFlags::SYMLINK_ICONV;
        }
        if config.allow_inc_recurse {
            flags |= CompatibilityFlags::INC_RECURSE;
        }
        (flags, None)
    }
}
```

| Concern | Upstream | Ours | Status |
|---------|----------|------|--------|
| `CF_INC_RECURSE` set | `allow_inc_recurse ? CF_INC_RECURSE : 0` | `if config.allow_inc_recurse { flags |= INC_RECURSE; }` (SSH path) and table-driven (`requires_inc_recurse: true`) on daemon path | Match |
| `CF_SYMLINK_TIMES` gate | `#ifdef CAN_SET_SYMLINK_TIMES` (Unix-only) | `#[cfg(unix)]` for SSH path; `platform_ok = cfg(unix)` for daemon path | Match |
| `CF_SYMLINK_ICONV` gate | `#ifdef ICONV_OPTION` | `#[cfg(all(unix, feature = "iconv"))]` for SSH; `requires_iconv = true` filtered by `iconv_capability_compiled_in()` for daemon | Match |
| Char-driven bits (`f`, `x`, `C`, `I`, `u`, `v`) | Six explicit `strchr` checks | Single table iteration in `build_compat_flags_from_client_info()` (`capability.rs:222-250`) | Match (semantically equivalent and shorter) |
| `do_negotiated_strings` side-effect | Set inline when `'v'` is present | Computed by `should_negotiate()` (`mod.rs:251-268`) after the flag bit is built | Match (decision is the same; side-effect lifted out for testability) |
| Pre-release `'V'` byte encoding | `write_byte` | `writer.write_all(&[bits as u8])` in `compat.rs:write_compat_flags` | Match |
| Default-byte encoding | `write_varint` | `protocol::write_varint` in same path | Match |
| Client-side read | `read_varint` | `negotiator.read_compat_flags()` -> `CompatibilityFlags::read_from()` -> `read_varint` | Match |
| Client-side `INC_RECURSE` defensive clear | Implicit via `if (inc_recurse && !allow_inc_recurse)` abort (`compat.c:768-774`) | Explicit `flags &= !CompatibilityFlags::INC_RECURSE` when `!config.allow_inc_recurse` (`mod.rs:121-123`) | Stricter: we silently mask instead of aborting. Acceptable because it preserves upstream's "the bit must not survive when not allowed" invariant without forcing a transfer abort on corner-case batches. |
| SSH-direction defaults | (no equivalent; upstream never executes the `else` branch on a non-server) | We seed sensible defaults so an SSH/client-mode build still emits the same `-e` capability set the daemon would | Match (additive; the SSH code path is exercised in our crates only). |

### Per-character upstream-vs-ours table

| Char | Upstream code | Our code (daemon path) | Our code (SSH path) |
|:----:|---------------|------------------------|---------------------|
| `i` | `compat_flags = allow_inc_recurse ? CF_INC_RECURSE : 0;` | `requires_inc_recurse: true` row in `CAPABILITY_MAPPINGS` filtered when `allow_inc_recurse=false` | `if config.allow_inc_recurse { flags |= INC_RECURSE; }` |
| `L` | `#ifdef CAN_SET_SYMLINK_TIMES compat_flags |= CF_SYMLINK_TIMES;` | `platform_ok = cfg(unix)` | `#[cfg(unix)] flags |= SYMLINK_TIMES;` |
| `s` | `#ifdef ICONV_OPTION compat_flags |= CF_SYMLINK_ICONV;` | `requires_iconv: true` filtered by `iconv_capability_compiled_in()` | `#[cfg(all(unix, feature = "iconv"))] flags |= SYMLINK_ICONV;` |
| `f` | `if (strchr(client_info, 'f')) compat_flags |= CF_SAFE_FLIST;` | table iteration / `client_info.contains('f')` | Always `flags |= SAFE_FILE_LIST;` |
| `x` | `if (strchr(client_info, 'x')) compat_flags |= CF_AVOID_XATTR_OPTIM;` | table iteration / `client_info.contains('x')` | Always `flags |= AVOID_XATTR_OPTIMIZATION;` |
| `C` | `if (strchr(client_info, 'C')) compat_flags |= CF_CHKSUM_SEED_FIX;` | table iteration / `client_info.contains('C')` | Always `flags |= CHECKSUM_SEED_FIX;` |
| `I` | `if (strchr(client_info, 'I')) compat_flags |= CF_INPLACE_PARTIAL_DIR;` | table iteration / `client_info.contains('I')` | Always `flags |= INPLACE_PARTIAL_DIR;` |
| `v` | `if (strchr(client_info, 'v')) { do_negotiated_strings = 1; compat_flags |= CF_VARINT_FLIST_FLAGS; }` | table iteration / `client_info.contains('v')`; `do_negotiated_strings` derived in `should_negotiate()` | Always `flags |= VARINT_FLIST_FLAGS;` |
| `u` | `if (strchr(client_info, 'u')) compat_flags |= CF_ID0_NAMES;` | table iteration / `client_info.contains('u')` | Always `flags |= ID0_NAMES;` |
| `V` | `if (strchr(client_info, 'V')) { compat_flags |= CF_VARINT_FLIST_FLAGS; write_byte(...); }` | `client_has_pre_release_v_flag()` in `setup/compat.rs:42-47` | n/a (never advertised) |

## 6. Test Coverage (Golden Byte Handshakes)

The wire encoding is locked down by golden-byte tests so any drift from
upstream becomes a test failure rather than a runtime interop bug.

### Direct golden-byte coverage in `crates/protocol/tests/golden_handshakes.rs`

| Test | Purpose | Pinned bytes |
|------|---------|--------------|
| `golden_compat_flags_empty` (line 564) | Zero flags varint | `[0x00]` |
| `golden_compat_flags_inc_recurse` (line 577) | `CF_INC_RECURSE` only | `[0x01]` |
| `golden_compat_flags_safe_flist` (line 589) | `CF_SAFE_FLIST` only | `[0x08]` |
| `golden_compat_flags_typical_server` (line 601) | `INC_RECURSE | SYMLINK_TIMES | SAFE_FLIST | CHKSUM_SEED_FIX` (= 0x2B) | `[0x2B]` |
| `golden_compat_flags_full_modern` (line 621) | Modern set including `VARINT_FLIST_FLAGS` (= 0x1AB) | `[0x81, 0xAB]` |
| `golden_compat_flags_all_known` (line 644) | `ALL_KNOWN` (= 0x1FF) | `[0x81, 0xFF]` |

### Per-bit encoding pin-down in `crates/protocol/tests/compatibility_flags.rs`

Each `CF_*` bit has a paired `bits()`-position assertion and a varint encoding
assertion (lines 50-149):

- `CF_INC_RECURSE` (bit 0) -> `[1]`
- `CF_SYMLINK_TIMES` (bit 1) -> `[2]`
- `CF_SYMLINK_ICONV` (bit 2) -> `[4]`
- `CF_SAFE_FLIST` (bit 3) -> `[8]`
- `CF_AVOID_XATTR_OPTIM` (bit 4) -> `[16]`
- `CF_CHKSUM_SEED_FIX` (bit 5) -> `[32]`
- `CF_INPLACE_PARTIAL_DIR` (bit 6) -> `[64]`
- `CF_VARINT_FLIST_FLAGS` (bit 7) -> `[128, 128]` (two-byte varint because bit 7 is the continuation bit)
- `CF_ID0_NAMES` (bit 8) -> `[129, 0]`

### Other golden-style coverage

- `protocol_v32_compat.rs` exercises `KnownCompatibilityFlag::name()` for
  every `CF_*` identifier (lines 337-358) and verifies bit positions
  (lines 304-308 onward), giving us a cross-check that the rust enum stays
  byte-for-byte aligned with the C macros.
- `compatibility_flags.rs:224` (`test_known_compatibility_flag_enum_all_array`)
  pins the `KnownCompatibilityFlag::ALL` ordering, which is the iteration
  order callers rely on for log output and diagnostics.
- `protocol_feature_gates.rs` and `protocol_v30_compat.rs` cover the
  protocol-version gating that wraps the entire compat-flag exchange (no
  exchange at protocol < 30, full exchange at protocol >= 30).

### Coverage gaps observed

- We do not yet have a golden test for the pre-release `'V'` single-byte
  encoding path. The behaviour is exercised by unit tests in
  `setup/mod.rs::tests` but not pinned to a literal byte sequence the way the
  varint path is.
- There is no explicit test asserting that an SSH/client-mode build emits
  `-e.LsfxCIvu` (Unix + iconv) versus `-e.LfxCIvu` (Unix without iconv) versus
  `-e.fxCIvu` (non-Unix). The behaviour is implied by the `CAPABILITY_MAPPINGS`
  table and its compile-time gates, but a string-level golden for each
  feature-flag combination would catch accidental table reorderings.

Both gaps are tractable additions if interop drift is observed in the field.

## Summary

Every `CF_*` bit defined in upstream rsync 3.4.1 has a 1:1 wire-compatible
mapping in `CompatibilityFlags`, a 1:1 capability character in
`CAPABILITY_MAPPINGS`, and explicit golden-byte coverage. The compile-time
gates on `CF_SYMLINK_TIMES` (`cfg(unix)`) and `CF_SYMLINK_ICONV`
(`feature = "iconv"`) match upstream's `#ifdef CAN_SET_SYMLINK_TIMES` and
`#ifdef ICONV_OPTION`. The pre-release `'V'` byte-encoding quirk is honoured
on the receive path even though we never advertise it ourselves. Our
`build_our_flags` is structurally rearranged for testability (table-driven
daemon path, defaulted SSH path) but produces the same final flag word as
upstream for every input combination.
