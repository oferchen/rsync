# FreeBSD many-xattrs parsing parity vs rsync 3.4.2

Tracking issue: #2231. Verified 2026-05-15 against `origin/master`.

## 1. Upstream fix

rsync 3.4.2 NEWS:

> Fixed handling of objects with many xattrs on FreeBSD.

`lib/sysxattrs.c::sys_llistxattr()` previously trusted the value
returned by `extattr_list_link()` even when the buffer filled exactly.
FreeBSD's `extattr_list_*` family signals "more data available" by
returning `len == size` and truncating to fit, so the old early-exit
guard silently dropped trailing entries when an object had enough
xattrs to fill the buffer.

3.4.1 (`lib/sysxattrs.c:124-148`):

```c
ssize_t off, len = extattr_list_link(path, EXTATTR_NAMESPACE_USER, list, size);

if (len <= 0 || (size_t)len > size)
    return len;

for (off = 0; off < len; off += keylen + 1) {
    keylen = ((unsigned char*)list)[off];
    if (off + keylen >= len) { errno = EINVAL; return -1; }
    memmove(list+off, list+off+1, keylen);
    list[off+keylen] = '\0';
}
return len;
```

3.4.2 (`lib/sysxattrs.c:124-157`):

```c
ssize_t off, len = extattr_list_link(path, EXTATTR_NAMESPACE_USER, list, size);

if (len <= 0 || size == 0)
    return len;

if ((size_t)len >= size) {
    /* FreeBSD signals "more data available" by returning size as len.
       Force ERANGE so xattrs.c retries with a bigger buffer. */
    errno = ERANGE;
    return -1;
}
/* ...same in-place transform as before... */
```

The kernel returns the entry list as `[len_u8][name_bytes]...` with no
NUL terminator. Upstream converts the prefix-length encoding into NUL-
terminated strings in place, advancing the cursor by `keylen + 1` per
entry. The actual bug fix is in the size check, not the parser:
`len > size` was changed to `len >= size` and split out so a full
buffer triggers an `ERANGE` retry instead of silently truncating.

## 2. oc-rsync surface area

oc-rsync does not call `extattr_*` directly. The FreeBSD xattr backend
lives in the `xattr` crate (v1.6.1, pinned in `Cargo.lock:4759`):

```
metadata::xattr_unix -> xattr crate (v1.6.1) -> libc::extattr_list_*
```

Files:

- `crates/metadata/src/xattr_unix.rs:20` - `list_attributes()` is a
  thin wrapper around `xattr::list` / `xattr::list_deref`.
- `crates/metadata/src/xattr.rs` - cross-platform facade that calls
  into `xattr_unix` on Unix targets (including FreeBSD).
- `crates/metadata/Cargo.toml:26` - `xattr = { version = "1.6", ... }`.

A workspace-wide grep confirms there is no direct FFI into FreeBSD's
extattr interface in oc-rsync's own code:

```sh
grep -rn 'extattr_list\|EXTATTR_NAMESPACE\|sys_llistxattr' crates/
# -> no matches
```

The only `extattr`/`FreeBSD` references in `crates/metadata` are doc
comments and `#[cfg(target_os = "freebsd")]` gates on ACL paths.

## 3. xattr crate v1.6.1 - parser audit

The FreeBSD code path is `src/sys/bsd.rs` in the
[Stebalien/xattr](https://github.com/Stebalien/xattr) repository
(v1.6.1, tagged 2025-09-21, latest release).

### 3.1 List-allocation loop

`allocate_loop()` (`src/sys/bsd.rs:54-72`) is the equivalent of
upstream's "retry on full buffer" logic, and it is already correct:

```rust
let new_len = cvt(f(ptr, len))?;
if new_len < len {
    Ok(slice::from_raw_parts_mut(ptr.cast(), new_len))
} else {
    // If the length isn't strictly smaller than the buffer, there
    // may be more to read. Fake an ERANGE error so we can try again
    // with a bigger buffer.
    Err(io::Error::from_raw_os_error(crate::sys::ERANGE))
}
```

The condition `new_len < len` is the inverse of upstream 3.4.2's
`(size_t)len >= size` and produces the same retry behaviour: a buffer
that the kernel filled exactly is treated as potentially truncated and
the outer `util::allocate_loop` re-runs with a doubled buffer. The
initial size estimate also adds `+1` to the probe result so that the
first real call cannot land on the `len == size` ambiguity by accident
(comment at `bsd.rs:68-71`).

### 3.2 Entry iterator

`XAttrs::next` (`src/sys/bsd.rs:82-122`) walks the prefix-length
buffer:

- Termination: `if self.offset == self.user_attrs.len() +
  self.system_attrs.len() { return None; }` (line 89). This is exact
  equality, matching upstream's `off < len` loop bound. A
  malformed buffer that overshoots would slice-panic rather than loop
  forever, so there is no risk of a runaway read past the end.
- Advance: `self.offset += siz + 1` (line 101), where `siz =
  data[0] as usize` is the FreeBSD single-byte length prefix. This
  matches upstream's `off += keylen + 1`.
- Name slice: `&data[1..siz + 1]` (lines 104, 109). If a kernel-
  emitted buffer ever declared a `siz` longer than the remaining
  bytes, this would panic; upstream defends with `off + keylen >= len`
  and returns `EINVAL`. Both treat the kernel as the source of truth,
  and the only observable difference is panic vs `EINVAL` on a
  malformed kernel buffer - a case neither implementation actually
  encounters in practice.

### 3.3 Two-namespace listing

The crate lists `EXTATTR_NAMESPACE_SYSTEM` and `EXTATTR_NAMESPACE_USER`
separately (`bsd.rs:207-236` and `bsd.rs:291-328`), tolerating `EPERM`
on the system namespace so non-root callers see only `user.*`. Each
namespace goes through its own `allocate_loop` invocation, so the
many-xattrs retry path is exercised independently for each. This is
strictly better than upstream's single-pass `EXTATTR_NAMESPACE_USER`-
only listing for the purposes of completeness, and the retry logic is
the same.

## 4. Conclusion

No production code change is required in oc-rsync.

- oc-rsync routes every FreeBSD xattr listing through the `xattr`
  crate v1.6.1.
- The crate's `allocate_loop` already implements the equivalent of the
  3.4.2 fix (`new_len < len` triggers a buffer-grow retry), and the
  `XAttrs` iterator advances the cursor by `length_prefix + name_len`
  with an exact-equality terminator that mirrors upstream's loop
  bound.
- v1.6.1 is the latest published release (2025-09-21); no newer fix
  needs to be pulled forward.

If a future upstream change moves the FreeBSD entry parsing back into
rsync itself, this audit should be revisited and the parser
re-implemented inside `crates/metadata` with a unit test that
synthesises a many-entry `[len_u8][name]...` buffer hitting the
`offset == buf.len()` terminator exactly.

## 5. Upstream references

- `target/interop/upstream-src/rsync-3.4.2/lib/sysxattrs.c:124-157`
  (`sys_llistxattr` after fix).
- `target/interop/upstream-src/rsync-3.4.1/lib/sysxattrs.c:124-148`
  (`sys_llistxattr` before fix).
- `target/interop/upstream-src/rsync-3.4.2/NEWS.md:62` - release note.
