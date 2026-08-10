# Daemon chroot + privilege drop audit

Tracking issue: oc-rsync task #2129.

Audit of the daemon's chroot and privilege-dropping behaviour compared
to upstream rsync 3.4.1. Covers the two-stage privilege ladder (daemon-level
and per-module), `numeric ids` interaction, `munge symlinks`, path
sanitization when chroot is disabled, and cross-platform behaviour.

Upstream source: `target/interop/upstream-src/rsync-3.4.1/clientserver.c`,
`loadparm.c`, `daemon-parm.txt`.

oc-rsync source: `crates/daemon/src/`, `crates/platform/src/privilege.rs`,
`crates/transfer/src/sanitize_path.rs`.

---

## 1. Current state

### 1.1 Daemon-level privilege ladder

Upstream rsync applies a two-stage privilege ladder. The first stage runs
once at daemon startup, before the accept loop:

| Step | Upstream (`clientserver.c:1301-1339`) | oc-rsync |
|------|---------------------------------------|----------|
| Parse `daemon chroot` | `lp_daemon_chroot()` | Parsed into `RuntimeOptions::daemon_chroot` via `config_parsing/global_directives.rs`. |
| Apply `daemon chroot` | `chroot(p)` + `chdir("/")` at line 1304-1311 | **Not wired.** Parsed and stored but no code invokes `platform::privilege::apply_chroot` for the daemon-level value. |
| Parse `daemon gid` | `lp_daemon_gid()` | Parsed and resolved by name at config-load time (`runtime_options/resolve.rs`). |
| Apply `daemon gid` | `setgid(gid)` at line 1320 | Called via `drop_privileges(daemon_uid, daemon_gid, sink)` in `accept_loop.rs:233-246`. |
| Parse `daemon uid` | `lp_daemon_uid()` | Parsed and resolved by name at config-load time. |
| Apply `daemon uid` | `setuid(uid)` at line 1333 | Same `drop_privileges` call. |

Both implementations run uid/gid name resolution before any chroot and
drop privileges after binding the TCP listener, which is correct. The
daemon-level privilege drop in oc-rsync follows the security-critical
ordering: `setgroups -> setgid -> setuid` (in `platform/src/privilege.rs:54-68`).

### 1.2 Per-module privilege ladder

The second stage runs per connection after module authentication:

| Step | Upstream (`clientserver.c:692-1045`) | oc-rsync |
|------|--------------------------------------|----------|
| Authentication | `auth_server()` at line 759 | `module_access/authentication.rs` via `handle_authentication`. |
| uid/gid resolution | `user_to_uid(lp_uid(i))` at line 781 (accepts names). `add_a_group()` at line 805 (accepts names, `*` wildcard, multiple groups). | `parse_numeric_identifier` in `module_directives.rs:164,170` - **numeric only**. Single uid and single gid per module. |
| Module path parsing (`/./` split) | `strstr(module_dir, "/./")` at line 844 splits into outer chroot dir and inner `module_dir`. `sanitize_paths = 1` when `module_dirlen > 0`. | **Not implemented.** No `/./` path splitting. The entire `path` is used as the chroot target. |
| Pre-exec / name-converter spawn | `start_pre_exec()` at line 934-969, **before** `chroot()` at line 978. | Spawned **after** `apply_module_privilege_restrictions` in `module_access/transfer.rs:368-394`. |
| `chroot(module_chdir)` | `chroot()` at line 979 then `change_dir(module_chdir, CD_NORMAL)` at line 987. | `platform::privilege::apply_chroot(path)` via `apply_module_privilege_restrictions` in `privilege.rs:64-77`. Calls `nix::unistd::chroot` then `set_current_dir("/")`. |
| `munge_symlinks` safety check | `stat(SYMLINK_PREFIX)` at line 998. Aborts if a `/rsyncd-munged` directory exists inside the jail. | **Not wired.** `effective_munge_symlinks()` computed but collision stat not performed. |
| `setgid` + `setgroups` | `setgid(gid_array[0])` at line 1008, `setgroups(gid_list.count, gid_array)` at line 1015. | `drop_privileges` in `platform/src/privilege.rs:54-68` calls `set_supplementary_groups` then `setgid` then `setuid`. |
| `setuid` | `setuid(uid)` at line 1033 | Same function, last step. |
| Module path rebase | `module_dir = "/"` at line 858 (or inner path). | `transfer.rs:399-407` rebases path to `PathBuf::from("/")` when `use_chroot` is true. |

