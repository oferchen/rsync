# Daemon chroot + uid/gid drop audit

Cross-references the upstream rsync 3.4.1 daemon privilege-drop sequence
(`clientserver.c`) against our implementation in `crates/daemon/` and
`crates/platform/`. The goal is a byte-faithful match for chroot ordering,
supplementary groups, and the boundary between operations that must execute
outside vs. inside the jail.

Upstream source under audit: `target/interop/upstream-src/rsync-3.4.1/clientserver.c`.

## 1. Upstream privilege-drop sequence

Upstream rsync runs two distinct chroot+drop ladders:

### 1a. Daemon-process ladder (`start_daemon`, before `accept()` loop)

`clientserver.c:1301-1339`:

1. `lp_daemon_chroot()` -> `chroot(p)` then `chdir("/")` (line 1304-1311).
2. `lp_daemon_gid()` -> `group_to_gid(p, &gid, True)` then `setgid(gid)` (line 1313-1325).
3. `lp_daemon_uid()` -> `user_to_uid(p, &uid, True)` then `setuid(uid)` (line 1326-1339).

Order is `chroot -> setgid -> setuid`. Name resolution (`group_to_gid`,
`user_to_uid`) runs *before* the chroot has taken effect because `lp_*`
parameter values are loaded into memory by `load_config(0)` at line 1295
and the helper functions consult `/etc/passwd` and `/etc/group` only when
called - so they fire before `chroot(p)` at 1304 only insofar as the helpers
are called *after* it. In practice the daemon-level ladder calls
`group_to_gid`/`user_to_uid` *after* the daemon chroot, so the daemon-chroot
target must contain a usable `/etc/passwd` and `/etc/group` (or numeric ids
must be supplied). Upstream documents this in `rsyncd.conf(5)` under
`daemon chroot`.

### 1b. Per-module ladder (`rsync_module`, after auth)

`clientserver.c:692-1045`:

1. Authentication (`auth_server`, line 759).
2. uid/gid resolution: `user_to_uid(lp_uid(i), ...)` at line 781,
   `add_a_group()` for `gid` list at line 805-820 (`getpwnam`/`getgrnam` run
   here, *before* chroot).
3. Module path normalisation (line 829-865), including the
   `/path/./inside-chroot` split that lets the admin pin a sub-path inside
   the jail.
4. Pre-exec / early-exec / pre-xfer-exec / name-converter spawn
   (line 897-970, inside `#ifdef HAVE_SETENV || HAVE_PUTENV`). All forked
   *before* the chroot so the helpers retain access to the host filesystem
   and `/etc/passwd`.
5. `chroot(module_chdir)` (line 978-985) - **outside-chroot operations end
   here**.
6. `change_dir(module_chdir, CD_NORMAL)` (line 987).
7. Munge-symlinks safety check (line 992-1004).
8. `setgid(gid_array[0])` (line 1008), `setgroups(gid_list.count, gid_array)`
   (line 1015), optional `initgroups(pw->pw_name, pw->pw_gid)` (line 1023).
9. `setuid(uid)` then optional `seteuid(uid)` (line 1033-1041).
10. `numeric_ids` adjustment based on `use_chroot` and `name_converter`
    presence (line 1187-1190): when chrooted with no name-converter and the
    module did not opt into name lookups, upstream silently flips
    `numeric_ids = -1` to disable id->name protocol traffic that would fail
    inside the jail.

Order is `setgroups -> setgid -> setuid`. The `setgroups` call must happen
while still root because the kernel rejects it from a non-root euid.

## 2. Why the order is critical

- `chroot()` requires `CAP_SYS_CHROOT` (Linux) / root (BSD/macOS). After
  `setuid(non_root)` the call fails with `EPERM`. Chroot must precede the
  uid drop.
- `setgroups()` requires `CAP_SETGID` (effectively root). Once `setuid()`
  drops to a non-root uid, `setgroups()` cannot reset the supplementary
  group list. The kernel will silently keep whatever inherited list was
  active, including `root`'s default groups, leaking privileges into the
  daemon worker.
- `setgid()` similarly must precede `setuid()`. After `setuid()` drops the
  effective uid, `setgid()` cannot lower (or change) the gid - the daemon
  would keep the parent's gid.
