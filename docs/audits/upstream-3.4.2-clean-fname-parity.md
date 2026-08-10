# `clean_fname` buffer-underflow parity vs rsync 3.4.2

Tracking issue: #2227. Last verified 2026-05-15 against origin/master.

## 1. Upstream fix

Upstream changed `util1.c:clean_fname()` between 3.4.1 and 3.4.2 to remove
a buffer underflow on pathological inputs.

Before (`rsync-3.4.1/util1.c:945` and `:990-991`):

```c
char *limit = name - 1, *t = name, *f = name;
...
while (s > limit && *--s != '/') {}
if (s != t - 1 && (s < name || *s == '/')) {
    t = s + 1;
```

After (`rsync-3.4.2/util1.c:945` and `:990-996`):

```c
char *limit = name, *t = name, *f = name;
...
/* backing up for ".." - avoid reading before 'name' */
while (s > limit && s[-1] != '/')
    s--;

/* If found prior '/', or we reached the start, adjust t. */
if (s != t - 1 && (s <= name || *s == '/')) {
    t = (s == name) ? name : s + 1;
```

Two undefined-behaviour points were removed:

1. `limit = name - 1` formed a pointer one byte before the allocation,
   forbidden by the C standard even without dereference.
2. `*--s` dereferenced `s` after pre-decrement; for `s == name` the read
   reached one byte before the buffer. The new form checks `s[-1]`
   *only after* the `s > limit` guard, so when `s == name` the read is
   skipped entirely.

The `s == name` branch is now folded back into `t = name`, leaving the
already-collapsed prefix intact instead of writing `s + 1` (which would
overwrite the first byte that was supposed to stay).

Trigger inputs: relative paths whose leading sequence collapses a `..`
that has nothing left to back over - e.g. `a/../../b`, `./../x`, or any
input where the prior loop has already emptied `t` down to `name` while
`CFN_COLLAPSE_DOT_DOT_DIRS` is set. Pre-fix the dereference past `name`
is silent on most platforms (an in-bounds neighbour byte) but is a real
UB and surfaces under ASan / hardened allocators.

## 2. oc-rsync surface area

oc-rsync has three byte-slice routines that perform the work
`clean_fname()` does in upstream, plus several `std::path::Component`
walkers that share the same goal but cannot underflow because they
iterate, not pointer-scan.

### 2.1 `clean_and_validate_name` - `CFN_REFUSE_DOT_DOT_DIRS`

`crates/protocol/src/flist/read/name.rs:149-242`. Mirrors
`util1.c:943` with `CFN_REFUSE_DOT_DOT_DIRS`. **Never collapses `..`**
- it returns an error instead. The function uses `usize` indices into
a `Vec<u8>` and `name.get(i + n)` for look-ahead. No backwards walk
exists, so the upstream bug class cannot occur. Verdict: **SAFE**.

### 2.2 `sanitize_path_with_depth` - `CFN_COLLAPSE_DOT_DOT_DIRS` analogue

`crates/transfer/src/sanitize_path.rs:58-140`. Mirrors
`util1.c:1039 sanitize_path()` which itself follows the same
backwards-walk pattern. The backwards walk is implemented as:

```rust
while result.len() > start && result.last() != Some(&b'/') {
    result.pop();
}
```

`result` is a `Vec<u8>`, `start` is a `usize` (always `<= result.len()`),
and the guard runs before each `pop()`. There is no pointer arithmetic,
no `result.len() - 1` indexing without a length check, and the empty-Vec
branch (`result.last() == None`) is harmless because `len() > start`
implies `len() >= 1`. Verdict: **SAFE**.

### 2.3 `has_mid_path_dotdot` - leading `../` skip

`crates/flist/src/symlink_safety.rs:115-140` and the sibling copy at
`crates/transfer/src/symlink_safety.rs:111-136`. Skips leading `../`
segments before searching for `/../`. The loop uses `pos + 2 <
target.len()` as the upper guard for every byte read; the trailing
slash-skip loop uses `pos < target.len()`. No backwards walk. Verdict:
**SAFE**.

### 2.4 `Path::components()` walkers

The remaining `..` consumers iterate `std::path::Component` values
returned by `Path::components()` and operate on `Vec<OsString>` or
`Vec<PathBuf>` segments. They cannot underflow because the iterator
never yields a position prior to the path root:

| Site | Purpose |
|------|---------|
| `crates/core/src/message/source.rs:285-342` | Diagnostic path normalization in error messages |
| `crates/transfer/src/receiver/quick_check.rs:262-264` | `..`-component rejector for the receiver |
| `crates/flist/src/symlink_safety.rs:68-103` | Symlink depth budget computation |
| `crates/transfer/src/symlink_safety.rs:64-100` | Symlink depth budget (sibling crate) |
| `crates/engine/src/local_copy/operands.rs:50-60` | Operand path translation |
| `crates/engine/src/local_copy/executor/special/symlink.rs:40-92` | Symlink target safety |
| `crates/engine/src/local_copy/overrides.rs:118-152` | Windows device-id derivation |

Verdict: **SAFE** for all sites.

## 3. Pathological input coverage

The pre-existing test suite in `crates/transfer/src/sanitize_path.rs`
already exercises every trigger documented in the upstream commit
message:

- `..` alone (`only_dotdot_becomes_dot`)
- Repeated leading `..` (`many_dotdots_becomes_dot`,
  `multiple_dotdot_at_start_dropped`)
- `..` against an empty accumulator (`absolute_dotdot_traversal_blocked`,
  `dotdot_cannot_escape_root`)
- Mixed `./` and `../` interleaving (`mixed_dot_and_dotdot`,
  `files_from_mixed_traversal`)
- Empty input (`empty_path_becomes_dot`)
- Root-only input (`root_only_becomes_dot`)
- Triple-slash collapse (`triple_slashes_collapsed`)
- Deep traversal beyond budget (`deep_dotdot_traversal_blocked`)

All pass under `cargo nextest` and `cargo miri test` (the latter would
flag any out-of-bounds slice access the pure-Rust code might attempt).

## 4. Verdict summary

| Path | Verdict |
|------|---------|
| `clean_and_validate_name` (REFUSE mode) | SAFE - no backwards walk |
| `sanitize_path_with_depth` (COLLAPSE mode) | SAFE - `Vec` + length-guarded pop |
| `has_mid_path_dotdot` | SAFE - forward scan with bounds checks |
| `Path::components()` walkers | SAFE - iterator cannot underflow |

oc-rsync is **not affected by the 3.4.2 `clean_fname` underflow**. No
remediation required. The structural reason is that Rust forbids the
two upstream UB patterns: `name - 1` would need raw-pointer arithmetic
in an `unsafe` block, and `*--s` past the start of a slice would panic
on the bounds-check before causing memory damage.

## 5. Future-proofing notes

- Keep all `..`-collapse logic on `Vec<u8>` / `&[u8]` with `usize`
  indices. Never reintroduce pointer arithmetic via `unsafe`.
- When porting future `util1.c` changes, mirror the new form
  (`s[-1]` after `s > limit`) directly: in Rust this is
  `result.len() > start && result[result.len() - 1] != b'/'` or
  equivalently the `result.last() != Some(&b'/')` pattern already in
  use.
- The `clean_and_validate_name` REFUSE-mode path must continue to use
  `name.get(i + n)` rather than direct indexing for look-ahead, so a
  malformed sender cannot trigger an out-of-bounds panic on the
  receiver.
