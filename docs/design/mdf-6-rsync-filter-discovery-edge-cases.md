# MDF-6: .rsync-filter discovery edge-case test spec

## Summary

This document specifies integration tests for `.rsync-filter` file discovery
during directory traversal. The `FilterChain::enter_directory()` method
(in `crates/filters/src/chain/mod.rs`) implements upstream rsync's
`push_local_filters()` from `exclude.c`, reading per-directory merge files
and pushing their rules onto the scoped stack. Several edge cases around
file presence, permissions, content, and concurrency need explicit coverage.

## Upstream reference

- `exclude.c:push_local_filters()` - reads per-directory filter files
- `exclude.c:pop_local_filters()` - restores state on directory exit
- `exclude.c:parse_filter_file()` - parses the file content

Upstream behaviour: `push_local_filters()` calls `parse_filter_file()` which
opens the per-dir merge file. If the open fails (ENOENT, EACCES), the file
is silently skipped. If the file exists and is readable, its content is parsed
line-by-line. Invalid syntax causes rsync to emit a warning but continue
(non-fatal in upstream).

## Current coverage

The existing test suite in `crates/filters/src/chain/tests.rs` covers:

- Normal discovery (file present with rules)
- Missing file (no error, zero scopes pushed)
- Empty file (no error, zero scopes pushed)
- Comments-only file (no error, zero scopes pushed)
- Multiple merge configs reading different filenames
- Nested directories each with their own filter file
- Parse errors in merge files (returns `FilterChainError`)
- Exclude-self modifier

The following edge cases are NOT covered and need tests.

## Test scenarios

### 1. Normal discovery: .rsync-filter in each directory during traversal

**Scenario:** Multi-level directory tree where every directory contains a
`.rsync-filter` with distinct rules. Traversal enters each directory in order
and verifies that rules accumulate correctly (innermost takes priority).

**Fixture:**
```
root/
  .rsync-filter  -> "- *.log"
  a/
    .rsync-filter -> "- *.tmp"
    b/
      .rsync-filter -> "- *.bak"
```

**Assertions:**
- After entering `root/`: `*.log` excluded, `*.tmp` allowed
- After entering `root/a/`: both `*.log` and `*.tmp` excluded
- After entering `root/a/b/`: all three patterns excluded
- After leaving `b/`: `*.bak` allowed again
- After leaving `a/`: `*.tmp` allowed again

**Note:** Already partially covered by `filter_chain_nested_directories_with_merge_files`
but should be extended to 3+ levels.

### 2. Missing .rsync-filter: directory has no filter file

**Scenario:** Directory traversal enters a directory without a `.rsync-filter`.
No error should be raised and no scope should be pushed.

**Fixture:** Empty temp directory.

**Assertions:**
- `enter_directory()` returns `Ok(guard)` with `pushed_count() == 0`
- All paths remain allowed (no filtering effect)
- `current_depth()` still increments (tracks nesting, not scope count)

**Note:** Already covered by `filter_chain_enter_directory_no_merge_file`.
Listed here for completeness of the specification.

### 3. Empty .rsync-filter: file exists but has zero bytes

**Scenario:** A `.rsync-filter` file exists but is completely empty (0 bytes).

**Fixture:** Write empty string to `.rsync-filter`.

**Assertions:**
- `enter_directory()` returns `Ok(guard)` with `pushed_count() == 0`
- No filtering effect
- No error or warning

**Note:** Already covered by `filter_chain_enter_directory_empty_merge_file`.

### 4. Symlinked .rsync-filter: filter file is a symlink to another file

**Scenario:** The `.rsync-filter` file is a symbolic link pointing to a filter
file in another location. Discovery should follow the symlink and read the
target file's content.

**Fixture:**
```
root/
  actual-rules.txt -> "- *.obj\n- *.o\n"
  subdir/
    .rsync-filter -> symlink to ../actual-rules.txt
```

**Assertions:**
- `enter_directory("subdir/")` succeeds
- Rules from the symlink target are applied (`*.obj` and `*.o` excluded)
- `pushed_count() == 1`

**Platform gate:** `#[cfg(unix)]` - symlinks require elevated privileges on
Windows.

