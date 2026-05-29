# FileEntry Field-Level Compaction Audit (RSS-A.12)

Field-by-field audit of the current `FileEntry` and `FileEntryExtras` structs,
listing incremental compaction wins that apply regardless of whether the flat
backing-store redesign (RSS-A.4) ever lands. This complements
[`file-entry-layout-audit.md`](file-entry-layout-audit.md) (RSS-A.2), which
covers the whole-struct size, padding, and the upstream `file_struct`
comparison. This doc goes one level deeper: it evaluates each field's current
in-memory representation against its real value range and proposes a tighter
representation, with per-field byte savings and a risk note. It feeds the
implementation task RSS-A.3.

Scope is deliberately narrow: only representation changes that fit inside the
existing `Vec<FileEntry>` model. No arena (RSS-7), no flat segment buffer
(RSS-A.4). All sizes are for 64-bit targets.

Source files:
- `crates/protocol/src/flist/entry/core.rs` - `FileEntry`
- `crates/protocol/src/flist/entry/extras.rs` - `FileEntryExtras`
- `crates/protocol/src/flist/entry/accessors.rs` - field accessors/mutators
- `crates/protocol/src/flist/flags.rs` - `FileFlags`
- `crates/protocol/src/flist/wire_mode.rs` - mode wire conversion
- upstream: `rsync-3.4.1/rsync.h:801` - `struct file_struct`

## Decoupling of in-memory representation from the wire

Every candidate below is an in-memory representation change. None of them alters
bytes on the wire on their own, because the flist reader/writer already converts
between the in-memory fields and the wire encoding:

- `mode` passes through `to_wire_mode()` / `from_wire_mode()` (returning `i32`),
  so the in-memory width is independent of the varint emitted on the wire.
- `size` is varint-encoded on the wire; the in-memory width only bounds the
  values that can be held, not the encoding.
- `uid`/`gid` presence is driven by the `XMIT_SAME_UID`/`XMIT_SAME_GID` flag
  logic, not by the in-memory `Option` discriminant.

The risk for the scalar-narrowing candidates is therefore value-range
truncation and cross-platform correctness, not wire divergence. The one
candidate that does touch wire-relevant state is called out explicitly.

## FileEntry inline fields

| Field | Current repr / size | Proposed repr | Bytes saved | Risk |
|---|---|---:|---:|---|
| `uid` | `Option<u32>` = 8 B (no niche: 4 B tag + 4 B payload) | raw `u32` + 1 presence bit in a packed flags byte (sentinel-free) | 4 | Low. Presence already tracked by `XMIT_SAME_UID`; accessor returns `Option<u32>` unchanged. No wire impact. |
| `gid` | `Option<u32>` = 8 B (no niche) | raw `u32` + 1 presence bit | 4 | Low. Same as `uid`. |
| `mode` | `u32` = 4 B | `u16` = 2 B (upstream `file_struct.mode` is `uint16`) | 2 | Medium. POSIX type+perm bits fit in 16 bits, but verify no platform sets bits above 0xFFFF (Windows native bits are normalized in `wire_mode.rs`). No wire impact (converter takes `u32`/`i32`). |
| `mtime_nsec` | `u32` = 4 B, inline | move behind a presence bit into `extras` (most transfers omit sub-second mtime) | 4 (when unused) | Low-Medium. Common case (no nsec) saves 4 B; nsec transfers pay the existing extras allocation. Accessor returns `0` when absent, matching current Default. No wire impact. |
| `content_dir` | `bool` = 1 B | single bit in the packed flags byte | 1 (absorbed into padding) | Low. Pure in-memory bit; wire still derives `XMIT_NO_CONTENT_DIR` from it. |
| `flags` | `FileFlags` = 3 B (primary/extended/extended16, align 1) | keep 3 distinct flag bytes; do NOT collapse to `u16` | 0 | High if narrowed. `extended16` carries bit 16/17 (`XMIT_CRTIME_EQ_MTIME`), so a `u16` would drop live wire bits. Listed to correct RSS-A.2 rec #5: the byte is real, not padding. |
| `size` | `u64` = 8 B | keep `u64` inline (do NOT split to `u32` + extras here) | 0 | Out of scope. Upstream's `len32` + `FLAG_LENGTH64` split is a flat-store concern (RSS-A.4); inside `Vec<FileEntry>` the saved 4 B is reclaimed by padding and adds a branch on every size read. |
| `name` | `PathBuf` = 24 B + heap | unchanged here | 0 | Out of scope. Owned by the arena/flat-store track (RSS-7 / RSS-A.4). |
| `dirname` | `Arc<Path>` = 16 B (fat ptr) | unchanged here | 0 | Out of scope. Thin-index replacement is a flat-store concern (RSS-A.4). |

The `uid`, `gid`, `content_dir`, and the relocated `mtime_nsec` presence bit all
fold into one shared presence-flags byte that already fits in the struct's tail
padding region, so they cost no extra inline bytes.

## FileEntryExtras fields

`FileEntryExtras` is boxed behind `Option<Box<...>>` and only allocated when a
rarely-used field is set, so these savings shrink the heap block (currently
~224 B per RSS-A.2), not the common-case inline footprint. They still matter for
hardlink-heavy, device-heavy, ACL, and xattr transfers.

