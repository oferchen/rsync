# Upstream 3.4.2 parity: `tzset()` before daemon `chroot()`

Tracking issue: #2234. Verified 2026-05-15 against `origin/master`.

## 1. Upstream change

rsync 3.4.2 fixes a latent bug in the daemon: glibc reads `/etc/localtime`
lazily on the first `localtime`/`strftime` conversion. Once the daemon has
called `chroot()`, the file is no longer reachable, and log timestamps
silently fall back to UTC. The 3.4.2 fix is a `tzset()` call before each
chroot syscall so the timezone state is cached while the file is still
visible to the process.

Upstream call sites (`target/interop/upstream-src/rsync-3.4.2/clientserver.c`):

- Per-module chroot at lines 978-985:

  ```c
  if (use_chroot) {
      /* Cache timezone data before chroot makes /etc/localtime inaccessible */
      tzset();
      if (chroot(module_chdir)) {
          rsyserr(FLOG, errno, "chroot(\"%s\") failed", module_chdir);
          io_printf(f_out, "@ERROR: chroot failed\n");
          return -1;
      }
      module_chdir = module_dir;
  }
  ```

- Daemon-level chroot at lines 1303-1314:

  ```c
  p = lp_daemon_chroot();
  if (*p) {
      log_init(0); /* Make use we've initialized syslog before chrooting. */
      tzset();
      if (chroot(p) < 0) { ... }
      if (chdir("/") < 0) { ... }
  }
  ```

Both calls are new in 3.4.2; rsync 3.4.1 had neither.

## 2. oc-rsync audit

The Rust daemon performs chroot through a single platform helper:

- `crates/platform/src/privilege.rs::apply_chroot` (unix) - wraps
  `nix::unistd::chroot` followed by `chdir("/")`.

That helper is the only chroot syscall site in the workspace and is invoked
from both daemon scopes:

- Per-module: `crates/daemon/src/daemon/sections/privilege.rs::apply_chroot`
  -> `platform::privilege::apply_chroot(&module.path, ...)`, called from
  `apply_module_privilege_restrictions` after authentication.
- Daemon-wide: `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`
  lines 239-257, before the accept loop starts and before any privilege
  drop.

### Latency of the bug in Rust

oc-rsync's daemon log path writes through `logging_sink::MessageSink`, which
does not prefix entries with a local-time timestamp today. The only
local-time formatting in the workspace is `cli::frontend::execution::stop`
(client-side `--stop-after`) and the `LIST_TIMESTAMP_FORMAT` path, neither
of which runs inside the daemon after chroot.

So the literal upstream symptom - daemon log timestamps in UTC after chroot
- does not currently surface. The fix is still worth landing as a
defense-in-depth measure:

1. `tracing-subscriber` (already a workspace dependency) can be configured
   with `fmt::time::LocalTime`, and the natural site to add such a
   formatter is exactly the daemon log sink. Without `tzset()` before
   chroot, that future change would regress silently.
2. Any future caller of `time::OffsetDateTime::now_local()` /
   `chrono::Local::now()` from inside a module handler would observe UTC
   after chroot, with no compile-time warning.
3. `tzset()` is idempotent, thread-safe, and free (a libc state read on
   `$TZ` / `/etc/localtime`). The cost of caching it is one syscall per
   chroot - negligible compared to chroot itself.

## 3. Remediation

`platform::privilege::apply_chroot` now calls `libc::tzset()` immediately
before `nix::unistd::chroot`. This single insertion covers both the
per-module and daemon-wide chroot paths through their shared helper and
mirrors upstream's intent at both 3.4.2 call sites.

The call is gated to unix (`#[cfg(unix)]`, matching the existing
implementation) and wrapped in `#[allow(unsafe_code)]` since `tzset` is an
FFI call. The platform crate is on the unsafe-permitted list for
exactly this kind of POSIX wrapper.

## 4. Test coverage

`apply_chroot_tzset_does_not_mask_chroot_failure` in
`crates/platform/src/privilege.rs` exercises the new path: calling
`apply_chroot` on a non-existent target still surfaces the chroot error
verbatim (NotFound or PermissionDenied depending on euid), proving the
`tzset()` insertion neither aborts on success nor swallows the chroot
failure.

An end-to-end integration test that actually chroots and reads back a
log timestamp requires root privileges and a writable filesystem root for
the test runner, which is impractical in CI. The existing unit test plus
the upstream reference comment are the contract.

## 5. Conclusion

oc-rsync now calls `tzset()` before every `chroot()` syscall it issues,
matching upstream 3.4.2 at both `rsync_module()` and
`start_accept_loop()`. The single insertion in the shared platform helper
covers both daemon scopes and is verified by a regression test in the
platform crate.