**Variant - dangling symlink:** The symlink target does not exist. Should
behave as "file not found" (silent skip, no error).

**Variant - symlink loop:** `.rsync-filter` symlinks to itself. Should fail
with an I/O error (the OS returns ELOOP on open), which maps to
`ErrorKind::NotFound` or a platform-specific error. The chain should handle
this gracefully (skip or return error, not panic).

### 5. Race condition: .rsync-filter deleted between stat and read

**Scenario:** The filter file disappears after discovery begins but before the
content is fully read. In practice, `enter_directory()` does a single
`fs::read_to_string()` call, so the race window is between the caller
deciding to enter the directory (perhaps after a `readdir`) and the actual
`read_to_string` inside `enter_directory()`.

**Fixture:** This is inherently racy and difficult to test deterministically.
The recommended approach is to test the error-handling path directly:

1. Create a directory with no `.rsync-filter`.
2. Verify `enter_directory()` handles `ErrorKind::NotFound` by returning
   `Ok(guard)` with `pushed_count() == 0`.

The current implementation already handles this: the `NotFound` match arm in
`enter_directory()` calls `continue`. A targeted unit test should confirm that
an `io::ErrorKind::NotFound` error from the file read produces zero scopes
pushed (not an error return).

**Assertions:**
- `enter_directory()` on a directory where the file vanishes returns `Ok`
- No scopes pushed

### 6. Permission denied: .rsync-filter exists but is unreadable

**Scenario:** The `.rsync-filter` file exists but has mode `000` (or similar
restriction), making it unreadable by the current process.

**Fixture:**
```
root/
  .rsync-filter  -> mode 0o000
```

**Assertions:**
- `enter_directory()` returns `Ok(guard)` with `pushed_count() == 0`
- No error propagated (upstream silently skips EACCES)
- Other merge configs in the same directory are still processed

The current implementation already has this behaviour
(`PermissionDenied => continue`). The test validates the contract.

**Platform gate:** `#[cfg(unix)]` - file permission semantics differ on
Windows. On Windows, use a test that verifies the path by mocking or by
verifying the error-kind matching logic in isolation.

### 7. Very large .rsync-filter: file with thousands of rules

**Scenario:** A `.rsync-filter` file contains 10,000 exclude rules. Discovery
should handle this without stack overflow, excessive latency, or OOM.

**Fixture:** Generate a file with 10,000 lines of `- pattern_NNNN`.

**Assertions:**
- `enter_directory()` succeeds within a reasonable time (< 1 second)
- All 10,000 rules are applied (spot-check first, last, and middle patterns)
- `pushed_count() == 1`
- Memory does not grow unboundedly (no regression test for this, just
  ensure the test completes without OOM)

### 8. Binary/corrupt content in .rsync-filter

**Scenario:** The `.rsync-filter` file contains non-UTF-8 binary data. Since
`fs::read_to_string()` is used, invalid UTF-8 will cause an I/O error.

**Fixture:** Write raw bytes `[0xFF, 0xFE, 0x00, 0x01]` to `.rsync-filter`.

**Assertions:**
- `enter_directory()` returns `Err(FilterChainError::Io { .. })` because
  `fs::read_to_string()` fails on invalid UTF-8
- The error kind is `InvalidData` (from `String::from_utf8` failure)
- No scopes are pushed
- The chain state is consistent (depth not corrupted)

**Variant - binary after valid rules:** File starts with valid rules but has
binary garbage mid-file. Since `read_to_string` reads the entire file first,
this will also fail at the I/O layer if the bytes are invalid UTF-8.

**Design note:** Upstream rsync reads files byte-by-byte with `fgets()` and
treats each line independently. It does not fail on binary content in the
middle of a file - it just produces unrecognized-rule warnings. The oc-rsync
behaviour of failing on non-UTF-8 is stricter than upstream. If upstream
parity is desired, the implementation should switch to `fs::read()` +
`String::from_utf8_lossy()` or byte-level line splitting. This should be
documented as a known divergence or fixed.

### 9. BOM (byte-order-mark) prefix in .rsync-filter

**Scenario:** The `.rsync-filter` file begins with a UTF-8 BOM
(`\xEF\xBB\xBF`) followed by valid filter rules. Some text editors on Windows
prepend a BOM when saving UTF-8 files.

