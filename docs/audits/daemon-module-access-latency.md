# Daemon Module Access Check Latency Audit

Tracking: oc-rsync task #1040.

## Summary

This audit profiles the per-connection cost of the daemon's module access
check pipeline by reading the code paths in `crates/daemon/src/`. The
pipeline runs from the moment a client sends a module name (or `#list`)
through hosts allow/deny matching, optional reverse DNS, lock file
acquisition, and challenge-response authentication against the secrets
file. The work mirrors upstream rsync 3.4.1 `clientserver.c:start_daemon`
and `clientserver.c:rsync_module`, with one notable divergence: the
secrets file is opened, stat'd, and parsed on every authenticated
request. The audit proposes five wire-compatible reductions targeting
the hottest steps - DNS, lock file, and secrets I/O - that together can
cut per-request access-check latency from O(disk I/O + DNS RTT) down to
O(memory) for the common steady-state case.

## Pipeline overview

The session-level driver `session_runtime.rs:281,300` dispatches each
client request to one of two sinks:

- `respond_with_module_list`
  (`crates/daemon/src/daemon/sections/module_access/listing.rs:26`).
- `respond_with_module_request`
  (`crates/daemon/src/daemon/sections/module_access/request.rs:232`).

Both sinks share the same access-check primitives, so the timing
analysis below covers `respond_with_module_request`, which is the
hot path for non-listing transfers.

The single-request flow:

1. Module lookup (`request.rs:246`) - linear scan of `&[ModuleRuntime]`
   by `==` on `name`.
2. Module-scope bandwidth limit application (`request.rs:258`,
   `helpers.rs:19`).
3. Reverse DNS gating + lookup (`request.rs:267`,
   `crates/daemon/src/daemon/module_state/hostname.rs:21`,
   `dns_lookup::lookup_addr`).
4. Hosts allow/deny evaluation (`request.rs:296`,
   `crates/daemon/src/daemon/module_state/definition.rs:161`).
5. Connection slot acquisition via `flock` on the shared lock file
   (`crates/daemon/src/daemon/sections/module_access/transfer.rs:210`,
   `crates/daemon/src/daemon/module_state/connection_limiter.rs:67`).
6. Refused-options check (`transfer.rs:224`).
7. Per-session `--dparam` overrides + `%MODULE%/%ADDR%` expansion
   (`transfer.rs:234-250`).
8. Authentication (`request.rs:140`,
   `crates/daemon/src/daemon/sections/module_access/authentication.rs:33`)
   only when `module.requires_authentication()` returns true.
9. Optional `early exec` script invocation (`transfer.rs:263`).

Steps 1-7 happen on every request, including unauthenticated public
modules. Step 8 fires for protected modules and is the dominant cost.

## Per-step cost from code reading

The numbers below are static estimates derived from the syscall surface
and algorithmic complexity of each step on a Linux daemon serving a
mid-sized configuration (50 modules, 2 hosts allow patterns, 5 secrets
file entries). They are intended for relative ranking, not absolute
budgets.

### 1. Module lookup - O(N), in-process, < 1 us

`modules.iter().find(|module| module.name == request)` (`request.rs:246`)
is a linear scan over an in-memory `&[ModuleRuntime]`. With 50 modules
and short names, the scan completes in well under a microsecond on any
modern CPU. The cost is dwarfed by cache-miss penalties on the first
touch of `ModuleRuntime` after a long idle period.

### 2. Bandwidth limit application - O(1), in-process, < 1 us

`apply_module_bandwidth_limit` (`helpers.rs:19`) compares config flags
and atomically swaps a `BandwidthLimiter` struct. No I/O. No
allocations on the unchanged path.

### 3. Reverse DNS - O(network), 0-50 ms

`module_peer_hostname` (`module_state/hostname.rs:21`) calls
`dns_lookup::lookup_addr`, which dispatches to libc `getnameinfo`. On
the first request that touches a hostname-style pattern, the daemon
blocks for the full DNS round trip. Cost varies wildly:

- Best case: PTR record cached by the system resolver (nscd, systemd-
  resolved) returns in 50-200 us.
- Typical case: WAN DNS round trip - 5-30 ms.
- Failure case: resolver applies the configured timeout (often 2 s) and
  retries before giving up, blocking the connection thread for seconds.

