# Quick Wins: Refactoring Tasks

**Priority**: Low-hanging fruit that improves code quality immediately

---

## 1. Add Rustdoc to Branding Functions (30 min)

All branding functions lack documentation:
```rust
// Current
pub fn rust_version() -> &'static str {

// Should be
/// Returns the Rust-based version string.
///
/// Format: "3.4.1-rust"
pub fn rust_version() -> &'static str {
```

**Files**:
- `crates/branding/src/branding/mod.rs`
- `crates/branding/src/branding/profile.rs`

---

## 2. Remove TODO/FIXME Comments (1 hour)

Found 16 TODO/FIXME comments:
```bash
rg "TODO|FIXME|XXX" --type rust crates/
```

**Action**: For each:
- Convert to GitHub issue if actionable
- Remove if obsolete
- Replace with proper documentation if explanatory

---

## 3. Add Upstream References to Key Functions (2 hours)

Add cross-references to major functions:

```rust
/// Builds the file list for transmission.
///
/// # Upstream Reference
///
/// - `flist.c:2192` - `send_file_list()`
/// - Matches recursive directory scanning behavior
pub fn build_file_list(root: &Path) -> Result<FileList> {
```

**Priority Files**:
- `crates/walk/src/builder.rs` → `flist.c`
- `crates/core/src/server/generator.rs` → `generator.c`
- `crates/core/src/server/receiver.rs` → `receiver.c`
- `crates/checksums/src/rolling/mod.rs` → `match.c`

---

## 4. Add `pub use walk as flist` Alias (5 min)

Zero-risk transitional alias:

```rust
// In crates/walk/src/lib.rs or crates/core/src/lib.rs
pub use walk as flist;
```

Provides upstream terminology without breaking changes.

---

## 5. Document Public Enums (30 min)

```rust
// Current
pub enum ProgressSetting {
    Auto,
    Enabled,
    Disabled,
}

// Should be
/// Progress reporting configuration.
///
/// Controls whether and how transfer progress is displayed.
pub enum ProgressSetting {
    /// Auto-detect based on terminal capabilities
    Auto,
    /// Force enable progress reporting
    Enabled,
    /// Disable all progress reporting
    Disabled,
}
```

**Files**:
- `crates/cli/src/frontend/progress/mode.rs`
- `crates/filters/src/action.rs`
- `crates/branding/src/branding/brand.rs`

---

## Implementation Order

1. ✅ Create documentation: `REFACTORING_PLAN.md`, `TERMINOLOGY_MAPPING.md`
2. ⏸️ Add rustdoc to branding (quick win)
3. ⏸️ Document public enums (quick win)
4. ⏸️ Add upstream references to 5 key functions
5. ⏸️ Remove TODO/FIXME comments
6. ⏸️ Add `walk` alias

**Time**: ~4-5 hours total for all quick wins