**Fixture:** Write `b"\xEF\xBB\xBF- *.tmp\n"` to `.rsync-filter`.

**Assertions:**
- `enter_directory()` either:
  - Succeeds and the rules are correctly parsed (BOM stripped), or
  - Fails with a parse error on line 1 (BOM treated as rule prefix)

**Current expected behaviour:** The BOM bytes are valid UTF-8
(U+FEFF), so `read_to_string` will succeed. The first line becomes
`"\u{FEFF}- *.tmp"`. After `trim()`, the BOM remains (it is not ASCII
whitespace). The line parser will see `\u{FEFF}-` which does not match any
known prefix, causing a parse error.

**Design note:** Upstream rsync does not strip BOMs either, so both
implementations would reject BOM-prefixed files. The test should assert this
behaviour and document it. If BOM tolerance is desired for Windows
compatibility, a BOM-stripping pre-pass can be added later.

### 10. Multiple dir-merge directives discovering different filenames

**Scenario:** Two merge configs are registered: one for `.rsync-filter` and
another for `.exclude`. A directory contains both files with different rules.

**Fixture:**
```
root/
  .rsync-filter -> "- *.log"
  .exclude      -> "- *.tmp"
```

**Assertions:**
- `enter_directory()` returns `Ok(guard)` with `pushed_count() == 2`
- Both rule sets are active: `*.log` excluded AND `*.tmp` excluded
- Order of evaluation matches config registration order (`.rsync-filter`
  rules checked before `.exclude` rules)
- Leaving the directory removes both scopes

**Variant - only one file present:** Directory has `.rsync-filter` but no
`.exclude`. Only one scope should be pushed.

**Variant - conflicting rules:** `.rsync-filter` says `+ *.txt` and
`.exclude` says `- *.txt`. The first config's scope is checked first
(innermost = last pushed), so order matters.

**Note:** Already partially covered by `filter_chain_multiple_merge_configs`.
Extend with conflict and partial-presence variants.

### 11. .rsync-filter in the transfer root itself

**Scenario:** The transfer root directory (the directory being synced) contains
a `.rsync-filter`. This is the first `enter_directory()` call and should work
identically to any subdirectory.

**Fixture:**
```
transfer-root/
  .rsync-filter -> "- *.o\n- *.so\n"
  src/
    main.rs
    helper.o
```

**Assertions:**
- `enter_directory("transfer-root/")` succeeds with `pushed_count() == 1`
- Rules apply to paths within the root (`helper.o` excluded)
- This is depth 1 (the root itself is depth 1 after the first enter call)

### 12. Interaction with --exclude-from and explicit --filter rules

**Scenario:** Global rules from `--exclude-from` or `--filter` are set on the
`FilterChain`, and per-directory `.rsync-filter` files add additional rules.
The per-directory rules should take priority (checked first) over global rules.

**Fixture:**
```
Global rules: "- *.bak"
root/
  .rsync-filter -> "+ important.bak\n- *.tmp\n"
```

**Assertions:**
- Before entering root: `important.bak` excluded (global rule)
- After entering root: the per-directory include for `important.bak` is
  checked first, but because `has_matching_rule` in the current implementation
  only stops lookup for excludes (not includes), the actual behaviour depends
  on scope evaluation
- `*.tmp` excluded by per-directory rule
- `*.bak` (other than `important.bak`) excluded by global rule
- After leaving root: global rules restored

**Design note:** This tests the Chain of Responsibility pattern. The current
implementation's `has_matching_rule()` helper determines whether a scope
"intercepts" the lookup. Understanding the include-passthrough semantics is
critical here.

### 13. Whitespace-only lines in .rsync-filter

**Scenario:** The file contains lines that are only spaces or tabs (not truly
empty after trimming).

**Fixture:** `"  \n\t\n- *.tmp\n  \t  \n"`

**Assertions:**
- Whitespace-only lines are treated as blank (skipped)
- The single valid rule `- *.tmp` is parsed and applied
- No parse error

### 14. Comment variations in .rsync-filter

**Scenario:** Comments using both `#` and `;` prefixes, inline content after
`#`, and edge cases like `#` inside a pattern.