The `cache: &mut Option<Option<String>>` parameter caches the result
across allow + deny evaluation for the same connection
(`hostname.rs:31`), but the cache is per-`ModuleRuntime`-iteration on
the listing path and per-request on the access path. Two separate
connections from the same peer pay the lookup cost twice.

The `requires_hostname_lookup()` predicate (`definition.rs:183`) skips
the DNS call entirely when neither `hosts allow` nor `hosts deny`
contains a `Hostname` pattern - this is a meaningful guard for
IP-only configurations and matches upstream's `lp_reverse_lookup`
behavior in `clientserver.c:721`.

### 4. Hosts allow/deny - O(P), in-process, 1-5 us

`ModuleDefinition::permits` (`definition.rs:161`) iterates each pattern
list (`Vec<HostPattern>`) and calls `HostPattern::matches`
(`crates/daemon/src/daemon/sections/config_helpers/host_pattern.rs:136`)
per peer. The match arms cover:

- `Any`: constant time.
- `Ipv4 { network, prefix }`: shift + mask + `==`, ~5 ns.
- `Ipv6 { network, prefix }`: same as IPv4 in 128-bit, ~10 ns.
- `Hostname(HostnamePatternKind::{Exact,Suffix,Wildcard})`: byte
  comparison or, for the `Wildcard` arm, a fresh `wildcard_match`
  recursive scan over the pattern (`host_pattern.rs:243`).

Total cost for the listed configuration is well under 5 us. The
wildcard branch becomes a hotspot only when administrators stack large
numbers of `*.foo.example.org` rules and the matched hostname is long.

### 5. Connection lock file - O(disk), 100 us-5 ms

`ConnectionLimiter::acquire`
(`crates/daemon/src/daemon/module_state/connection_limiter.rs:67`) is
the most surprising cost in the pipeline. Every accepted connection
performs the following sequence:

1. `OpenOptions::new().read(true).write(true).open(path)` (`open_file`,
   line 95) - one `openat` syscall.
2. `file.lock_exclusive()` via `fs2::FileExt` - one `flock` syscall
   that may block on a contending peer.
3. `file.seek(SeekFrom::Start(0))` + `read_to_string` - rewinds and
   reads the full file (one `lseek` + one or more `read` syscalls).
4. Parse every `module count` line into a `BTreeMap<String, u32>`
   (`read_counts`, line 131).
5. `file.set_len(0)` + `seek` + `writeln!` per entry +
   `flush` (`write_counts`, line 150) - typically 4-6 syscalls.
6. `drop(file)` releases the `flock` and closes the descriptor.

On warm SSDs with no contention this completes in ~100-300 us. Under
contention from concurrent sessions, the `flock` call serializes all
clients holding handles to the same lock file, so the worst case
balloons to several milliseconds. The same sequence runs again on
session teardown via `ConnectionLockGuard::drop` (line 168).

This work is unconditional: it happens for every module request even
when no `max connections` limit is configured, because the limit is
checked *after* the lock has already been taken
(`increment_count`, line 99). Upstream `clientserver.c:744`
short-circuits via `claim_connection` returning success when no limit
is set.

### 6. Refused options - O(R), in-process, < 1 us

Linear scan of `module.refuse_options` against the client's options
list. Negligible.

### 7. Per-session module clone + variable expansion - O(M), 5-20 us

`module.definition.clone()` (`transfer.rs:235`) and
`expand_module_vars` (`transfer.rs:248`) allocate a fresh
`ModuleDefinition` per request, even when the client sent no
`--dparam` overrides. The clone copies all `String` and `Vec` fields,
including `auth_users`, `hosts_allow`, `hosts_deny`, and the filter
rule lists. For a module with five `auth users` and ten filter rules
this is 15-20 small allocations.

### 8. Authentication - O(disk + crypto), 200 us-5 ms

`perform_module_authentication`
(`crates/daemon/src/daemon/sections/module_access/authentication.rs:33`)
runs the AUTHREQD handshake:

1. `generate_auth_challenge` (line 103) - one MD5 (or MD4) hash over a
   32-byte buffer plus a base64 encode. ~5 us.
2. Write challenge frame + flush - one `sendto`/`send` round trip.
3. `read_trimmed_line` - blocks on the client's response (network RTT
   bound, not daemon CPU).
