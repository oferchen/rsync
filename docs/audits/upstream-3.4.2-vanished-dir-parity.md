# Upstream 3.4.2 parity: ignore "directory has vanished" errors

Tracking issue: #2232. Verified 2026-05-15 against `origin/master`.

## 1. Upstream change

The rsync 3.4.2 NEWS file records the following parity fix:

> Ignore "directory has vanished" errors.

The change is upstream commit
`0d0f6152` (Jeremy Norris, merged by Andrew Tridgell). It updates the
`support/rsync-no-vanished` wrapper script to extend the existing
`file has vanished` regex to also match the `directory has vanished`
warning that `flist.c:interpret_stat_error` has emitted since at least
3.0:

```
- IGNOREOUT='^(file has vanished: |rsync warning: some files vanished ...)'
+ IGNOREOUT='^((file|directory) has vanished: |rsync warning: some files vanished ...)'
```

The walk-time tolerance itself is older. The relevant upstream sources
(`flist.c` is byte-identical between 3.4.1 and 3.4.2) are:

- `flist.c:1804-1812` (`interpret_stat_error`) - on ENOENT from
  `link_stat`, set `IOERR_VANISHED`, emit
  `"%s has vanished: %s"`, return (continue traversal).
- `flist.c:1833-1838` (`send_directory`) - on `opendir` returning
  ENOENT, call `interpret_stat_error` (sender) or silently return,
  matching the same "warn and continue" pattern.
- `flist.c:1269-1296` (`make_file`) - on `readlink_stat` ENOENT, emit
  `"file has vanished: %s"`, return NULL, traversal continues.

## 2. oc-rsync status: at parity in the production walk

The production sender walks via
`crates/transfer/src/generator/file_list/walk.rs`. Every readdir/stat
failure is funnelled through `record_io_error`, which maps
`io::ErrorKind::NotFound` to `IOERR_VANISHED` and all other errors to
`IOERR_GENERAL` (`crates/transfer/src/generator/mod.rs:653-658`), then
returns `Ok(())` so the recursive walk continues. The tolerant sites
are:

| Site | Behaviour on ENOENT |
|------|---------------------|
| `walk_path` resolve symlink metadata (walk.rs:37-44) | log, record_io_error, return Ok |
| Recursive `read_dir` (walk.rs:142-155, 206-220) | log "opendir failed", record_io_error, return Ok |
| `read_dir` iterator item error (walk.rs:235-247) | log "readdir", record_io_error, skip entry |
| Batched child stat (walk.rs:262-297) | log_stat_error, record_io_error, skip entry |
| Post-batch `--copy-unsafe-links` re-stat (walk.rs:279-285) | log_stat_error, record_io_error, skip entry |

`log_stat_error` (walk.rs:307-320) emits `file has vanished:` for
ENOENT and `link_stat ... failed:` otherwise, matching upstream
`flist.c:1289`/`flist.c:1810` byte-for-byte. The wording is pinned by
`rsyserr_wording_tests` in the same file.

A new regression test
(`crates/transfer/src/generator/tests.rs::build_file_list_tolerates_vanished_subdirectory`)
creates a child subdirectory, removes it before `build_file_list`
runs, and asserts:

1. `build_file_list` returns `Ok` and yields the survivor entries.
2. The vanished subdirectory is absent from the list.
3. `ctx.io_error() & IOERR_VANISHED != 0`.

## 3. oc-rsync status: hardened in the `engine::walk` public API

`crates/engine/src/walk/walkdir_impl.rs` is a public re-export of a
parallel walker built on `jwalk`. It is not currently wired into the
transfer path, but its iterator previously surfaced ENOENT errors from
`dir_entry.metadata()` and from `jwalk::Error` (e.g. ENOENT from
`opendir` on a vanished subtree) as `WalkError::Walk`, which aborts
any consumer that does the natural `for entry in walker?` pattern.

Two narrow fixes in `walkdir_impl.rs::next` bring it in line with
upstream:

1. `dir_entry.metadata()` returning `ErrorKind::NotFound` now
   `continue`s instead of producing an error.
2. A jwalk error whose `io_error()` is `NotFound` and whose `depth() > 0`
   is also skipped. Depth zero (the root argument the user passed on
   the command line) is still reported because upstream only ignores
   top-level ENOENT under `--ignore-missing-args` (`flist.c:2393`).

`tolerates_vanished_subdirectory_mid_walk` (same file) is the
companion regression test - it creates a subdirectory, deletes it,
walks the parent, and asserts no errors are produced.

## 4. oc-rsync status: other walkers (not on the production path)

| Module | Used by sender? | Vanish handling |
|--------|-----------------|-----------------|
| `crates/flist/src/file_list_walker.rs` | No (public API only) | Strict; aborts on first I/O error. Acceptable because consumers can wrap the iterator if they want upstream-style tolerance. |
| `crates/flist/src/parallel.rs::collect_paths_recursive` | No (public API only) | Tolerant; uses `if let Ok(...)` for both `symlink_metadata` and `read_dir`, silently skipping ENOENT mid-walk. |

These are exposed for crate consumers and benchmarks; the production
sender does not call into them.

## 5. The `rsync-no-vanished` wrapper

oc-rsync does not ship a port of the upstream `support/rsync-no-vanished`
helper, so the regex change itself is not a parity concern. The
production warning text we emit on ENOENT during the walk is already
`file has vanished:` or `... link_stat ... failed:` - identical to
upstream and therefore matched by both the old and new regex if a user
chooses to run our binary through the upstream wrapper.

## 6. Conclusion

- Production walk: already tolerated vanished entries; regression
  test added to lock the behaviour in.
- `engine::walk` public API: tightened to match upstream and covered
  by a new regression test.
- No wire protocol or man-page changes are required.