| Field | Current repr / size | Proposed repr | Bytes saved | Risk |
|---|---|---:|---:|---|
| `rdev_major` | `Option<u32>` = 8 B (no niche) | raw `u32` + presence bit | 4 | Low. Devices set major+minor together; presence bit shared with `rdev_minor`. No wire impact. |
| `rdev_minor` | `Option<u32>` = 8 B (no niche) | raw `u32` + presence bit | 4 | Low. Same presence bit as `rdev_major`. |
| `hardlink_idx` | `Option<u32>` = 8 B (no niche) | raw `u32` + presence bit | 4 | Low. Sentinel-free presence; accessor unchanged. No wire impact. |
| `acl_ndx` | `Option<u32>` = 8 B (no niche) | raw `u32` + presence bit | 4 | Low. Index into ACL list; presence already gated by `--acls`. |
| `def_acl_ndx` | `Option<u32>` = 8 B (no niche) | raw `u32` + presence bit | 4 | Low. Directory-only; same scheme. |
| `xattr_ndx` | `Option<u32>` = 8 B (no niche) | raw `u32` + presence bit | 4 | Low. Index into xattr list; gated by `--xattrs`. |
| `hardlink_dev` | `Option<i64>` = 16 B (no niche) | raw `i64` + presence bit | 8 | Low-Medium. Protocol < 30 only; 0 is a valid dev so a real presence bit is required (not a sentinel). No wire impact. |
| `hardlink_ino` | `Option<i64>` = 16 B (no niche) | raw `i64` + presence bit | 8 | Low-Medium. Protocol < 30 only; 0 is a valid inode, so presence bit required. No wire impact. |
| `atime` | `i64` = 8 B (raw, 0 = absent) | unchanged | 0 | Already Option-free. No change needed. |
| `crtime` | `i64` = 8 B (raw, 0 = absent) | unchanged | 0 | Already Option-free. No change needed. |
| `atime_nsec` | `u32` = 4 B (raw) | unchanged | 0 | Already Option-free. No change needed. |
| `link_target` | `Option<PathBuf>` = 24 B (niche) | unchanged | 0 | Already exploits the `NonNull` niche - no tag overhead. |
| `user_name` | `Option<String>` = 24 B (niche) | unchanged | 0 | Already niched. |
| `group_name` | `Option<String>` = 24 B (niche) | unchanged | 0 | Already niched. |
| `checksum` | `Option<Vec<u8>>` = 24 B (niche) | unchanged | 0 | Already niched. Inline `[u8; N]` would touch the wire/checksum-length contract - out of scope. |
| `xattr_list` | `Option<XattrList>` = 24 B (niche) | unchanged | 0 | Already niched (wraps a `Vec`). |

The eight non-niche `Option` fields collapse into raw values plus a shared
presence bitfield. A single `u16` presence word covers all eight bits with room
to spare and fits in the existing 4 B tail padding of the extras block, so it
adds no net bytes.

## Safe wins vs risky wins

Safe (no wire impact, no truncation risk, accessor signatures preserved):

- `uid`, `gid` -> raw `u32` + presence bit: 8 B inline.
- `content_dir` -> presence bit: 1 B inline (absorbed into padding).
- Extras `Option<u32>` x6 -> raw + presence: 24 B in the heap block.
- Extras `Option<i64>` x2 -> raw + presence: 16 B in the heap block.

Lower-confidence (need a verification step before implementing):

- `mode` -> `u16`: 2 B inline, but requires confirming no live platform sets
  mode bits above 0xFFFF after `from_wire_mode()` normalization.
- `mtime_nsec` -> behind a presence bit in extras: 4 B inline in the common
  (no-nsec) case, at the cost of pushing nsec into the boxed block.

Do not pursue under RSS-A.3:

- Collapsing `FileFlags` to `u16` (drops the live `extended16` byte, bit 16/17).
- Splitting `size` to `u32` + extras (a flat-store concern; padding reclaims the
  inline byte saving inside `Vec<FileEntry>` and adds a hot-path branch).
- Touching `name` / `dirname` (arena / flat-store track, RSS-7 and RSS-A.4).

## Estimated savings for the safe subset

Inline (`FileEntry`), per entry, every transfer:

- `uid` + `gid`: 8 B.
- `content_dir`: folds into the shared presence byte, reclaiming 1 B that
  currently rounds up to padding.

Net inline saving for the safe subset: **8 B per entry** that is realized in
struct size (96 B -> 88 B after re-rounding to 8-byte alignment), plus the
`content_dir` bit which is absorbed without changing the rounded total. At 1M
files this is roughly 8 MB of inline footprint.

Adding the lower-confidence `mode` (2 B) and `mtime_nsec` (4 B) candidates would
bring the inline field savings to 14 B raw, which after 8-byte re-rounding lands
the struct near 80 B - consistent with the 80-88 B target sketched in RSS-A.2's
RSS-A.3 recommendation.

Heap (`FileEntryExtras`), per allocated extras block:

- Six `Option<u32>`: 24 B.
- Two `Option<i64>`: 16 B.

Net extras saving for the safe subset: **40 B per extras block** (~224 B ->
~184 B), realized only on entries that allocate extras (symlinks, devices,
hardlinks, ACLs, xattrs).

## Candidate count

- 4 safe inline candidates (`uid`, `gid`, `content_dir`, plus the shared
  presence byte they fold into).
- 8 safe extras candidates (six `Option<u32>`, two `Option<i64>`).
- 2 lower-confidence inline candidates (`mode`, `mtime_nsec`).
- 3 explicitly-rejected candidates documented to prevent re-litigation
  (`FileFlags` u16, `size` split, `name`/`dirname`).