- Name resolution (`getpwnam_r`, `getgrnam_r`) reads `/etc/passwd`,
  `/etc/group`, and any NSS modules (e.g. `nss_systemd`, `nss_ldap`). These
  files and sockets do not exist inside a typical chroot. All name lookups
  must therefore be resolved before the chroot, with the resulting numeric
  uid/gid carried into the jail.

## 3. Our equivalent code paths

### 3a. Daemon-process ladder

| Upstream step | Our code |
|---|---|
| Parse `daemon chroot`, `uid`, `gid` | `crates/daemon/src/daemon/sections/config_parsing/global_directives.rs:725-737` (parse), `crates/daemon/src/daemon/runtime_options/config.rs:96-97` (carry into runtime opts), `crates/daemon/src/daemon/runtime_options/config.rs:397-437` (uid/gid resolution via `resolve_uid`/`resolve_gid`) |
| `chroot(p) + chdir("/")` | **Not implemented** - parsed and stored as `RuntimeOptions::daemon_chroot`, but no caller invokes `platform::privilege::apply_chroot` for the daemon-level value. Accessor at `crates/daemon/src/daemon/runtime_options/accessors.rs:46-48`. |
| `setgid(daemon_gid)` | `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:233-246` calls `drop_privileges(daemon_uid, daemon_gid, sink)` which delegates to `crates/platform/src/privilege.rs:54-68`. |
| `setuid(daemon_uid)` | Same call as above. |

The drop happens after `bind`, after `become_daemon` (Unix detach,
`accept_loop.rs:212-215`), and after `PidFileGuard::create`
(`accept_loop.rs:223-227`), matching upstream which also drops privileges
post-bind.

Name resolution for the daemon-level `uid`/`gid` happens at config-load
time (`runtime_options/resolve.rs:6-17` via `metadata::id_lookup::lookup_user_by_name`),
before any chroot, which is correct.

### 3b. Per-module ladder

| Upstream step | Our code |
|---|---|
| Auth | `crates/daemon/src/daemon/sections/module_access/transfer.rs:319-323` (preceded by `module_access::auth`). |
| uid/gid resolution | `crates/daemon/src/daemon/sections/config_parsing/module_directives.rs:163-174` and `crates/daemon/src/daemon/sections/module_parsing.rs:469-478` - both use `parse_numeric_identifier` and reject non-numeric values. |
| Module path validation | `validate_module_path` in `module_access/transfer.rs:14-42` and `finish_requires_absolute_path_with_chroot` in `module_definition/finish.rs:29-37`. |
| Pre-xfer / name-converter spawn | `module_access/transfer.rs:368-394` (name converter), `module_access/transfer.rs:453-462` (pre-xfer exec). |
| chroot | `crates/daemon/src/daemon/sections/privilege.rs:7-20` -> `crates/platform/src/privilege.rs:27-31` (`nix::unistd::chroot(path)` then `set_current_dir("/")`). |
| `setgroups` | `crates/platform/src/privilege.rs:74-100` (`set_supplementary_groups` -> `nix::unistd::setgroups` on Linux/BSD, `libc::setgroups` on Apple). |
| `setgid` | `crates/platform/src/privilege.rs:55-60` via `nix::unistd::setgid`. |
| `setuid` | `crates/platform/src/privilege.rs:62-65` via `nix::unistd::setuid`. |
| Trigger | `apply_module_privilege_restrictions` at `crates/daemon/src/daemon/sections/privilege.rs:64-77`, called from `module_access/transfer.rs:351-364`. |

Order inside `drop_privileges` is `setgroups -> setgid -> setuid`, matching
upstream (`platform/src/privilege.rs:54-68`).

## 4. Inside-chroot vs. outside-chroot operations

Upstream classification:

| Outside chroot | Inside chroot |
|---|---|
| Config load, name lookups (`user_to_uid`, `group_to_gid`, `getpwnam`) | Module-path stat, `change_dir`, all transfer I/O |
| `auth_server()` (reads secrets file) | Filter-rule application against module-relative paths |
| `start_pre_exec()` for early-exec, pre-xfer-exec, name-converter (forked before chroot) | Pre-xfer-exec parent waits for child results inside the jail |
| Reverse DNS (`client_name`) | Read-args, parse-arguments, transfer engine |
| Chmod-mode parsing (`parse_chmod`) | Munge-symlinks safety check (after chroot but before drop) |

