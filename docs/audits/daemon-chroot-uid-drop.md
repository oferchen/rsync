# Daemon chroot + uid/gid drop conformance (#2129)

Cross-references upstream rsync 3.4.1 privilege-reduction sequences against
oc-rsync's implementation in `crates/daemon/` and `crates/platform/`.

Upstream sources: `target/interop/upstream-src/rsync-3.4.1/clientserver.c`,
`main.c`, `daemon-parm.txt`.

## 1. Upstream chroot handling

Upstream has two distinct chroot ladders - one at daemon startup and one per
module connection.

### 1.1 Daemon-level chroot (`start_daemon`, before accept loop)

`clientserver.c:1301-1312`:

```c
p = lp_daemon_chroot();
if (*p) {
    log_init(0);                   // init syslog before chroot
    if (chroot(p) < 0) { ... }    // line 1304
    if (chdir("/") < 0) { ... }   // line 1308
}
```

This applies once before the accept loop, jailing the entire daemon process.
The global `daemon chroot` directive (`daemon-parm.txt` line 4: `STRING
daemon_chroot NULL`) defaults to NULL (no daemon-level chroot).

### 1.2 Per-module chroot (`rsync_module`, after auth)

`clientserver.c:704,829-985`:

The `use chroot` directive is a `BOOL3` (`daemon-parm.txt` line 68: `BOOL3
use_chroot Unset`). Upstream handles the three states:

1. **Explicit `true`**: chroot is applied.
2. **Explicit `false`**: chroot is skipped.
3. **Unset (default -1)**: upstream probes with `chroot("/")` at line 832.
   If the probe succeeds, `use_chroot = 1`. If it fails (EPERM - non-root
   daemon), upstream falls back to `use_chroot = 0` and logs a message.

Path remapping with the `/path/./inner` syntax (line 844-859):

- The path is split at `/./`. The prefix becomes the `module_chdir` (the
  chroot target). The suffix becomes `module_dir` (the inner working
  directory).
- Without `/./`, the entire path becomes `module_chdir`, and `module_dir`
  is set to `"/"` with `module_dirlen = 1`.
- `full_module_path` preserves the original path for logging.

Chroot application (line 978-988):

```c
if (use_chroot) {
    if (chroot(module_chdir)) { ... }  // line 979
    module_chdir = module_dir;         // rebase for chdir
}
if (!change_dir(module_chdir, CD_NORMAL))  // line 987
    return path_failure(...);
```

The chroot happens after auth but before `setgid`/`setuid` and before the
transfer engine starts.

### 1.3 Path sanitization interaction

After chroot, upstream enables `sanitize_paths` when `module_dirlen > 0`
(line 989-990), preventing clients from escaping the module sub-path via
`../` sequences.

## 2. Upstream uid/gid drop

### 2.1 Daemon-level drop

`clientserver.c:1313-1339`, after daemon chroot:

1. `lp_daemon_gid()` - resolve group name via `group_to_gid()`, then
   `setgid(gid)` (line 1319).
2. `lp_daemon_uid()` - resolve user name via `user_to_uid()`, then
   `setuid(uid)` (line 1332). Updates `our_uid` and `am_root`.

Note: no `setgroups()` call at the daemon level. The daemon-level drop is
simpler than the per-module drop.

### 2.2 Per-module drop

`clientserver.c:776-1045`:

**uid resolution** (line 779-788):

- If `lp_uid(module_id)` is non-empty, resolve via `user_to_uid()` (accepts
  both names and numeric strings). If empty and running as root, defaults to
  `NOBODY_USER`.

**gid resolution** (line 790-821):

- If `lp_gid(module_id)` is non-empty, parse as comma-separated list. The
  special value `"*"` triggers `getgrouplist()` or `initgroups()` to obtain
  all groups for the target uid. Multiple gid values accumulate into
  `gid_list`.
- If empty and running as root, defaults to `NOBODY_GROUP`.

**Drop sequence** (line 1006-1045), in strict order:

1. `setgid(gid_array[0])` - set primary group (line 1008).
2. `setgroups(gid_list.count, gid_array)` - set supplementary groups
   (line 1015, `#ifdef HAVE_SETGROUPS`). Must happen while still root.
3. `initgroups(pw->pw_name, pw->pw_gid)` - alternative when `gid = "*"` and
   `getgrouplist()` is unavailable (line 1023, conditional).
4. `our_gid = MY_GID()` - cache new gid (line 1029).
5. `setuid(uid)` + `seteuid(uid)` - drop user privileges (line 1033-1037,
   `seteuid` gated by `#ifdef HAVE_SETEUID`). Irreversible.
6. `our_uid = MY_UID(); am_root = (our_uid == ROOT_UID)` - recompute root
   status (line 1043-1044).

The ordering is critical:

- `setgroups()` requires `CAP_SETGID` (effectively root). After `setuid()`
  drops privileges, `setgroups()` would be rejected.
