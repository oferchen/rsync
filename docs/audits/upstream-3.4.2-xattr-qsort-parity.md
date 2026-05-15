# Upstream 3.4.2 xattr qsort element-count parity

## Upstream change

Upstream 3.4.1 `xattrs.c:864` invoked `qsort` over `receive_xattr()`'s entry
buffer using `count` - the wire-encoded entry count - as the element count:

```c
if (need_sort && count > 1)
    qsort(temp_xattr.items, count, sizeof (rsync_xa), rsync_xal_compare_names);
```

The receive loop may drop entries via `continue` (filter exclusion at line
815, non-root-namespace at 824, ENV-strip at 845, `rsync.%` internal at
851). Each skip leaves `temp_xattr.count < count`, so sorting `count`
elements walked past the populated tail into uninitialised slots of the
`item_list` backing buffer, producing a non-deterministic ordering for the
list cached by `rsync_xal_store()`. Subsequent transfers comparing the
list against a freshly-built one diverged on order, breaking the abbreviated
xattr cache and corrupting wire output on the next file with the same
xattr set.

3.4.2 fixes both the guard and the qsort length to use the actual stored
count:

```c
if (need_sort && temp_xattr.count > 1)
    qsort(temp_xattr.items, temp_xattr.count, sizeof (rsync_xa),
          rsync_xal_compare_names);
```

The companion qsort at `xattrs.c:297` already used `xalp->count` and is
unchanged.

## oc-rsync sort sites audited

| Site | Path | Verdict |
|------|------|---------|
| Send-side collection sort | `crates/metadata/src/xattr.rs:148` | Safe. `entries.sort_unstable_by` operates on the populated `Vec<XattrEntry>`; no separate element-count parameter exists. |
| Receive-side post-translation sort | `crates/protocol/src/xattr/cache.rs:220` calls `XattrList::sort_by_name` | Safe. `sort_by_name` (`crates/protocol/src/xattr/list.rs:117`) sorts `self.entries` (the `Vec` built by `push` after every `continue` filter). Length is the `Vec`'s own state. |
| Free-function helper | `XattrList::sort_by_name` | Safe. Uses `sort_unstable_by` on a `Vec`. |

Rust's `Vec::sort_unstable_by` borrows the slice's true length, so the C
bug class - passing a stale element count alongside a pointer - cannot
manifest. Every sort site sorts the same `Vec` that received the `push`
calls, never an external counter.

Comparator parity: every site uses `a.name().cmp(b.name())` over the raw
name bytes (`Vec<u8>`), which is the lexicographic byte ordering produced
by `strcmp` on NUL-terminated names with no embedded NULs (upstream
guarantees trailing NUL at `xattrs.c:803`). The wire encoder strips the
NUL before storing in the `Vec`, so the trailing byte is absent from both
keys and `cmp` yields the same order `strcmp` would on the
NUL-terminated upstream representation.

## Verdict

No divergence. The Rust receive path is structurally immune to the
upstream off-by-element-count bug. A regression test was added to lock
in the invariant that sorting is correct after `continue`-skipped
entries.

## Test added

`crates/protocol/src/xattr/cache.rs` -
`receive_entries_sorted_after_skips`: feeds a literal xattr set where the
internal `rsync.%stat` entry is filtered out at `preserve_xattrs == 1`,
then verifies the remaining tail is sorted by local name.
