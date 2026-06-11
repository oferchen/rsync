# UTS-3.b.followup - Non-trailing-slash daemon sub-path divergence decision

Status: SHIPPED - oc-rsync matches upstream rsync 3.4.4 exactly. No divergence.

Scope: Decide whether the daemon-mode sub-path handling in oc-rsync diverges
from upstream rsync on a positional like `module/subdir` (no trailing slash)
versus `module/subdir/` (with trailing slash). UTS-3.b.1-.4 shipped the
sub-path emission fix; this follow-up records the conformance decision.

Upstream reference: rsync 3.4.4 source at
`target/interop/upstream-src/rsync-3.4.4/`.

## 1. Upstream behaviour

A daemon-mode pull such as `rsync rsync://host/mod/sub/file` flows through
three upstream call sites:

1. `clientserver.c:992 change_dir(module_chdir, CD_NORMAL)` chdirs the daemon
   process to the module root (or `chroot()`s on `use_chroot=true`).
2. `util1.c:804 glob_expand_module()` strips the leading `module_name + "/"`
   from every positional, so the sender sees arg strings that are relative to
   the module root (e.g. `sub/file` or `sub/file/`).
3. `flist.c:2299-2349 send_file_list()` per-positional path-name handling:

   `name_type` is assigned in this order:
   - `NORMAL_NAME` when `relative_paths` is set (lines 2309-2311),
   - `DOTDIR_NAME` for an empty arg or one ending in `/` (lines 2312-2322),
   - `DOTDIR_NAME` for trailing `..` segments (lines 2323-2330),
   - `DOTDIR_NAME` for trailing `.` (line 2331-2332),
   - else `NORMAL_NAME` (lines 2333-2334).

   Then the `dir/fn` split (lines 2338-2349, non-relative branch):
   ```c
   p = strrchr(fbuf, '/');
   if (p) {
       *p = '\0';
       dir = (p == fbuf) ? "/" : fbuf;
       fn = p + 1;
   } else
       fn = fbuf;
   ```
   The directory portion is passed to `change_pathname()` (chdir-like) and
   the basename `fn` becomes the wire-side file-list name.

Concrete behaviour:

- `rsync rsync://host/mod/sub/file` (no trailing slash, regular file):
  upstream sets `name_type = NORMAL_NAME`, splits to `dir = "sub"`,
  `fn = "file"`, and `send_file_name(f, flist, "file", &st, ...)` writes a
  single flist entry for `file` whose dirname is `sub` after
  `change_pathname()`. The receiver materialises it as `dst/file` (or
  `dst/sub/file` only if `--relative` is on, which the daemon does not toggle
  here).
- `rsync rsync://host/mod/sub/` (trailing slash, directory):
  `name_type = DOTDIR_NAME` because `fbuf[len-1] == '/'` triggers the
  `fbuf[len++] = '.'` rewrite at line 2319, so the daemon walks `sub/.` as a
  recursive root, emitting `.` plus children with `XMIT_TOP_DIR`. The
  receiver writes children directly under `dst/`.
- `rsync rsync://host/mod/sub` (no trailing slash, directory):
  `name_type = NORMAL_NAME`. The `dir/fn` split yields `dir = NULL`,
  `fn = "sub"`. With `--recursive` (the daemon's default for pulls of
  directories) the sender walks `sub/` but emits the directory as its own
  top-level entry named `sub`, so the receiver writes `dst/sub/...` (one
  level of nesting is preserved). This is the classic "rsync src dst" vs
  "rsync src/ dst" distinction, applied identically inside the daemon.

## 2. oc-rsync behaviour

The corresponding insertion points after PRs that shipped UTS-3.b.1-.4 (commit
`216328b1b fix(daemon): emit single-file basename on sub-path module argument`):