Our code:

- **Outside**: config parsing (`crates/daemon/src/rsyncd_config/`,
  `crates/daemon/src/daemon/sections/config_parsing/`), uid/gid name
  resolution (`runtime_options/resolve.rs`), auth (in
  `module_access::auth`), reverse-DNS hooks at the listener layer.
- **Inside (after chroot in `apply_module_privilege_restrictions`)**:
  module-path validation (`Path::new(&module.path).exists()`), filter
  loading (`build_daemon_filter_rules` in
  `module_access/transfer.rs:417`), name-converter spawn
  (`module_access/transfer.rs:368-394`), pre-xfer exec
  (`module_access/transfer.rs:453-462`), the rsync transfer engine.

The chroot fixes `module.path` to "/" via the `effective_module` rebinding
in `transfer.rs:399-407` so downstream code sees a chroot-relative root,
matching upstream's `module_dir = "/"` reset at `clientserver.c:858`.

## 5. `numeric ids` vs. name-lookup interaction

Upstream behaviour (`clientserver.c:1187-1190`):

```c
if (!numeric_ids
 && (use_chroot ? lp_numeric_ids(module_id) != False && !*lp_name_converter(module_id)
                : lp_numeric_ids(module_id) == True))
    numeric_ids = -1; /* Set --numeric-ids w/o breaking protocol. */
```

Translation:

- If chroot is active **and** the module did not explicitly set
  `numeric ids = no` **and** no `name converter` is configured, upstream
  silently forces numeric-ids mode. Reason: NSS lookups inside a chroot
  without `/etc/passwd` would fail for every uid->name conversion the
  protocol asks for.
- If chroot is inactive, upstream forces numeric-ids only when the module
  explicitly set `numeric ids = yes`.

Our equivalent: `crates/daemon/src/daemon/sections/module_access/client_args.rs:350`
sets `config.flags.numeric_ids = true` in response to the module's stored
`numeric_ids` flag (`module_definition/finish.rs:85`). We do **not**
currently apply the chroot+no-name-converter implicit override that
upstream applies at `clientserver.c:1187`. As a result, a chrooted module
without an explicit `numeric ids = yes` and without a `name converter`
will attempt id->name lookups inside the jail and silently fall back to
numeric ids when the lookup fails (because `metadata::id_lookup` returns
`None`), which produces the same on-wire result but emits an extra NSS
syscall per lookup.

Name resolution itself happens pre-chroot in our code:

- Daemon-level `uid`/`gid` strings are resolved in
  `runtime_options/resolve.rs` at config-load time, before any privilege
  transition.
- Per-module `uid`/`gid` are parsed as **numeric only**
  (`parse_numeric_identifier` in
  `daemon/sections/config_helpers/value_parsing.rs:168-175`), so no name
  lookup is needed at chroot time.

The `name converter` itself - which performs id<->name conversions on
behalf of the rest of the daemon - is spawned by
`NameConverter::spawn(&expanded)` in
`crates/daemon/src/daemon/sections/module_access/transfer.rs:379` *after*
`apply_module_privilege_restrictions`. This is a divergence from upstream,
where `start_pre_exec` for `lp_name_converter` runs at line 962-969,
*before* `chroot()` at line 978. See section 7.

## 6. Test coverage