### 1.3 `use chroot` default handling

Upstream `daemon-parm.txt` declares `use_chroot` as `BOOL3` with default
`Unset`. When unset, upstream auto-detects at `clientserver.c:829-841`:

1. If the module path contains `/./`, force chroot on.
2. Else probe with `chroot("/")` - if it succeeds, use chroot; if EPERM
   (non-root), fall back to no-chroot with a log message.

oc-rsync defaults `use_chroot` to `true` unconditionally
(`rsyncd_config/parser.rs:472`). This means a non-root daemon with no
explicit `use chroot = false` will attempt chroot and fail with
`EPERM`, instead of gracefully falling back as upstream does.

### 1.4 Chroot and `numeric ids` interaction

Upstream at `clientserver.c:1187-1190` silently forces `numeric_ids = -1`
when:

- chroot is active, **and**
- the module did not explicitly set `numeric ids = no`, **and**
- no `name converter` is configured.

Rationale: NSS lookups inside a chroot without `/etc/passwd` would fail
for every uid-to-name conversion the protocol asks for. Setting
`numeric_ids = -1` suppresses the protocol's id-to-name exchange without
requiring the client to send `--numeric-ids`.

oc-rsync at `module_access/client_args.rs:350` sets
`config.flags.numeric_ids = true` only when the module's stored
`numeric_ids` field is explicitly true. The chroot+no-name-converter
implicit override is not applied.

### 1.5 Munge symlinks

Upstream defaults `munge_symlinks` to `Unset` (BOOL3). At runtime
(`clientserver.c:992-993`):

```c
if ((munge_symlinks = lp_munge_symlinks(module_id)) < 0)
    munge_symlinks = !use_chroot || module_dirlen;
```

When chroot is active with no inner path (`module_dirlen == 0`), munging
is off. When chroot is inactive or an inner path is present, munging is
on. After computing the effective value, upstream checks for a
`/rsyncd-munged` directory inside the jail and aborts if one exists,
since an attacker could create that directory to defeat the munge
protection.

oc-rsync implements `effective_munge_symlinks()` in
`daemon/module_state/definition.rs:245-247` with the correct auto-logic
(`munge_symlinks.unwrap_or(!use_chroot)`). The munge/unmunge functions
exist in `metadata::symlink_munge` and are re-exported from
`daemon/sections/symlink_munge.rs` with passing roundtrip tests.

Missing: the `/rsyncd-munged` directory collision check (upstream
lines 994-1003), and the `module_dirlen` factor in the auto-detection
(since `/./` path splitting is not implemented).

### 1.6 Path sanitization when chroot is disabled

Upstream enables `sanitize_paths = 1` in two situations:

1. When the module uses chroot with an inner path (`module_dirlen > 0`,
   line 989-990).
2. When the module does not use chroot (the `sanitize_paths` global
   is set in `options.c` based on `am_daemon`).

The `sanitize_path()` function in `util1.c:1035-1108` strips `..`
components, collapses `//`, and prevents path traversal above the module
root.

oc-rsync has a `sanitize_path` module in `crates/transfer/src/sanitize_path.rs`
that mirrors upstream `util1.c:sanitize_path()`. It strips `..`
components and collapses redundant slashes. This is used during file-list
processing (`generator/filters.rs:152`). The daemon does not have an
explicit `sanitize_paths` flag toggle per module, but the transfer engine
applies path sanitization unconditionally during file-list exchange.

### 1.7 Platform implementation

| Platform | chroot | uid/gid drop | Notes |
|----------|--------|--------------|-------|
| Linux | `nix::unistd::chroot` + `set_current_dir("/")`. `nix::unistd::setgroups`. | `nix::unistd::setgid` + `nix::unistd::setuid`. | Full support. |
| macOS | Same `nix::unistd::chroot`. `libc::setgroups` fallback (nix does not expose setgroups on Apple). | Same nix calls. | Full support. Requires SIP-exempted binary or root. |
| Windows | No-op with stderr warning. | `drop_privileges_windows` using `LogonUserW` + `ImpersonateLoggedOnUser` exists in `platform/src/privilege.rs:142-196` but is **never called** from daemon code. Unix `drop_privileges` no-op returns `Ok(())`. | No chroot equivalent. No privilege drop wired. |