- `setgid()` must precede `setuid()` for the same reason.
- `chroot()` must precede all of the above because it requires
  `CAP_SYS_CHROOT`.

## 3. Current oc-rsync implementation

### 3.1 Daemon-level chroot

- **Parsed**: `config_parsing/global_directives.rs:710-737` parses
  `daemon chroot`, stores in `GlobalParseState::daemon_chroot`.
- **Propagated**: `runtime_options/config.rs:96-97` carries it to
  `RuntimeOptions::daemon_chroot`.
- **Accessor**: `runtime_options/accessors.rs:46-48`.
- **NOT APPLIED**: `server_runtime/accept_loop.rs` never calls
  `platform::privilege::apply_chroot()` for the daemon-level value. The
  `drop_privileges(daemon_uid, daemon_gid, sink)` call at line 233-246
  silently ignores `daemon_chroot`.

### 3.2 Daemon-level uid/gid drop

- **Implemented**: `accept_loop.rs:229-246` calls
  `drop_privileges(daemon_uid, daemon_gid, sink)` after bind, daemonize, and
  PID file creation. Delegates to `platform/src/privilege.rs:54-68`.
- Name resolution (`runtime_options/config.rs:397-437`) happens at
  config-load time, before any chroot - correct.

### 3.3 Per-module chroot

- **Implemented**: `sections/privilege.rs:7-20` calls
  `platform::privilege::apply_chroot(module_path)` when `module.use_chroot`
  is true. Called from `module_access/transfer.rs:346-364`.
- `apply_chroot` (`platform/src/privilege.rs:27-31`) does
  `nix::unistd::chroot(path)` then `std::env::set_current_dir("/")`.
- After chroot, `module.path` is remapped to `"/"` via `transfer.rs:399-407`.

### 3.4 Per-module uid/gid drop

- **Implemented**: `sections/privilege.rs:33-57` calls
  `platform::privilege::drop_privileges(uid, gid)`.
- The platform implementation (`platform/src/privilege.rs:54-68`) follows the
  correct ordering: `set_supplementary_groups(gid)` - `setgid(gid)` -
  `setuid(uid)`.
- `set_supplementary_groups` (`privilege.rs:75-86`) calls
  `nix::unistd::setgroups(&[gid])` on Linux/BSD, `libc::setgroups` on macOS.

### 3.5 Config parsing for uid/gid/chroot

- `use chroot`: parsed as boolean in both global
  (`global_directives.rs:387-414`) and per-module
  (`module_directives.rs:113-122`) sections. Defaults to `true` in
  `ModuleDefinitionBuilder::finish` when no explicit value is set.
- `uid` / `gid` per module: `module_directives.rs:163-174` uses
  `parse_numeric_identifier` - accepts only numeric values.
- `uid` / `gid` global: `global_directives.rs:523-585` stores as strings,
  resolved via `set_daemon_uid_from_config` / `set_daemon_gid_from_config`
  in `runtime_options/config.rs:397-437`, which accept both names and numeric
  values.

## 4. Gaps and security implications

### G1: `daemon chroot` is parsed but never enforced (HIGH)

The `daemon chroot` directive is fully parsed and stored but the accept-loop
code never calls `apply_chroot()`. An operator who configures
`daemon chroot = /var/lib/rsyncd` gets zero jail isolation before the first
connection. Any vulnerability in the listener, protocol parser, or auth code
retains full filesystem reach.

**upstream**: `clientserver.c:1301-1312` - applied before `accept()`.

### G2: Per-module `uid`/`gid` reject username/groupname strings (MEDIUM)

Upstream `lp_uid`/`lp_gid` accept usernames (`nobody`, `rsync`) and resolve
them via `user_to_uid()`/`group_to_gid()`. Our per-module parser
(`parse_numeric_identifier`) rejects non-numeric values. This breaks
`rsyncd.conf` files that specify `uid = nobody` or `gid = rsync`.

The daemon-level `uid`/`gid` **do** support names, so the inconsistency is
per-module only.

### G3: Name-converter spawn happens after chroot (MEDIUM)

Upstream forks the name-converter helper at `clientserver.c:962-969` before
`chroot()` at line 978. The helper inherits host-filesystem visibility,
including `/etc/passwd`. Our code spawns the name converter at
`module_access/transfer.rs:368-394` after `apply_module_privilege_restrictions`
at line 351. The helper therefore inherits the chroot view and may fail to
resolve names when `/etc/passwd` is absent from the jail.

### G4: Pre-xfer / early / post-xfer exec spawn after chroot (MEDIUM)

Same issue as G3. Upstream at `clientserver.c:897-970` forks all exec hooks
before the chroot. Our code runs them post-chroot. Any exec hook that depends
on host-filesystem state needs its dependencies mounted into the jail.

### G5: Chroot-test fallback for unset `use chroot` missing (MEDIUM)