| Area | Tests |
|---|---|
| `apply_chroot` with nonexistent path | `crates/platform/src/privilege.rs:209-213` (`chroot_rejects_nonexistent_path`) |
| `apply_chroot` error kind | `crates/platform/src/privilege.rs:233-243` (`chroot_error_has_os_error_kind`) |
| `drop_privileges(None, None)` no-op | `crates/platform/src/privilege.rs:215-219` |
| `drop_privileges` failure path when root | `crates/platform/src/privilege.rs:245-254` (skipped when not root) |
| Windows impersonation no-op when no account | `crates/platform/src/privilege.rs:221-231` |
| `apply_module_privilege_restrictions` no-op when disabled | `crates/daemon/src/daemon/sections/privilege.rs:103-114, 116-129` |
| Chroot config: parse, default, empty rejected, duplicate rejected | `crates/daemon/src/rsyncd_config/tests.rs:794-826`, `crates/daemon/src/daemon/sections/config_parsing/tests.rs:1796-1834` |
| `use chroot` global override + per-module override | `crates/daemon/src/daemon/sections/config_parsing/tests.rs:1231-1308` |
| Absolute-path enforcement when chroot enabled | `crates/daemon/src/daemon/sections/module_definition/tests.rs:547-565`, `crates/daemon/src/tests/chunks/runtime_options_rejects_relative_path_with_chroot_enabled.rs` |
| Duplicate `use chroot` rejection | `crates/daemon/src/tests/chunks/runtime_options_rejects_duplicate_use_chroot_directive.rs` |

Coverage gaps:

- No integration test exercises the full chroot+setgid+setuid path under
  root in CI - the path is only reachable when running as uid 0.
- No test asserts the *order* of `setgroups -> setgid -> setuid` (relies on
  visual inspection of `drop_privileges`).
- No test covers the daemon-level chroot ladder (because it is not wired,
  see section 7).
- No test verifies that NSS lookups happen pre-chroot (relies on parser
  ordering, which is checked indirectly by config-load tests).

## 7. Discrepancies

### 7.1. Daemon-level `daemon chroot` is parsed but never enforced

`daemon_chroot` is read from `[global]` in
`config_parsing/global_directives.rs:725-737`, propagated to
`RuntimeOptions::daemon_chroot` in `runtime_options/config.rs:96-97`, and
exposed via `accessors.rs:46-48`, but no caller invokes
`platform::privilege::apply_chroot` for the daemon-level value before the
accept loop. Upstream applies this chroot at `clientserver.c:1301-1312`
before `accept()`. Our `accept_loop.rs:229-246` only invokes
`drop_privileges(daemon_uid, daemon_gid, ...)` and silently ignores
`daemon_chroot`. **Severity: high** - operators relying on the
`daemon chroot` directive get no jail isolation.

### 7.2. Per-module `uid`/`gid` reject username/groupname strings

Upstream `lp_uid`/`lp_gid` accept usernames and groupnames; resolution runs
through `user_to_uid`/`group_to_gid` at `clientserver.c:781, 805`. Our
config parsers at
`crates/daemon/src/daemon/sections/config_parsing/module_directives.rs:163-174`
and `crates/daemon/src/daemon/sections/module_parsing.rs:469-478` use
`parse_numeric_identifier` which rejects anything non-numeric. The
daemon-level `uid`/`gid` directives **do** support names
(`runtime_options/config.rs:411,434`), so the discrepancy is per-module
only. **Severity: medium** - breaks existing `rsyncd.conf` files that
specify `uid = nobody` per module.

### 7.3. Name-converter spawn happens after chroot, not before

Upstream forks the name-converter helper at `clientserver.c:962-969`,
**before** `chroot(module_chdir)` at line 978, so the helper inherits
host-filesystem visibility (especially `/etc/passwd`). Our code spawns
the name converter at
`crates/daemon/src/daemon/sections/module_access/transfer.rs:368-394`,
**after** `apply_module_privilege_restrictions` at line 351. The helper
therefore inherits the chroot view, which can break id<->name conversions
when `/etc/passwd` is not present in the jail. **Severity: medium** -
deployments that rely on `name converter` inside chrooted modules will
see resolution failures unless the helper binary plus its NSS dependencies
are mounted into the jail.

### 7.4. Pre-xfer / early / post-xfer exec spawn after chroot

Same root cause as 7.3. Upstream at `clientserver.c:897-970` forks all
exec hooks before the chroot. Our `module_access/transfer.rs:453-462`
runs `run_pre_xfer_exec` post-chroot. **Severity: medium** - any exec
hook that depends on host-filesystem state (e.g. `/usr/bin/env`,
`/etc/...` config) needs that state mounted into the jail, contrary to
the upstream contract documented in `rsyncd.conf(5)`.

### 7.5. `numeric ids` implicit override on chroot is not applied