**Fixture:**
```
# full line comment
; semicolon comment
- *.tmp
- file#name    (this is a pattern containing #, not a comment)
```

**Assertions:**
- Lines 1-2 skipped as comments
- `*.tmp` parsed as exclude rule
- `file#name` parsed as an exclude pattern (mid-line `#` is NOT a comment
  delimiter in rsync filter syntax)

### 15. Unicode filenames in .rsync-filter patterns

**Scenario:** Filter rules reference files with Unicode names (e.g., CJK
characters, emoji, accented characters).

**Fixture:** `"- \u{00E9}l\u{00E8}ve.txt\n- \u{4E2D}\u{6587}/\n"`

**Assertions:**
- Rules parse successfully
- Pattern matching works against Unicode filenames
- No encoding errors

## Module placement

All new tests should be placed in:

```
crates/filters/src/chain/tests.rs
```

This is the existing test module for `FilterChain` and already contains the
`tempfile::TempDir` + `fs::write` fixture pattern. Tests that require
platform-specific features should be gated:

- `#[cfg(unix)]` - symlink tests, permission tests
- No `#[cfg(windows)]` equivalents needed unless Windows-specific discovery
  behaviour is added later

For tests that are too large for unit tests (e.g., full traversal simulation
with many directories), consider a dedicated integration test file:

```
crates/filters/tests/discovery_edge_cases.rs
```

## Fixture design

All tests should use `tempfile::TempDir` for isolation. The pattern:

```rust
#[test]
fn filter_chain_discovery_<scenario>() {
    let dir = TempDir::new().unwrap();
    // Create fixture files
    fs::write(dir.path().join(".rsync-filter"), "<content>").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

    let guard = chain.enter_directory(dir.path()).unwrap();
    // Assertions...
    chain.leave_directory(guard);
    // Post-leave assertions...
}
```

For symlink fixtures:
```rust
#[cfg(unix)]
std::os::unix::fs::symlink(target, link_path).unwrap();
```

For permission fixtures:
```rust
#[cfg(unix)]
{
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
    // Test...
    // Restore permissions for cleanup (TempDir cannot remove mode-000 files)
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
}
```

## Assertion strategy

1. **Structural assertions:** Check `pushed_count()`, `scope_depth()`,
   `current_depth()` to verify the chain state machine.
2. **Behavioural assertions:** Check `allows(path, is_dir)` for specific
   paths to verify rule application.
3. **Error assertions:** For error cases, match on `FilterChainError` variants
   and verify the error message/path content.
4. **Invariant assertions:** After `leave_directory()`, verify that the chain
   returns to its previous state (scopes popped, rules no longer active).

## Priority

| Scenario | Priority | Rationale |
|----------|----------|-----------|
| 4. Symlinked filter file | P0 | Common in real deployments (shared filter configs) |
| 6. Permission denied | P0 | Silent skip is safety-critical; regression would break transfers |
| 8. Binary/corrupt content | P0 | Documents known divergence from upstream |
| 9. BOM prefix | P1 | Windows interop concern |
| 7. Very large filter file | P1 | Performance regression guard |
| 10. Multiple directives | P1 | Interaction correctness |
| 12. Interaction with global | P1 | Chain-of-responsibility correctness |
| 5. Race condition | P2 | Hard to test deterministically |
| 11. Transfer root filter | P2 | Edge of traversal boundary |
| 13-15. Content variations | P2 | Parser robustness |

## Known divergences from upstream

1. **Non-UTF-8 content:** oc-rsync fails with I/O error; upstream treats each
   line independently via `fgets()` and issues warnings for unparseable lines.
2. **BOM handling:** Both upstream and oc-rsync reject BOM-prefixed files, but
   for different reasons (upstream sees unrecognized prefix; oc-rsync sees
   unrecognized first character after Unicode decoding).
3. **Permission denied:** Both skip silently. Upstream does not log; oc-rsync
   also does not log. This is correct shared behaviour.

## Related tasks

- MDF-1: Modifier coverage audit (completed)
- MDF-5: `no_inherit` chain semantics (open)
- MDF-7: Reject `w` outside merge rules (open)
- MDF-9: Comma separator in merged filter files (open)
