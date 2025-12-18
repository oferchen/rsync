# Upstream Rsync ↔ oc-rsync Terminology Mapping

**Purpose**: Cross-reference between upstream rsync C source and oc-rsync Rust implementation

---

## File/Module Mapping

| Upstream File | oc-rsync Location | Status | Notes |
|---------------|-------------------|--------|-------|
| `main.c` | `crates/core/src/client/run.rs` | ✓ | Orchestration |
| `flist.c` | `crates/walk/` | ⚠️ | Should rename to `flist` |
| `generator.c` | `crates/core/src/server/generator.rs` | ✓ | Matches upstream |
| `sender.c` | `crates/engine/` (partial) | ⚠️ | Mixed with local copy |
| `receiver.c` | `crates/core/src/server/receiver.rs` | ✓ | Matches upstream |
| `match.c` | `crates/checksums/src/rolling/` | ✓ | Rolling checksum |
| `checksum.c` | `crates/checksums/src/strong/` | ✓ | Strong checksums |
| `io.c` | `crates/transport/` | ⚠️ | Could rename to `io` |
| `compat.c` | `crates/protocol/src/compat.rs` | ✓ | Compat flags |
| `clientserver.c` | `crates/daemon/` | ⚠️ | `daemon` clearer than `clientserver` |
| `authenticate.c` | `crates/core/src/auth/` | ✓ | Auth logic |
| `options.c` | `crates/cli/` | ✓ | CLI parsing |
| `log.c` | `crates/logging/` | ✓ | Logging |
| `rsync.h` | `crates/protocol/src/constants.rs` | ✓ | Protocol constants |

---

## Type/Concept Mapping

| Upstream Term | oc-rsync Term | Location | Notes |
|---------------|---------------|----------|-------|
| `file_list` | `FileList` | `crates/walk/` | ✓ Matches |
| `file_struct` | `FileEntry` | `crates/walk/` | Different naming |
| `sum_struct` | `Signature` | `crates/checksums/` | Different naming |
| `map_struct` | `MappedFile` | `crates/engine/` | Different naming |
| `stats` | `TransferStats` | Various | Different naming |
| `flist_ndx_item` | `FileIndex` | `crates/walk/` | Different naming |

---

## Function Mapping (Key Functions)

| Upstream Function | oc-rsync Equivalent | Location |
|-------------------|---------------------|----------|
| `send_file_list()` | `build_file_list()` | `crates/walk/` |
| `recv_file_list()` | `receive_file_list()` | `crates/core/src/server/receiver.rs` |
| `generate_files()` | `Generator::run()` | `crates/core/src/server/generator.rs` |
| `recv_generator()` | `Generator::process_entry()` | `crates/core/src/server/generator.rs` |
| `receive_data()` | `Receiver::receive_data()` | `crates/core/src/server/receiver.rs` |
| `send_files()` | `engine::send_files()` | `crates/engine/` |
| `match_sums()` | `match_blocks()` | `crates/checksums/` |
| `sum_init()` | `RollingChecksum::new()` | `crates/checksums/src/rolling/` |

---

## Constant Mapping

| Upstream Constant | oc-rsync Constant | Location |
|-------------------|-------------------|----------|
| `PROTOCOL_VERSION` | `ProtocolVersion::NEWEST` | `crates/protocol/` |
| `CF_*` flags | `CompatibilityFlags::*` | `crates/protocol/src/compat.rs` |
| `XMIT_*` flags | `TransmitFlags::*` | `crates/walk/` |
| `RERR_*` codes | `*_EXIT_CODE` constants | `crates/core/src/client/error.rs` |
| `MSG_*` tags | `MessageTag::*` | `crates/protocol/` |
| `NDX_*` values | `FileIndex::*` | `crates/walk/` |

---

## Recommended Renames (Priority Order)

### High Priority (Clear Wins)

1. **`walk` → `flist`**
   - Direct 1:1 mapping with upstream
   - Low risk (internal crate, limited external usage)
   - Clear improvement in upstream comprehension

2. **Add `pub use` aliases**
   ```rust
   // In lib.rs
   pub use walk as flist;  // Transitional alias
   ```

### Medium Priority (Consider Benefits)

3. **`FileEntry` → `FileStruct`** (internal)
   - Matches upstream exactly
   - May cause confusion with "struct" keyword

4. **`TransferStats` → `Stats`** (if public)
   - Matches upstream
   - Generic name may clash

### Low Priority (Keep Current)

5. **`daemon` vs `clientserver`**
   - Keep `daemon` - clearer modern terminology
   - Upstream `clientserver.c` is dated naming

6. **`transport` vs `io`**
   - Keep `transport` - clearer intent
   - `io` too generic in Rust ecosystem

---

## Documentation Cross-Reference Format

### Recommended Format

```rust
/// Builds the file list for transmission.
///
/// Constructs a sorted list of files, directories, and special entries
/// for transfer. Mirrors upstream recursive directory scanning.
///
/// # Upstream Reference
///
/// - `flist.c:2192` - `send_file_list()` - Main file list builder
/// - `flist.c:1456` - `send_file_entry()` - Per-file encoding
///
/// # Protocol
///
/// Encodes entries using delta compression against previous entry
/// to minimize wire size. See `crates/protocol/src/wire/flist.rs`.
pub fn build_file_list(root: &Path) -> Result<FileList> {
    // ...
}
```

---

## Next Steps

1. ✅ Audit: Run `rg` queries to find undocumented APIs
2. ⏸️ Rename: Start with `walk` → `flist` 
3. ⏸️ Document: Add upstream references to key functions
4. ⏸️ Clean: Remove TODO/FIXME, convert to issues or remove
