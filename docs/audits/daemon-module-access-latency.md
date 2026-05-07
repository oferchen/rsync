# Daemon Module Access Check Latency

Tracking issue: #1040.

Profiles the per-connection latency of the daemon module access path -
from the client sending the module name through the point where the
session begins serving the transfer. Target: < 1 ms p99 module access
at 1K connections/s on commodity Linux hosts.

## 1. Module access check sequence

Each accepted TCP connection runs through `serve_connections()` in
`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`,
which spawns a per-session thread. The session drives the module
request flow in `crates/daemon/src/daemon/sections/module_access/request.rs`
(`respond_with_module_request`), which interacts with
`crates/daemon/src/daemon/module_state/`. Phases, in order:

1. Module lookup. Linear scan over `Arc<Vec<ModuleRuntime>>`
   (`modules.iter().find(|m| m.name == request)`).
2. Bandwidth limiter rebind (`apply_module_bandwidth_limit`).
3. Optional reverse-DNS lookup, only when a host pattern needs it
   (`module_state/hostname.rs::resolve_peer_hostname`).
4. `hosts allow` / `hosts deny` evaluation -
   `ModuleDefinition::permits()` in `module_state/definition.rs`,
   walking pre-parsed `HostPattern` enums (no per-conn regex).
5. Connection slot acquisition - `ModuleRuntime::try_acquire_connection`
   in `module_state/runtime.rs` does a CAS on `AtomicU32`, then
   optional `flock`-backed `ConnectionLimiter::acquire` for
   cross-process `max connections`.
6. Refused-options check (`refuse_options` linear scan).
7. Authentication, when `auth_users` is set
   (`module_access/authentication.rs::perform_module_authentication`):
   `check_secrets_file_permissions()` stats the secrets file each
   connection, then opens, reads, and parses it line-by-line via
   `platform::secrets`.
8. `read only` / `write only` enforcement at `process_approved_module`.
9. `path` chroot + uid/gid drop (Unix only) inside the session
   handler before transfer, using `metadata::id_lookup` (NSS
   `getpwnam_r` / `getgrnam_r`) for non-numeric user/group names.

## 2. Suspected costs

- Secrets-file open + stat + line-parse on every authenticated
  connection (step 7).
- `chroot(2)` + `chdir(2)` syscalls on every `use chroot = yes`
  module (step 9).
- `getpwnam_r` / `getgrnam_r` NSS lookups behind glibc nscd; cold
  lookups can hit LDAP / SSSD and dominate latency (step 9).
- Linear scans over `modules`, `hosts_allow`, `hosts_deny`,
  `refuse_options`, `auth_users` - O(n) but n is small in practice.
- Reverse-DNS lookup when any module references hostname patterns -
  bounded by resolver timeout, currently uncached across connections.

Hosts patterns are parsed at config load (`HostPattern::parse` in
`config_helpers/host_pattern.rs`), so the original "regex compile per
connection" hypothesis does not apply to the current code.

## 3. Profile plan

Instrument `respond_with_module_request` to wrap each phase in
`std::time::Instant::now()` deltas, emit nanos via `tracing` at
`debug`, and aggregate into a `hdrhistogram::Histogram<u64>` keyed
by phase name. Phases to time:

- `lookup`, `bandwidth`, `reverse_dns`, `permits`,
  `acquire_slot`, `refuse_options`, `auth_secrets_open`,
  `auth_secrets_parse`, `chroot`, `id_resolve`.

Bench harness: `tools/bench/daemon_access_bench.rs` driving 1K cold
TCP connections via `tokio` against a daemon configured with one
authenticated module. Run under `perf record -g` and
`strace -c -p <pid>` for syscall counts. Target host: bench container
(`localhost/oc-rsync-bench:latest`), pinned to 4 cores.

## 4. Optimization candidates

- Cache parsed secrets files keyed by `(path, mtime, ino)` behind a
  `RwLock<HashMap>` with a 5 s TTL revalidation; mmap when the file
  is larger than 4 KiB so re-reads avoid `read(2)`.
- Memoize `getpwnam_r` / `getgrnam_r` in a `Mutex<LruCache<String, u32>>`
  with a 30 s TTL inside `metadata::id_lookup`; invalidate on
  `SIGHUP` to mirror upstream's reload semantics.
- Pre-resolve the chroot directory's canonical path at config load
  and cache the open `DirFd` so each session can `fchdir` instead
  of re-walking the path component lookup chain.
- Replace the `modules` linear lookup with a
  `HashMap<&str, ModuleRuntime>` once module count exceeds a small
  threshold (e.g., 16).
- Cache reverse-DNS results (`IpAddr -> Option<String>`) for the
  duration of one accept-loop tick with a small TTL.

## 5. Pass criteria

- p99 module access latency < 1 ms at 1K conn/s sustained on the
  bench container.
- p50 < 200 us on the same load.
- No more than 2 syscalls per phase outside of authentication
  (verified via `strace -c`).
- Zero per-connection allocation in `permits()` (verified via
  `dhat-rs`).
- Bench results checked into
  `docs/benchmarks/daemon-access-latency.md` on the PR that lands
  the optimizations.