Upstream tries `chroot("/")` when `use chroot` is unset (BOOL3 = -1) to
detect privileges, falling back to no-chroot if the call fails
(`clientserver.c:829-842`). Our `ModuleDefinitionBuilder::finish` defaults
`use_chroot` to `true` when unset, which will fail the entire connection
when the daemon runs unprivileged.

### G6: `setuid` not paired with `seteuid` (LOW)

Upstream calls `seteuid(uid)` after `setuid(uid)` when `HAVE_SETEUID` is
defined (`clientserver.c:1033-1037`). On Linux this is a no-op because
`setuid` already resets the effective uid, but on BSD-derived systems a
saved-set-uid could allow `seteuid(0)` after a compromise.

### G7: Supplementary groups not cleared when only uid is set (LOW)

`drop_privileges` only invokes `set_supplementary_groups` when `gid` is
`Some`. If a module sets `uid = 65534` but no `gid`, the worker keeps the
parent's supplementary groups (potentially including root's groups).

### G8: `/path/./inner` split not implemented (MEDIUM)

Upstream supports the `/srv/rsync/./module` path syntax where `/srv/rsync`
becomes the chroot target and `/module` becomes the inner working directory.
Our `apply_chroot` chroots to the entire module path and chdirs to `/`.
There is no parsing or handling of the `/./` separator.

### G9: `numeric ids` implicit override on chroot not applied (LOW)

Upstream (`clientserver.c:1187-1190`) silently forces `numeric_ids = -1` when
chrooted with no name-converter and `numeric ids` is not explicitly `no`. Our
code does not apply this implicit override. The on-wire result is the same
(lookups fail and fall back to numeric), but extra NSS syscalls occur per id.

### G10: Windows privilege drop not wired (MEDIUM)

`platform::privilege::drop_privileges_windows` exists but is never invoked
from the daemon crate. On Windows, the daemon performs no privilege drop -
the service-account identity is retained for the entire transfer.

### G11: `munge symlinks` collision check missing (LOW)

Upstream at `clientserver.c:992-1004` checks for a `rsyncd-munged` directory
after chroot and aborts if found. Our code computes `effective_munge_symlinks`
but does not perform the safety stat.

## 5. Recommendations (priority ordered)

### P0 - Wire daemon chroot (G1)

Insert `platform::privilege::apply_chroot(daemon_chroot_path)` plus
`chdir("/")` into `accept_loop.rs` between `become_daemon` and
`drop_privileges(daemon_uid, daemon_gid, ...)`. Estimated effort: 1-2 hours.

### P1 - Accept username/groupname for per-module uid/gid (G2)

Replace `parse_numeric_identifier` in `module_directives.rs:163-174` with a
resolution path that calls `metadata::id_lookup::lookup_user_by_name` /
`lookup_group_by_name` for non-numeric values. Resolution must happen
pre-chroot (currently satisfied since config parsing runs before module
access). Estimated effort: half day.

### P2 - Reorder exec/name-converter before chroot (G3, G4)

Move the name-converter spawn and pre-xfer-exec fork in
`module_access/transfer.rs` above the `apply_module_privilege_restrictions`
call. This restores upstream's contract that exec hooks run with
host-filesystem visibility. Estimated effort: half day.

### P3 - Implement chroot-test fallback (G5)

When `use_chroot` is unset in the config, probe with `chroot("/")` (then
`chdir("/")` to restore), caching the result. If the probe fails, downgrade
to `use_chroot = false` and log a warning. Estimated effort: half day.

### P4 - Parse `/path/./inner` module paths (G8)

Split module paths at `/./`. Use the prefix as the chroot target and the
suffix as the post-chroot working directory. Apply `chdir(inner)` after
`chroot(outer)` instead of always `chdir("/")`. Estimated effort: 1 day.

### P5 - Clear supplementary groups unconditionally (G7)

When running as root and either `uid` or `gid` is configured, call
`setgroups(&[primary_gid])` (or `setgroups(&[])` when no gid). Estimated
effort: 1 hour.

### P6 - Add `seteuid` on BSD targets (G6)

After `setuid(uid)`, call `seteuid(uid)` on
`target_os = "freebsd" | "netbsd" | "openbsd" | "dragonfly"`. Estimated
effort: 1 hour.

### P7 - Apply implicit `numeric_ids` override (G9)

When chrooted with no name-converter and `numeric_ids` is not explicitly
`false`, set `numeric_ids = true` in the server config. Estimated effort:
1 hour.

### P8 - Wire Windows privilege drop (G10)

Invoke `platform::privilege::drop_privileges_windows` from the module access
flow on Windows when uid/gid map to an account name. Estimated effort: half
day.

### P9 - Add `munge symlinks` collision check (G11)

After chroot, stat `SYMLINK_PREFIX` and abort with `RERR_UNSUPPORTED` if it
exists as a directory. Estimated effort: 1 hour.
