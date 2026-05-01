# Windows Backslash Wire-Encoding Audit

Investigation of the user claim filed in tasks #1905 and #1939 that Windows
filesystem path separators (`\`) leak into wire-encoded filenames sent by
oc-rsync, breaking transfers from a Windows sender to a non-Windows
receiver.

Last updated: 2026-04-29
Tracks: #1905 (fix) and #1939 (this audit)
Companion audit: `docs/audits/windows-path-normalization.md` (#1842)

## Verdict

**BUG CONFIRMED.** Severity: **HIGH**.

A Windows oc-rsync sender that builds a `FileEntry` whose path was
constructed via `Path::join` / `PathBuf::push` (the normal case for
recursive transfers) emits the native string with `\` separators
verbatim on the wire. A POSIX rsync receiver decoding these bytes
treats every `\` as part of a single filename, producing one literal
filename per source file instead of the expected directory hierarchy.

This finding mirrors and expands F1 in the existing
`windows-path-normalization.md` audit (#1842), which classified F1 as
HIGH but deferred the fix because it changes wire-encoded bytes.

## Reproducer

```rust
#[cfg(windows)]
#[test]
fn windows_path_uses_forward_slash_on_wire() {
    use protocol::flist::{FileEntry, FileType};
    use std::path::PathBuf;

    let mut path = PathBuf::from("subdir");
    path.push("file.txt");                // Windows: now stored as `subdir\file.txt`

    let entry = FileEntry::new_file(path, 1024, 0o644);
    assert_eq!(entry.name_bytes(), b"subdir/file.txt"); // FAILS today: returns b"subdir\\file.txt"
}
```

The same byte stream reaches the receiver via
`crates/protocol/src/flist/write/mod.rs:376`:

```rust
let raw_name = entry.name_bytes();
let name = self.apply_encoding_conversion(raw_name)?;
// ...
writer.write_all(&name[same_len..])?;        // mod.rs eventually -> encoding.rs:97
```

A Linux receiver of the bytes `subdir\file.txt` then writes a single
15-byte file named literally `subdir\file.txt`, with the backslash
embedded in the filename. The destination directory hierarchy
(`subdir/file.txt`) is not created.

## Wire-format Invariant

Upstream rsync 3.4.1, `flist.c:534-570` (`send_file_entry`):

```c
if (xflags & XMIT_SAME_NAME)
    write_byte(f, l1);
if (xflags & XMIT_LONG_NAME)
    write_varint30(f, l2);
else
    write_byte(f, l2);
write_buf(f, fname + l1, l2);
```

`write_buf` writes `fname + l1` for `l2` bytes verbatim - the
filename bytes go on the wire untouched. Upstream does not normalise
separators at this layer because every upstream build either runs on a
POSIX kernel (paths are already `/`-separated) or runs under Cygwin,
whose POSIX layer presents `/`-separated paths to the application
before this code is reached.

oc-rsync targets native Win32 (`windows-msvc`, `windows-gnu`) where no
such layer exists. The wire format remains `/` because both POSIX and
Cygwin senders produce it, but the sender is responsible for producing
those bytes. The bug is therefore on the oc-rsync sender side and not
a wire-protocol divergence.

## Affected Code

All sites are in `crates/protocol/src/flist/`. None of them perform
separator normalisation on Windows.

| File | Line(s) | Purpose | Carries `\` on Windows? |
|---|---|---|---|
| `entry/accessors.rs` | 120-128 | `name_bytes()` accessor used by every wire write site | YES (root cause) |
| `entry/accessors.rs` | 77-107 | `strip_leading_slashes()` Windows branch trims `/` only | YES (does not strip leading `\`) |
| `write/encoding.rs` | 105-122 | `write_symlink_target()` writes `target.as_os_str().as_encoded_bytes()` verbatim | YES (Windows symlink targets carry `\`) |
| `write/mod.rs` | 376 | `write_entry()` calls `entry.name_bytes()` and forwards the bytes through `apply_encoding_conversion` to `write_name` | Inherits the bug from `name_bytes()` |
| `sort.rs` | 44, 88, 231-245, 764-768 | Sort comparison uses `name_bytes()` | Stable across platforms only after the fix |

## Remediation

Add a single-purpose helper that mirrors the existing `wire_mode.rs`
identity-on-Unix / convert-on-Windows pattern:

```rust
// crates/protocol/src/flist/wire_path.rs

#[cfg(unix)]
pub(crate) fn path_bytes_to_wire(p: &std::path::Path) -> std::borrow::Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;
    std::borrow::Cow::Borrowed(p.as_os_str().as_bytes())
}

#[cfg(not(unix))]
pub(crate) fn path_bytes_to_wire(p: &std::path::Path) -> std::borrow::Cow<'_, [u8]> {
    let s = p.to_string_lossy();
    if s.as_bytes().contains(&b'\\') {
        std::borrow::Cow::Owned(s.as_bytes().iter().map(|&b| if b == b'\\' { b'/' } else { b }).collect())
    } else {
        std::borrow::Cow::Owned(s.into_owned().into_bytes())
    }
}
```

Apply this helper at every wire-encode call site:

1. `entry::FileEntry::name_bytes()` - return wire-format bytes. Because
   the helper may need to allocate on Windows the accessor changes
   signature from `&[u8]` to `Cow<'_, [u8]>`. Sort comparators in
   `sort.rs` borrow the inner slice; they continue to work unchanged
   because sort keys are still `&[u8]` borrowed from the `Cow`.
2. `entry::FileEntry::strip_leading_slashes()` - the Windows branch
   must trim both `/` and `\` so that `--relative` pre-strip behaves
   the same on Windows as on POSIX.
3. `write::encoding::write_symlink_target()` - call the helper for the
   target's bytes. Targets are paths and follow the same convention.

Testing:

- `#[cfg(windows)]` unit test asserting `path_bytes_to_wire` returns
  `b"a/b/c"` for `Path::new("a\\b\\c")`.
- `#[cfg(windows)]` integration test that builds a `FileEntry` via
  `PathBuf::push`, writes it through `FileListWriter`, reads the bytes
  back, and asserts no `\` byte appears anywhere in the encoded
  filename.
- Roundtrip: a writer-then-reader test that asserts the decoded entry
  yields a `name()` of `subdir/file.txt` regardless of host platform.
- Identity test on Unix: the helper performs zero allocation when the
  input is already `/`-separated (asserted via
  `matches!(_, Cow::Borrowed(_))` on Unix only).

## Why this is "fix only" and the audit goes in this PR

The companion audit `windows-path-normalization.md` (HIGH severity F1)
was filed without code changes because the fix touches wire-encoded
bytes and required dedicated regression coverage. This PR closes that
loop by shipping the helper, the call-site changes, and the regression
tests in one atomic change. The file `docs/audits/windows-path.md`
serves as the per-fix audit-of-record citing #1905 and #1939.

## References

- Upstream `flist.c:534-570` (`send_file_entry` filename emission).
- Upstream `flist.c:570` (`write_buf(f, fname + l1, l2)` - the wire
  bytes are written without any separator normalisation).
- Upstream `util1.c:955-961` `__CYGWIN__` block (Cygwin's two-leading-
  slash preservation - the only `\` handling in upstream is on the
  Cygwin POSIX boundary, which oc-rsync does not run under).
- oc-rsync `crates/protocol/src/flist/wire_mode.rs` (the
  identity-on-Unix / convert-on-Windows precedent this fix copies).
- oc-rsync `docs/audits/windows-path-normalization.md` F1 (the prior
  classification of the same bug).