---

## 2. Gaps

### 2.1 Daemon-level `daemon chroot` is parsed but never enforced

**Risk: High.**

`daemon_chroot` is read from `[global]` and stored in
`RuntimeOptions::daemon_chroot` (accessor at
`runtime_options/accessors.rs:46-48`), but `accept_loop.rs` never
invokes `apply_chroot`. An operator who configures
`daemon chroot = /var/lib/rsyncd` receives no jail isolation before
the accept loop. A vulnerability in the listener, protocol parser, or
auth code retains full filesystem reach.

### 2.2 Per-module uid/gid rejects name strings

**Risk: Medium.**

Upstream `rsyncd.conf` accepts `uid = nobody` and `gid = nogroup` for
per-module directives. oc-rsync's per-module parser uses
`parse_numeric_identifier` which rejects non-numeric values. Existing
`rsyncd.conf` files using name-based uid/gid per module will fail to
load. The daemon-level `uid`/`gid` directives correctly accept names
(resolved via `metadata::id_lookup::lookup_user_by_name`), so only the
per-module path is affected.

### 2.3 No `/./` inner-path chroot split

**Risk: Medium.**

Upstream supports `/srv/rsync/./module` to chroot to `/srv/rsync/` then
`chdir` into `module/`. This is the documented idiom in
`rsyncd.conf(5)`. oc-rsync does not parse the `/./` separator, so the
entire path is used as the chroot root. Modules relying on the inner-path
pattern get incorrect chroot boundaries.

### 2.4 `use chroot` defaults to `true` instead of auto-detect

**Risk: Medium.**

Upstream defaults `use chroot` to `Unset` and auto-detects based on
the process's ability to call `chroot("/")`. A non-root daemon silently
falls back to no-chroot with a log message. oc-rsync defaults to `true`,
causing non-root daemons to fail with `EPERM` at module access time
unless the operator explicitly sets `use chroot = false`.

### 2.5 Name-converter and exec hooks spawn after chroot

**Risk: Medium.**

Upstream spawns the name-converter helper and pre-xfer/early-exec hooks
before `chroot()` so they inherit host-filesystem visibility (access to
`/etc/passwd`, helper binaries, etc.). oc-rsync spawns these after
`apply_module_privilege_restrictions`, so they run inside the chroot jail.
Deployments relying on these hooks need the helper binaries and
their dependencies mounted into the jail, contrary to upstream's
documented behaviour.

### 2.6 Chroot + `numeric ids` implicit override not applied

**Risk: Low.**

When chroot is active and no `name converter` is configured, upstream
silently forces `numeric_ids = -1` to suppress uid-to-name protocol
traffic. oc-rsync does not apply this override. The transfer still
works because `metadata::id_lookup` returns `None` inside the jail,
causing a fallback to numeric IDs. However, the protocol still exchanges
id-to-name mappings (which all fail), adding unnecessary wire overhead
and per-lookup NSS syscalls.

### 2.7 `munge symlinks` collision check not performed

**Risk: Low.**

Upstream stats `/rsyncd-munged` after chroot and aborts if the directory
exists, since an attacker could use it to defeat munge protection.
oc-rsync computes the effective munge flag but never performs the
collision check. The practical risk is narrow - it requires an attacker
to have write access inside the module root to create the directory.

### 2.8 Windows daemon has no privilege drop

**Risk: Medium (Windows deployments only).**

`drop_privileges_windows` with `LogonUserW` + `ImpersonateLoggedOnUser`
exists in `platform/src/privilege.rs` but is never called from
`crates/daemon/`. The Unix `drop_privileges` no-op on non-Unix returns
`Ok(())`, so a Windows daemon retains the launching user's full
privileges for all module transfers.

### 2.9 Per-module gid does not support multiple groups or `*` wildcard

**Risk: Low.**

Upstream `gid` accepts a comma-separated list of groups and the `*`
wildcard (which expands to all groups of the resolved user via
`getgrouplist`/`initgroups`). oc-rsync accepts a single numeric gid
per module. The supplementary group list is set to a single-element
array. This limits fine-grained group-based access control inside the
jail.

---

## 3. Risk assessment summary