Upstream `clientserver.c:1187-1190` silently flips `numeric_ids = -1` when
chroot is active and no `name converter` is configured. Our daemon flag
plumbing in `module_access/client_args.rs:350` only honours the explicit
`numeric ids` directive. Behaviour is observationally similar (lookups
fail and the receiver falls back to numeric), but extra NSS syscalls
occur per id and the protocol does not get the `--numeric-ids`
optimisation flag set. **Severity: low** - cosmetic / perf, not
correctness.

### 7.6. Windows impersonation is implemented but not wired

`platform::privilege::drop_privileges_windows` exists at
`crates/platform/src/privilege.rs:117-127, 142-196` but is never invoked
from `crates/daemon/`. On Windows the daemon currently performs no
privilege drop at all (the Unix `drop_privileges` stub at line 102-106
returns `Ok(())`). **Severity: medium** for Windows daemon deployments;
the Windows daemon lacks any module-level identity transition.

### 7.7. `munge symlinks` safety check is not run

Upstream at `clientserver.c:992-1004` performs a stat against
`SYMLINK_PREFIX` after chroot and aborts with `RERR_UNSUPPORTED` if a
collision exists. We compute `effective_munge_symlinks` for filtering
purposes (`crates/daemon/src/daemon/sections/symlink_munge.rs`) but never
run the stat-based collision check. **Severity: low** - the check guards
a narrow misconfiguration scenario.

### 7.8. `chdir(module_chdir)` after chroot is not explicit

Upstream at `clientserver.c:987-990` calls `change_dir(module_chdir,
CD_NORMAL)` after chroot to land the process inside the inner-path when
the module path used the `/outer/./inner` syntax. Our `apply_chroot` calls
`std::env::set_current_dir("/")` (`platform/src/privilege.rs:29`) but the
module's inner-path component (the `module_dir` after `/./`) is never
applied. We do not currently parse the `/./` split in module paths, so
the inner-path feature is missing entirely. **Severity: medium** for
operators using the idiomatic `/srv/rsync/./module` pattern.

## Security implications

- **7.1 (daemon chroot not wired)** is the highest-risk gap: an operator
  who configures `daemon chroot = /var/lib/rsyncd` reasonably expects
  process-level jail isolation before the daemon ever accepts a
  connection. Today they get none. A vulnerability in our listener,
  protocol parser, or auth code therefore retains full filesystem reach.
- **7.6 (Windows no-op drop)** means a Windows daemon retains the
  service-account identity (typically `LocalSystem` or a privileged
  service user) for the entire transfer. Per-module `uid`/`gid` settings
  are silently ignored on Windows.
- **7.3 / 7.4 (helpers spawned post-chroot)** can cascade into denial of
  service if the helper binary is not present in the jail, but they do
  not weaken security.
- The supplementary-group ordering in `drop_privileges`
  (`setgroups -> setgid -> setuid`) is correct and matches upstream.
- Name resolution happens pre-chroot in every code path that currently
  resolves names, so the `getpwnam_r`/`getgrnam_r` jail-failure mode does
  not occur.

## Remediation summary

1. Wire `RuntimeOptions::daemon_chroot` into `accept_loop.rs` between
   `become_daemon` and `drop_privileges`, mirroring
   `clientserver.c:1301-1312`. (Discrepancy 7.1.)
2. Replace `parse_numeric_identifier` with `resolve_uid`/`resolve_gid` for
   per-module `uid`/`gid` directives. (Discrepancy 7.2.)
3. Reorder `transfer.rs` so name-converter and pre-xfer-exec hooks spawn
   *before* `apply_module_privilege_restrictions`. (Discrepancies 7.3,
   7.4.)
4. Apply the implicit `numeric_ids = -1` override when chrooted with no
   name converter. (Discrepancy 7.5.)
5. Invoke `platform::privilege::drop_privileges_windows` from
   `module_access/transfer.rs` on Windows when `module.uid`/`gid` map to
   an account name. (Discrepancy 7.6.)
6. Add the `munge symlinks` collision stat after chroot.
   (Discrepancy 7.7.)
7. Parse `/path/./inner` and chdir to the inner path post-chroot.
   (Discrepancy 7.8.)
