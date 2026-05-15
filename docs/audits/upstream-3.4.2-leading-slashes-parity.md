# Upstream 3.4.2 parity: removal of multiple leading slashes

Tracking issue: #2233. Verified 2026-05-15 against `origin/master`.

## 1. Upstream change

The rsync 3.4.2 NEWS entry reads:

> Fixed the removal of multiple leading slashes.

The fix is upstream commit `d4c4f67` ("fixed remove multiple leading
slashes") and it lands in exactly one place:

- File: `support/rrsync` (a Python wrapper that locks an SSH-forced
  command into a restricted directory; not part of the rsync C core).

```diff
 if arg.startswith('./'):
     arg = arg[1:]
 arg = arg.replace('//', '/')
+arg = arg.lstrip('/')
 if args.dir != '/':
     if HAS_DOT_DOT_RE.search(arg):
         die("do not use .. in", opt, ...)
     if arg.startswith('/'):
         arg = args.dir + arg
```

`str.replace('//', '/')` walks the string left-to-right matching
non-overlapping pairs, so `///foo` collapses to `//foo` and an odd-length
run of leading slashes leaves a double slash behind. The follow-up
`startswith('/')` check then mis-classifies the path as still absolute
and silently joins it under `args.dir`. Adding `lstrip('/')` collapses
any run of leading slashes (POSIX-style) before the absolute-path
re-anchor logic runs.

No C source file in `rsync-3.4.2/` was touched by this commit. A full
`diff -r rsync-3.4.1 rsync-3.4.2` confirms `util1.c::clean_fname`,
`util1.c::sanitize_path`, `util1.c::make_path`, `flist.c::recv_file_entry`,
and `flist.c::sort_and_clean` are unchanged with respect to leading-slash
handling between the two releases. The 3.4.2 util1.c diff is the
`clean_fname` `..` underflow rework already audited under #2227 / PR #4085
and is out of scope here.

## 2. oc-rsync surface area

oc-rsync does not ship the `rrsync` Python helper. The audit therefore
focuses on the three Rust sites that normalize wire/CLI paths and might
plausibly carry an equivalent bug.

### 2.1 `crates/protocol/src/flist/read/name.rs::clean_and_validate_name`

Mirrors upstream `flist.c:756-760` (`clean_fname(CFN_REFUSE_DOT_DOT_DIRS)`
plus the `!relative_paths && *thisname == '/'` reject). The leading-slash
strip is implemented as a single bulk skip (`name.rs:185-189`):

```rust
let start = if anchored {
    name.iter().position(|&b| b != b'/').unwrap_or(name.len())
} else {
    0
};
```

`position` consumes every contiguous leading `/`, so `//foo`, `///foo`,
and `/////bar` all reach the component-copy loop pointing at the first
non-slash byte. Inside the loop, runs of interior slashes are coalesced
by the `name[i] == b'/' { i += 1; continue; }` guard, matching upstream
`clean_fname` lines 967-972.

Trace table (with `relative_paths = true`):

| Input | `anchored` | `start` | Output |
|-------|-----------|---------|--------|
| `//foo` | true | 2 | `foo` |
| `///foo` | true | 3 | `foo` |
| `/////bar` | true | 5 | `bar` |
| `/foo//bar` | true | 1 | `foo/bar` |
| `foo//bar` | false | 0 | `foo/bar` |
| `///` | true | 3 (=len) | `.` |

With `relative_paths = false`, the leading-`/` byte is rejected with
`ABORTING due to unsafe pathname from sender`, matching upstream's
`exit_cleanup(RERR_UNSUPPORTED)` at `flist.c:758`.

**Note on byte-for-byte parity:** upstream `clean_fname` keeps one
leading slash on relative-paths input (`*t++ = *f++` at `util1.c:954`);
the leading slash is stripped later in `flist.c:3079-3080` via
`while (*file->dirname == '/') file->dirname++` during the post-sort
`strip_root` pass. oc-rsync collapses the run in a single step at the
flist-read site, producing the same final dirname/basename pair without
the intermediate two-step state. The output observed by every downstream
consumer (sort key, dirname dedup, on-disk path) is identical to
upstream's, and the eager strip is itself a `while (head == '/')` loop -
the precise pattern the upstream `rrsync` fix introduces.

Verdict: **SAFE**. No `replace("//", "/")` pair-walk anywhere in the
function.

### 2.2 `crates/transfer/src/sanitize_path.rs::sanitize_path`

Mirrors upstream `util1.c::sanitize_path()`. The leading-slash handling
is a single `if` (`sanitize_path.rs:63-65`) that consumes one byte, then
the main loop discards any remaining slashes:

```rust
if p < bytes.len() && bytes[p] == b'/' {
    p += 1;
}
// ...
while p < bytes.len() {
    if bytes[p] == b'/' { p += 1; continue; }
    // ...
}
```

This is bytewise identical to upstream `util1.c:1046-1051` followed by
the `while (*p) { if (*p == '/') { p++; continue; } ... }` body at
`util1.c:1072-1076`. Existing unit test
`triple_slashes_collapsed` (`sanitize_path.rs:226-229`) already exercises
`foo///bar///baz -> foo/bar/baz`. The leading-slash sweep is covered by
`absolute_path_stripped`, but a `///foo`-shaped input was not pinned to
a test before this audit; see section 4.

Verdict: **SAFE**.

### 2.3 `crates/flist/src/symlink_safety.rs::is_unsafe_symlink`

Mirrors upstream `util1.c::unsafe_symlink()`. The function rejects any
target whose first byte is `/` (line 39), so `/foo`, `//foo`, `///foo`
are all classified unsafe identically. No collapse occurs because the
function is a yes/no oracle, not a normalizer.

Verdict: **SAFE**.

### 2.4 `crates/protocol/src/flist/entry/accessors.rs::root_basename`

Uses `s.trim_start_matches(|c| c == '/' || c == '\\')` (line 103) which
strips every leading separator in one call. POSIX semantics.

Verdict: **SAFE**.

## 3. No equivalent of the `rrsync` site

A grep for `replace("//", "/")` / `replace('//', '/')` / `str::replace`
with slash pairs across all 70 crates finds zero matches. Every place
that needs to collapse a leading-slash run uses one of:

- `iter().position(|&b| b != b'/')` (flist read)
- `while *p == '/' { p++ }` (sanitize_path main loop)
- `trim_start_matches('/')` / `trim_start_matches(|c| c == '/' || c == '\\')`
  (entry accessors, daemon module-list parsing)

All three idioms are correct for arbitrary-length runs. There is no
oc-rsync code path that exhibits the `rrsync` left-to-right pair-replace
bug.

## 4. Test coverage added

`crates/transfer/src/sanitize_path.rs` previously covered
`/etc/passwd`, `foo///bar///baz`, and `/`. The audit adds a focused
sweep over leading-slash run lengths so that any future refactor that
inadvertently switches to a pair-replace strategy is caught:

- `leading_double_slash_collapsed`: `//foo -> foo`.
- `leading_triple_slash_collapsed`: `///foo -> foo`.
- `leading_quintuple_slash_collapsed`: `/////bar -> bar`.
- `leading_slashes_with_interior_double_slash`: `///foo//bar -> foo/bar`.
- `all_slashes_becomes_dot`: `/////` -> `.`.

Tests live alongside the existing `sanitize_path` suite in the same
file, run under `cargo nextest run -p transfer`, and require no extra
fixtures.

## 5. Conclusion

The upstream 3.4.2 fix lives entirely in the `rrsync` Python helper,
which oc-rsync does not redistribute. The three Rust sites that perform
the analogous wire-path normalization (flist receive, daemon
sanitize_path, symlink safety) already strip arbitrary-length leading
slash runs via single-pass loops or `iter::position`. No production
remediation is required. The new unit-test sweep pins the parity claim.

## References

- Upstream commit: `d4c4f6754eff0d8ea6fdb327abf5c874bfccb8dd`
- Upstream file: `target/interop/upstream-src/rsync-3.4.2/support/rrsync:297-310`
- Upstream NEWS: `rsync-3.4.2/NEWS.md:71`
- Related audit (clean_fname `..` underflow, distinct bug): #2227 / PR #4085