| ID | Gap | Severity | Exploitability | Impact |
|----|-----|----------|----------------|--------|
| 2.1 | Daemon-level chroot not enforced | High | Low (requires daemon vulnerability) | Full filesystem exposure before accept loop |
| 2.2 | Per-module uid/gid rejects names | Medium | High (any rsyncd.conf with `uid = nobody`) | Config fails to load |
| 2.3 | No `/./` inner-path split | Medium | Medium (documented idiom) | Incorrect chroot boundary |
| 2.4 | `use chroot` defaults true vs auto-detect | Medium | High (any non-root daemon) | EPERM on first connection |
| 2.5 | Exec hooks spawn after chroot | Medium | Medium (any chrooted module with exec) | Exec hooks fail without jail provisioning |
| 2.6 | `numeric ids` implicit override missing | Low | N/A (correctness preserved) | Extra wire overhead and NSS syscalls |
| 2.7 | `munge symlinks` collision check missing | Low | Low (requires write access in jail) | Theoretical munge bypass |
| 2.8 | Windows privilege drop not wired | Medium | N/A (Windows daemon deployment) | No identity transition on Windows |
| 2.9 | Single gid, no wildcard | Low | Low (uncommon configuration) | Limited group-based access control |

---

## 4. Recommendations

### 4.1 Wire daemon-level chroot (addresses 2.1)

In `accept_loop.rs`, after binding and daemonizing but before
`drop_privileges`, call `apply_chroot(daemon_chroot)` when the
directive is present. Order: `chroot -> setgid -> setuid`, matching
upstream `clientserver.c:1301-1339`.

### 4.2 Support name-based uid/gid in per-module directives (addresses 2.2)

Replace `parse_numeric_identifier` for `uid`/`gid` in
`module_directives.rs` with a two-phase parser: try numeric first,
then fall back to `metadata::id_lookup::lookup_user_by_name` /
`lookup_group_by_name`. Name resolution must happen at config-load
time (before any chroot).

### 4.3 Implement `/./` chroot path split (addresses 2.3)

In the module path normalisation, detect the `/./` separator. Split
the path into `module_chdir` (the chroot target) and `module_dir`
(the inner path to `chdir` into after chroot). Set
`module_dirlen` accordingly, and enable `sanitize_paths` when
`module_dirlen > 0`.

### 4.4 Auto-detect `use chroot` when unset (addresses 2.4)

Change the `use_chroot` default from `true` to a tri-state
(`Option<bool>`) that mirrors upstream's `BOOL3`. When `None`, probe
with `chroot("/")` at module access time: succeed means use chroot,
EPERM means fall back to no-chroot with a log message.

### 4.5 Move exec hook spawns before chroot (addresses 2.5)

Reorder `process_approved_module` in `transfer.rs` so that
`NameConverter::spawn`, `run_early_exec`, and `run_pre_xfer_exec`
execute before `apply_module_privilege_restrictions`. This matches
upstream's ordering at `clientserver.c:897-978` and ensures exec
hooks have host-filesystem visibility.

### 4.6 Apply implicit `numeric ids` override on chroot (addresses 2.6)

After `apply_module_privilege_restrictions` and before building the
server config, check: if `use_chroot` is true, `numeric_ids` was not
explicitly set by the module, and no `name_converter` is configured,
then force `config.flags.numeric_ids = true`. This suppresses
unnecessary id-to-name protocol exchanges.

### 4.7 Add `munge symlinks` collision check (addresses 2.7)

After chroot (or after `chdir` to module path when not chrooted),
stat `/rsyncd-munged` (without trailing slash). If it exists and is a
directory, abort with `RERR_UNSUPPORTED`. Mirror upstream
`clientserver.c:994-1003`.

### 4.8 Wire Windows privilege drop (addresses 2.8)

In `apply_module_privilege_restrictions`, add a `#[cfg(windows)]`
branch that calls `drop_privileges_windows` with the module's uid
translated to a Windows account name. Requires mapping numeric UIDs
to account names or accepting account names directly.

### 4.9 Support multiple gids and `*` wildcard (addresses 2.9)

Extend the `gid` module directive to accept a comma-separated list.
When the value is `*`, use `getgrouplist` (via nix or libc) to
enumerate all groups for the resolved uid. Pass the full group array
to `setgroups` instead of the single-element array.