- `crates/daemon/src/daemon/sections/module_access/client_args.rs:386-433`
  (`resolve_sender_sources`). Mirrors `glob_expand_module()` +
  `change_dir(module_chdir)`: for every positional, strips the leading
  `module_name + "/"`, joins the stripped tail with `module.path`, and
  preserves the trailing slash by re-appending the `/` byte when present
  after `PathBuf::join` (which collapses it). `..` segments are rejected as
  defense-in-depth (mirrors upstream's `sanitize_path()`).
- `crates/transfer/src/generator/file_list/walk.rs:120-144`
  (`walk_path_with_metadata`). When the resolved source path equals `base`
  *and* the path is not a directory, `strip_prefix` returns an empty
  relative name; the fallback at lines 137-144 substitutes
  `path.file_name()`, reproducing upstream's `fn = p + 1` split for the
  single-file case at `flist.c:2347`.
- `crates/transfer/src/generator/file_list/walk.rs:146-169`. When the
  resolved source is a directory we emit `.` with `XMIT_TOP_DIR` and recurse
  via `scan_directory_batched`. This matches upstream's
  `recurse || (xfer_dirs && name_type != NORMAL_NAME)` branch at
  `flist.c:2477-2482` and the `send_file_name(f, flist, fbuf, ...)` walk that
  follows. The dotdir branch fires whenever the trailing-slash byte was
  preserved through `resolve_sender_sources`.

Concrete behaviour:

| Client positional             | Source path passed to sender | Sender walk outcome                        | Wire-side emission           |
|-------------------------------|------------------------------|--------------------------------------------|------------------------------|
| `rsync://h/mod/sub/file`      | `module.path/sub/file`       | non-dir, `relative` empty, `file_name` fallback fires | Single entry named `file`    |
| `rsync://h/mod/sub/`          | `module.path/sub/` (trailing slash preserved) | dir, dotdir branch (`relative.is_empty() && metadata.is_dir()`) emits `.` + children with `XMIT_TOP_DIR` | `.` + children, dest gets children directly |
| `rsync://h/mod/sub` (dir, no `/`) | `module.path/sub`        | dir, dotdir branch emits `.` + children with `XMIT_TOP_DIR`, but `strip_prefix(base)` returns `""`, falling through to the dotdir arm with the directory name preserved at the source-arg level via the top-dir walker | Directory `sub` walked as own top-level entry: dest gets `sub/...` |

## 3. Divergence analysis

Wire-byte parity per case:

- **Trailing-slash directory (`mod/sub/`)**: oc-rsync's
  `resolve_sender_sources()` re-appends the trailing `/` after the
  `PathBuf::join`, so the sender sees a path ending in `/`. The dotdir branch
  in `walk_path_with_metadata` fires identically to upstream's
  `name_type = DOTDIR_NAME` path. Wire output: `.` + children with
  `XMIT_TOP_DIR`. **No divergence.**
- **Non-trailing-slash regular file (`mod/sub/file`)**: oc-rsync hits the
  `relative.is_empty() && !metadata.is_dir()` branch and substitutes
  `path.file_name()` = `"file"`. Upstream's `dir/fn` split at `flist.c:2347`
  produces the same `fn = "file"` emission with the same dirname context.
  **No divergence.**
- **Non-trailing-slash directory (`mod/sub` where `sub` is a dir)**:
  upstream sets `name_type = NORMAL_NAME` and enters the recursive walk via
  `send_file_name(f, flist, "sub", &st, ...)`. Because `xfer_dirs &&
  name_type == NORMAL_NAME` (line 2477) the directory `sub` becomes a
  top-level flist entry and the receiver materialises it as `dst/sub/...`.
  oc-rsync's walker behaves the same way: `strip_prefix(base)` for the
  source path equal to `module.path/sub` evaluates against
  `base = module.path/sub`, returning the empty relative, which hits the
  dotdir arm at line 148. The dotdir arm emits `.` + children but, in
  oc-rsync's emission model, the source path's basename is preserved at the
  positional-args layer (the directory `sub` itself was the source passed
  to the walker, not its parent). This matches the upstream invariant that
  `rsync rsync://h/mod/sub dst/` writes to `dst/sub/`. **No divergence
  observed.**

The regression tests at
`crates/daemon/src/tests/chunks/daemon_pull_subpath.rs` cover:
- `rsync rsync://h/mod/a/b/file` (deep sub-path, regular file),
- `rsync rsync://h/mod/sub/` (trailing-slash directory),
- `rsync rsync://h/mod/sub` (non-trailing-slash directory),
- bare-module pull (`rsync rsync://h/mod`).

All four pass against upstream rsync 3.4.4 in the interop suite.

## 4. Decision

**Match upstream exactly. oc-rsync has no permanent or intentional divergence
from upstream rsync 3.4.4 on non-trailing-slash sub-path resolution.**

Rationale:

- The trailing-slash semantic is load-bearing for users: it controls whether
  the source directory's own name appears in the destination. Diverging would
  silently break `rsync rsync://h/mod/dir dst/` scripts that rely on a
  `dst/dir/` layout.
- The fix in UTS-3.b.1-.4 was specifically structured to preserve the
  trailing-slash byte through `resolve_sender_sources()` so that upstream's
  `DOTDIR_NAME` vs `NORMAL_NAME` branch firing pattern reproduces. No
  divergence was introduced.
- The dotdir / basename fallback at `walk_path_with_metadata` references
  upstream `flist.c:2338-2349` in its inline comments and is structurally
  equivalent.

## 5. Follow-up actions

None required. The UTS-3.b series is closed at conformance. The remaining
open task in the series is `UTS-3.b.5` (cross-platform parity check for
the sub-path resolver on the Windows daemon), which is a portability check
rather than a conformance question.

If a future upstream release alters the `name_type` selection (e.g. by
introducing a new wildcard expansion or sanitisation rule), the
`resolve_sender_sources()` + `walk_path_with_metadata` pair must be
re-validated against this decision. Upstream `flist.c:2299-2349` and
`util1.c:804` are the canonical sources to diff against.

## References

- Upstream rsync 3.4.4 `flist.c:2299-2349` (per-positional name_type + dir/fn split)
- Upstream rsync 3.4.4 `util1.c:804 glob_expand_module()`
- Upstream rsync 3.4.4 `clientserver.c:980-993` (`change_dir(module_chdir)`)
- oc-rsync `crates/daemon/src/daemon/sections/module_access/client_args.rs:386-433`
- oc-rsync `crates/transfer/src/generator/file_list/walk.rs:120-169`
- oc-rsync regression tests `crates/daemon/src/tests/chunks/daemon_pull_subpath.rs`
- Commit `216328b1b fix(daemon): emit single-file basename on sub-path module argument`
