# Refactoring Plan: Align with Upstream Rsync Terminology

**Status**: Planning Phase
**Priority**: Medium - Improves maintainability and upstream comprehension
**Impact**: Better alignment with upstream rsync for contributors familiar with C codebase

---

## Goals

1. **Terminology Alignment**: Rename crates, modules, and types to match upstream rsync
2. **Structure Alignment**: Reorganize directories to mirror upstream source organization
3. **Documentation Quality**: Replace inline comments with proper rustdoc
4. **Code Clarity**: Remove unhelpful comments, add useful ones

---

## Phase 1: Audit Current vs Upstream

### Upstream rsync Source Structure

```
rsync/
├── flist.c          # File list building/transmission
├── generator.c      # File generator (drives transfer)
├── sender.c         # Sender role (reads basis, sends deltas)
├── receiver.c       # Receiver role (applies deltas)
├── main.c           # Entry point, role dispatch
├── compat.c         # Compatibility flags
├── options.c        # Command-line parsing
├── clientserver.c   # Daemon protocol
├── authenticate.c   # Daemon authentication
├── match.c          # Block matching (rolling checksum)
├── checksum.c       # Strong checksums (MD4/MD5)
├── io.c             # Multiplexed I/O
├── util.c           # Utilities
├── log.c            # Logging
└── rsync.h          # Protocol constants
```

### Current oc-rsync Crate Structure

```
crates/
├── core/            # ✓ Orchestration (maps to main.c)
│   ├── client/      # ❌ Should align with sender/receiver/generator
│   └── server/      # ✓ Matches upstream server role
├── engine/          # ❌ Non-upstream term (maps to sender.c + match.c)
├── walk/            # ❌ Non-upstream term (maps to flist.c)
├── protocol/        # ✓ Protocol constants (rsync.h)
├── checksums/       # ✓ Maps to checksum.c + match.c
├── filters/         # ✓ Maps to filter.c
├── compress/        # ✓ Maps to compression logic
├── metadata/        # ✓ Maps to metadata handling
├── transport/       # ✓ Maps to I/O layer
├── daemon/          # ✓ Maps to clientserver.c
├── logging/         # ✓ Maps to log.c
├── cli/             # ✓ Maps to options.c
└── bandwidth/       # ✓ Bandwidth limiting
```

### Terminology Gaps

| Current | Upstream | Notes |
|---------|----------|-------|
| `engine` | `sender` + `match` | Should split or rename |
| `walk` | `flist` | Should rename to `flist` |
| `client` | `sender` + `receiver` | Conflates two distinct roles |
| `LocalCopy` | N/A | Local-only optimization, no upstream equivalent |

---

## Phase 2: Proposed Renaming

### Option A: Minimal Impact (Preserve Structure, Add Aliases)

**Pros**: Low risk, backward compatible
**Cons**: Doesn't fully align with upstream

```rust
// Add module aliases
pub use walk as flist;
pub use engine as sender;
```

### Option B: Full Restructure (Match Upstream)

**Pros**: Perfect alignment with upstream
**Cons**: High churn, breaks existing code

```
crates/
├── core/
│   ├── flist/       # Renamed from walk/
│   ├── generator/   # Extracted from server/
│   ├── sender/      # Extracted from engine/
│   ├── receiver/    # Extracted from server/
│   └── match/       # Extracted from engine/ or checksums/
├── protocol/
├── io/              # Renamed from transport/
├── clientserver/    # Renamed from daemon/
└── ...
```

### Option C: Hybrid (Rename Core, Keep Periphery)

**Pros**: Balanced risk/reward
**Cons**: Partial alignment

**Rename**:
- `walk` → `flist`
- `engine::sender` → `sender` (extract to crate?)
- `server::generator` → `generator` (keep as module)
- `server::receiver` → `receiver` (keep as module)

**Keep**:
- `protocol`, `checksums`, `filters`, `compress`, `metadata`
- `daemon` (clearer than `clientserver`)
- `transport` (clearer than `io`)

---

## Phase 3: Documentation Cleanup

### Current Issues

1. **Inline Comments**:
   ```rust
   // This mirrors upstream behavior
   let x = foo();  // FIXME: need to verify
   ```

2. **Missing Rustdoc**:
   ```rust
   pub fn important_function() -> Result<()> {
       // No /// documentation
   }
   ```

3. **Redundant Comments**:
   ```rust
   // Increment counter
   counter += 1;
   ```

### Target State

1. **Proper Rustdoc**:
   ```rust
   /// Builds the file list for transmission.
   ///
   /// Mirrors upstream `flist.c::send_file_list()` behavior by recursively
   /// scanning the source tree and encoding file metadata.
   ///
   /// # Arguments
   ///
   /// * `root` - Source directory root
   ///
   /// # Returns
   ///
   /// Encoded file list on success.
   ///
   /// # Upstream Reference
   ///
   /// - `flist.c:2192` - `send_file_list()`
   pub fn build_file_list(root: &Path) -> Result<FileList> {
       // Implementation
   }
   ```

2. **Upstream Cross-References**:
   ```rust
   /// Applies delta operations to reconstruct the target file.
   ///
   /// # Upstream Reference
   ///
   /// - `receiver.c:340` - `receive_data()`
   /// - Match block-by-block application logic
   ```

3. **Remove Unhelpful Comments**:
   ```diff
   - // Set flag to true
   + // Mirror upstream: disable incremental recursion when --no-inc-recurse specified
     inc_recurse = false;
   ```

---

## Phase 4: Implementation Strategy

### Step 1: Audit (Low Risk)

```bash
# Find all public APIs without rustdoc
rg "^pub (fn|struct|enum|trait)" --type rust | grep -v "///"

# Find TODO/FIXME comments
rg "TODO|FIXME|XXX" --type rust

# Find inline comments that should be rustdoc
rg "^\s+//" --type rust | grep -v "^\s+///"
```

### Step 2: Documentation Pass (Low Risk)

1. Add rustdoc to all public APIs
2. Add upstream cross-references
3. Convert useful inline comments to rustdoc
4. Remove redundant comments

### Step 3: Rename Pass (Medium Risk)

1. Create rename plan with deprecation strategy
2. Add `#[deprecated]` attributes to old names
3. Add type aliases for smooth transition
4. Update all internal references
5. Update documentation

### Step 4: Restructure Pass (High Risk - Optional)

1. Only if full upstream alignment is required
2. Requires coordinated update across all crates
3. Should be done in separate PR with clear migration guide

---

## Phase 5: Success Criteria

- [ ] All public APIs have rustdoc comments
- [ ] All rustdoc includes upstream file/line references where applicable
- [ ] Zero TODO/FIXME comments remain (convert to issues or remove)
- [ ] Key modules renamed to match upstream (walk → flist minimum)
- [ ] CLAUDE.md updated with new structure
- [ ] ARCHITECTURE.md updated with upstream mapping

---

## Timeline Estimate

- **Audit**: 2-4 hours
- **Documentation Pass**: 8-12 hours (depends on API surface)
- **Rename Pass**: 4-6 hours (minimal), 16-24 hours (full)
- **Restructure Pass**: 40+ hours (if pursued)

**Recommended**: Start with Audit + Documentation, defer Rename/Restructure

---

## References

- Upstream rsync source: https://github.com/RsyncProject/rsync
- Current architecture: `docs/ARCHITECTURE.md`
- Agent definitions: `CLAUDE.md`