4. `module.get_auth_user(username)` (`definition.rs:196`) - linear
   scan of `auth_users`. < 1 us.
5. `verify_secret_response` (line 155). This is the heavy step:
   - `check_secrets_file_permissions` (line 200) -> one `stat` + mode
     comparison via the platform helper.
   - `fs::read_to_string(secrets_path)` (line 171) - reads the entire
     file into memory on every authenticated request, even though the
     contents almost never change at runtime.
   - Line-by-line iteration; for each matching user, call
     `verify_daemon_auth_response`
     (`crates/core/src/auth/mod.rs:230`) which computes one MD5/MD4/
     SHA1/SHA512 digest and compares constant-time
     (`mod.rs:270`).

Steady-state cost on a warm cache: 200-400 us. Cold cache or
network-mounted secrets files: 1-5 ms.

### Total budget summary

| Step | Common case | Worst case |
| --- | --- | --- |
| 1. Module lookup | < 1 us | < 1 us |
| 2. Bandwidth apply | < 1 us | < 1 us |
| 3. Reverse DNS | 0 (no hostname rule) | 30 ms (WAN) - 2 s (timeout) |
| 4. Hosts allow/deny | 1-5 us | 50 us (large wildcard set) |
| 5. Lock file | 100-300 us | 5 ms (contention) |
| 6. Refused options | < 1 us | < 1 us |
| 7. Module clone + expand | 5-20 us | 50 us |
| 8. Authentication | 200-400 us | 5 ms (cold secrets) |
| **Per-request total** | **~0.5 ms** | **30 ms-5 s** |

DNS, lock file, and secrets I/O dominate. Everything else is
in-process work measured in microseconds.

## Comparison with upstream `clientserver.c`

The task brief refers to `clientserver.c:start_inbound_session`. The
3.4.1 source actually names the equivalent function `start_daemon`
(`clientserver.c:1275`); it then dispatches to `rsync_module`
(`clientserver.c:692`) once the client picks a module. The combined
flow is the comparison target.

| Stage | Upstream | oc-rsync | Notes |
| --- | --- | --- | --- |
| Reverse DNS at accept | `client_name(addr)` gated by `lp_reverse_lookup(-1)` (`clientserver.c:1342`) | Same gating; resolved lazily inside `module_peer_hostname` only when a hostname pattern exists | oc-rsync is strictly cheaper when only IP rules are present. |
| Per-module reverse DNS retry | `if (host == undetermined_hostname && lp_reverse_lookup(i)) host = client_name(...)` (`clientserver.c:721`) | Per-request `lookup_addr` via the per-call `hostname_cache` | Equivalent semantics. Neither caches across connections. |
| Hosts allow/deny | `allow_access` -> `access_match` `strdup`/`strtok` over allow/deny list strings (`access.c:264`, `:246`) | `permits` over `Vec<HostPattern>` parsed once at config load | oc-rsync wins: upstream re-parses the list strings on every request. |
| Connection slot | `claim_connection` short-circuits on `lp_max_connections(i) <= 0` and only opens/locks the lock file when a limit is configured | `try_acquire_connection` always opens, locks, reads, writes the lock file | oc-rsync regression: lock-file I/O is unconditional. See proposal 2. |
| Authentication digest | MD5 vs MD4 selected by negotiated protocol (`compat.c:858`) | Same selection in `generate_auth_challenge` | Match. |
| Secrets file read | `check_secret` opens, `fstat`s, walks the file (`authenticate.c:100`) | `verify_secret_response` mirrors the upstream flow | Both re-read on every request. See proposal 3. |
| Constant-time compare | `strcmp` (timing-vulnerable) | `constant_time_eq` (`auth/mod.rs:270`) | oc-rsync is strictly safer at no measurable cost. |

Net summary: oc-rsync is faster than upstream on hosts allow/deny
parsing and equivalent on most other steps, but regresses on lock-file
I/O when no `max connections` limit is configured.

## Proposed latency reductions

These five proposals target the dominant costs. None changes the wire
protocol or client-visible semantics.

### Proposal 1: short-circuit reverse DNS at the daemon scope

`module_peer_hostname` already skips `lookup_addr` when no hostname
pattern is present. Extend the same gate to the daemon scope: when
*no* module in the loaded config requires a hostname lookup,
`server_runtime` should never invoke `lookup_addr` and never schedule
a per-connection cache slot. Implementation: add a
`requires_any_hostname_lookup` bool computed once at config-load time
and threaded into the session driver. Saves the full `getnameinfo`
syscall on IP-only deployments.

### Proposal 2: skip the lock file when no limit applies

`try_acquire_connection` should bail out early when
`module.max_connections()` is `None`, returning a no-op
`ConnectionLockGuard`. Mirror upstream's `claim_connection` behavior
(`clientserver.c:744`). Saves 4-8 syscalls (`openat`, `flock`, `lseek`,
`read`, `lseek`, `write`, `fsync` via `flush`, `close`) per
unconstrained module. This is the single largest reduction available
to the steady-state path and is also a parity fix.

### Proposal 3: cache the secrets file with mtime + size invalidation

`verify_secret_response` re-reads the secrets file on every
authenticated request. Replace the `fs::read_to_string` with an
in-memory cache keyed by `(path, st_mtime, st_size, st_ino)`.
Re-read only when stat metadata changes. The stat call is the same
syscall already issued by `check_secrets_file_permissions`
(`authentication.rs:200`), so the steady-state cost drops from
"open + read full file + parse" to a single `stat`. This preserves
upstream semantics: an admin editing the secrets file still takes
effect on the next request.

### Proposal 4: pre-compile wildcard hostname patterns

`HostnamePattern::matches` recursively scans the pattern bytes via
`wildcard_match` on every comparison. For deployments with many
wildcard rules (`*.example.org`, `web-??.dc.example.org`, ...), this
is wasted work that does not depend on the peer.

Pre-compile each `HostnamePatternKind::Wildcard(String)` into a small
NFA (`Vec<MatchOp>` with `Literal(u8)`, `AnyByte`, `Star`) at
config-load time and store it inside `HostnamePattern`. The match step
becomes a tight loop over a pre-built byte program. For the common
case of "`*` followed by a literal suffix", the compiler can collapse
the program into a single `ends_with` check.

### Proposal 5: skip the per-request `ModuleDefinition` clone when no overrides apply

`process_approved_module` clones the entire `ModuleDefinition` to apply
optional `--dparam` overrides and `%MODULE%/%ADDR%` substitution. When
the client sent no overrides *and* no path-style field contains a `%`
placeholder, the clone is pure overhead. Add a fast path:

- If `options.is_empty()` and `module.has_no_path_placeholders()`,
  skip the clone and reuse the shared `&ModuleRuntime` directly.
- Compute `has_no_path_placeholders` once at config-load time by
  scanning `path`, `pre_xfer_exec`, `post_xfer_exec`, `early_exec`,
  `lock_file`, and `log_file` for `%`. Store the bool on the
  runtime struct.

This removes 15-20 small allocations per request on the dominant
configuration shape.

### Combined impact

| Scenario | Today | After 1+2+3+4+5 |
| --- | --- | --- |
| Public IP-only module | 0.3-0.5 ms | 0.05-0.1 ms |
| Auth'd module, warm secrets | 0.5-1 ms | 0.1-0.2 ms |
| Auth'd module, cold secrets | 1-5 ms | 0.2 ms after first request |
| Hostname-rule deployment | 5-30 ms (DNS) | unchanged (DNS bound) |

The DNS path is fundamentally network-bound and is left out of scope -
proposal 1 only avoids the DNS call when no rule needs it.

## Open questions

- Cache eviction policy for the secrets file when many modules share a
  single path: keyed by absolute path, sized to one entry per distinct
  `secrets file` config value. Eviction is naturally bounded by the
  module count.
- Whether to surface the lock-file short-circuit behind a feature flag
  to make the parity fix bisectable. Recommendation: ship as a
  bug-fix-flavored change, since it strictly aligns with upstream.

## References

- oc-rsync source: `crates/daemon/src/daemon/sections/module_access/`,
  `crates/daemon/src/daemon/module_state/`,
  `crates/daemon/src/daemon/sections/config_helpers/host_pattern.rs`,
  `crates/core/src/auth/mod.rs`.
- Upstream rsync 3.4.1: `clientserver.c:start_daemon` (line 1275),
  `clientserver.c:rsync_module` (line 692), `access.c:allow_access`
  (line 264), `authenticate.c:check_secret` (line 100).
