# DIS-4.b: module-select roundtrip latency

Focused audit of the daemon module-select path: from the moment the
greeting loop reads the client's module-request line through the
moment the daemon hands off to the auth / capability negotiation
phase that DIS-4.c owns. The greeting itself (write `@RSYNCD:` + read
the client's version reply) is audited by DIS-4.a in
`docs/audits/dis-4a-rsyncd-greeting-overhead.md`; auth and capability
negotiation are audited by DIS-4.c. Both are out of scope here.

This audit narrows the DIS-3 phase-table rows **5, 6, 7, 8, 11, 12,
13** to file-and-line evidence, counts syscalls and allocations
versus upstream rsync 3.4.1, and ranks fixes for DIS-6 to schedule.

Sources cited (all paths relative to worktree root):

- `crates/daemon/src/daemon/sections/session_runtime.rs`
- `crates/daemon/src/daemon/sections/greeting.rs`
  (`read_trimmed_line`)
- `crates/daemon/src/daemon/sections/module_access/request.rs`
- `crates/daemon/src/daemon/sections/module_access/client_args.rs`
- `crates/daemon/src/daemon/sections/module_access/transfer.rs`
- `crates/daemon/src/daemon/sections/module_access/helpers.rs`
- `crates/daemon/src/daemon/sections/module_parsing.rs`
  (`refused_option`, `apply_module_timeout`,
  `apply_daemon_param_overrides`)
- `crates/daemon/src/daemon/sections/privilege.rs`
  (`apply_module_privilege_restrictions`)
- `crates/daemon/src/daemon/module_state/runtime.rs`
  (`try_acquire_connection`)
- `crates/daemon/src/daemon/module_state/connection_limiter.rs`
  (file-locked count update)
- `crates/daemon/src/daemon/module_state/hostname.rs`
  (`module_peer_hostname`, cache reuse)
- `crates/daemon/src/daemon/module_state/definition.rs`
  (`ModuleDefinition::permits`)
- `crates/transfer/src/config/mod.rs`
  (`ServerConfig::from_flag_string_and_args`)
- `crates/transfer/src/flags.rs` (`ParsedServerFlags::parse`)
- upstream:
  `target/interop/upstream-src/rsync-3.4.1/clientserver.c::rsync_module`
  (lines 692-1110), `connection.c::claim_connection`,
  `io.c::read_args`, `access.c::allow_access`.

## 1. Module-select-phase inventory (per accepted connection)

Numbered in protocol order, starting from the point where DIS-4.a
finished. Each row cites file:line for the operation and tags
syscalls (`S`), heap allocations (`A`), and lock or atomic
acquisitions (`L`).

The phase begins immediately after the client's `@RSYNCD: <ver>`
reply has been parsed by the greeting loop in
`session_runtime.rs:233-272`. It ends right before
`handle_authentication` runs (auth is DIS-4.c).

| # | Operation | Where | Cost tag | DIS-3 row |
|---|-----------|-------|----------|-----------|
| 1 | Second `read_trimmed_line` for the module-request line (allocates a fresh `String`, calls `BufReader::read_line` which issues at most one `read(2)`) | `session_runtime.rs:233`, `greeting.rs:49-62` | 1 A, 1 S | 5 |
| 2 | `parse_legacy_daemon_message(&line)` - zero-alloc parse on the borrowed line | `session_runtime.rs:234` | 0 | 5 |
| 3 | Optional `read_early_input` check (`#early_input=` prefix). Default config: zero-cost prefix test, no I/O | `session_runtime.rs:265-268` | 0 on default | 5 |
| 4 | Dispatch into `respond_with_module_request` with `&request`, peer, log, messages, protocol, early-input | `session_runtime.rs:300-313` | 0 | 5 |
| 5 | Linear scan over `modules: &[ModuleRuntime]` looking for `module.name == request` | `request.rs:257` | 0 S; N comparisons (N = number of modules, typically 1-10) | 6 |
| 6 | `apply_module_bandwidth_limit` - applies module `bwlimit` overrides on top of the daemon-wide limiter. Default config: no module `bwlimit`, returns `Unchanged`. | `request.rs:269-276`, `helpers.rs:19-51` | 0 on default; possibly 1 A on rebuild when limit changes | 7 |
| 7 | `module_peer_hostname`: returns `None` immediately unless `requires_hostname_lookup()` AND `reverse_lookup`. When triggered, calls `lookup_addr` -> `getnameinfo(2)`, caches the result in a local `Option<Option<String>>` | `request.rs:278-280`, `hostname.rs:21-49` | 0 S on default (no host patterns); 1 S + 1+ A when `hosts allow`/`hosts deny` contains hostnames | 7 |
| 8 | `ModuleDefinition::permits(peer_ip, hostname)` - linear scan over `hosts_allow` then `hosts_deny`; each `HostPattern::matches` is a CIDR / hostname compare | `request.rs:307`, `definition.rs:161-180` | 0 S, 0 A | 7 |
| 9 | `try_acquire_connection`. Two paths: (a) no `max connections` -> returns an empty guard, 0 cost; (b) `max connections` configured -> see rows 9a-9d | `transfer.rs:388`, `runtime.rs:55-74` | branch | 8 |
| 9a | When limited: `ConnectionLimiter::acquire` opens the lock file (`OpenOptions::open` on the configured `lock file` path) | `connection_limiter.rs:67-72`, `runtime.rs:60` | 1 S (`open`) | 8 |
| 9b | `file.lock_exclusive()` (`fs2::FileExt`) - issues `flock(LOCK_EX)` on Unix, `LockFileEx` on Windows | `connection_limiter.rs:73` | 1 S + 1 L | 8 |
| 9c | `read_counts(file)`: `seek(0)` + `read_to_string` (full file slurp) then `for line in contents.lines()` building a fresh `BTreeMap<String,u32>` | `connection_limiter.rs:131-147` | 2 S (`lseek`, `read`), 1-N A (one per module entry) | 8 |
| 9d | `write_counts(file, &counts)`: `set_len(0)` + `seek(0)` + `writeln!` per entry + `flush`. Drops the file (releases flock + closes fd via `close(2)`) | `connection_limiter.rs:150-158`, `runtime.rs:76` | 4-5 S (`ftruncate`, `lseek`, `write`, `fsync`-ish via flush, `close`) | 8 |
| 9e | `acquire_local_slot` - in-process `AtomicU32` CAS loop in `ModuleRuntime::active_connections` | `runtime.rs:77-95` | 1+ L (atomic RMW) | 8 |
| 10 | `log_module_request` (writes "rsync allowed access on module ..." to the SharedLogSink). Default config: no log file -> 0; with log -> 1 A (`format!`) + 1 S (`write` + flush under `Mutex`) | `transfer.rs:398-400` | 0-1 S, 0-1 A, 0-1 L | 7 (logged but on critical path) |
| 11 | `refused_option(module, options)` - early-exit on empty refuse list; otherwise iterates client-sent daemon options and globs. Default config: empty `refuse options`, returns `None` immediately | `transfer.rs:402-404`, `module_parsing.rs:51-64` | 0 on default | 12 (refuse evaluation rides on phase 12) |
| 12 | `module.definition.clone()` to build a session-local `ModuleDefinition` (so per-connection `--dparam` overrides do not mutate shared state) | `transfer.rs:412-413` | 1+ A (clones all `String`/`Vec`/`PathBuf` fields of the definition - tens of small `Box<str>`/`Box<[u8]>` for filters, includes, excludes, etc.) | 12 |
| 13 | `apply_daemon_param_overrides(options, &mut definition)` (no-op if no `--dparam` flags) | `transfer.rs:415-422`, `module_parsing.rs:544` | 0 on default | 12 |
| 14 | `expand_module_vars(&mut definition, &client_addr, client_host)` - rewrites `%MODULE%`, `%ADDR%`, `%HOST%`, `%RSYNC_USER%` in the cloned definition's path-type fields | `transfer.rs:426`, `variable_expansion.rs:97` | 1+ A per template match; 0 on default | 12 |
| 15 | `ModuleRuntime::from(definition)` - rewraps in a runtime with a fresh `AtomicU32` and no limiter (session-local copy, never observed by the cross-process limiter) | `transfer.rs:427-428` | 1 A | 12 |
| 16 | `apply_module_timeout(stream, module)` - if `timeout = N` set, two `setsockopt` calls (`SO_RCVTIMEO`, `SO_SNDTIMEO`). Default: 0 S | `transfer.rs:431`, `module_parsing.rs:125-133` | 0-2 S | 13 |

Phase 12 (`ServerConfig` build) and phase 13 (privilege / chroot /
Landlock setup) sit physically *after* authentication in the current
code (`process_approved_module` order: auth, then `build_server_config`,
then `apply_module_privilege_restrictions`, then
`engage_landlock_sandbox`). They are still owned by DIS-4.b in the
DIS-3 mapping because they consume the result of module-select and
do not themselves drive any roundtrip back to the client. For
inventory completeness:

| # | Operation | Where | Cost tag | DIS-3 row |
|---|-----------|-------|----------|-----------|
| 17 | `read_and_log_client_args` - reads null/newline-terminated args (`-s` detection then optional secluded-args phase 2). Each `read_until(b'\0')` allocates a fresh `Vec<u8>` per arg, then `String::from_utf8_lossy(...).into_owned()` allocates a `String` (~15-20 args on the small-files corpus) | `transfer.rs:498`, `client_args.rs:21-62`, `client_args.rs:116-172` | ~15-20 S (one `read` per arg, kernel side) + ~30-40 A (one `Vec` + one `String` per arg) | 11 |
| 18 | `determine_server_role(&client_args)` - linear `iter().any(|a| a == "--sender")`. Zero-cost beyond the scan | `transfer.rs:508`, `client_args.rs:178-184` | 0 | 11 |
| 19 | `read_only` / `write_only` check (returns `@ERROR` and exits the path before the heavy build when refused) | `transfer.rs:509-518` | 0 on default | 11 |
| 20 | `validate_module_path(ctx, module)` - `Path::new(&module.path).exists()` runs one `stat(2)` | `transfer.rs:520`, `transfer.rs:14-42` | 1 S | 13 |
| 21 | `validate_client_paths_in_module(ctx, module, &client_args)` - one `module.path.canonicalize()` (issues `lstat` chain to resolve symlinks - typically 1-5 syscalls on a short module path), then for each `--temp-dir` / `--partial-dir` / `--backup-dir` argument, another `canonicalize` (default config: none of these are set, so just the up-front module canonicalize) | `transfer.rs:529`, `transfer.rs:53-116` | 1-5 S (canonicalize root) + 0 on default for client args | 13 |
| 22 | `apply_module_privilege_restrictions` - default config (no `use chroot`, no `uid`, no `gid`): zero-cost no-op. When chroot + setuid configured: 1 `chroot(2)` + 1 `chdir(2)` + 1 `setgid(2)` + 1 `setuid(2)` | `transfer.rs:539,546`, `privilege.rs:110-123` | 0 on default; 3-4 S when configured | 13 |
| 23 | `build_server_config` - `ParsedServerFlags::parse` (byte-loop over the compact `-logDtpre.iLsfxC` flag string), then `apply_long_form_args` (linear scan over `client_args` matching `--delete`, `--inplace`, etc.). Allocates one `ServerConfig` with several `Vec`/`PathBuf` fields | `transfer.rs:596`, `client_args.rs:216-300`, `flags.rs:176-210` | 0 S; ~5-10 A | 12 |
| 24 | `build_daemon_filter_rules` - rebuilds `daemon_filter_rules` from `module.filter`, `module.include`, `module.exclude`, `module.include_from`, `module.exclude_from`. Default config: all five fields empty, returns an empty `Vec` | `transfer.rs:604`, `helpers.rs:226-283` | 0 S on default; 1 S per `include_from`/`exclude_from` file otherwise | 12 |
| 25 | `engage_landlock_sandbox` - calls `fast_io::landlock::is_supported()` and (on Linux 5.13+) `restrict_to_module_paths(&[module.path])` which executes a `landlock_create_ruleset` + per-root `landlock_add_rule` + `landlock_restrict_self` sequence. Stub no-op on non-Linux | `transfer.rs:621`, `transfer.rs:140-219` | 0 S on non-Linux; 3-5 S on Linux supporting kernels | 13 |
| 26 | `setup_transfer_streams` - `set_nodelay(true)` (1 `setsockopt`) + two `TcpStream::try_clone()` (two `dup(2)` syscalls) | `transfer.rs:625`, `transfer.rs:226-251` | 3 S | (end of module-select phase) |

Phase 26 is the handoff point: from here the connection enters the
auth pipeline (DIS-4.c) when auth is configured, or proceeds
directly into compat exchange / capability advertisement.

### Allocation tally for module-select only (default config, no auth, no chroot, no host patterns, no refuse list, no log file)

Counting the rows above:

- Row 1 (module-request `read_line`): 1 alloc, 1 syscall.
- Rows 2-4: zero-cost.
- Rows 5-8 (lookup, permit check): zero alloc and zero syscalls
  (host patterns absent).
- Rows 9-9e (unlimited connection guard path): zero cost.
- Row 10 (log line): zero on the no-log default.
- Row 11 (refuse list empty): zero.
- Row 12 (`module.definition.clone()`): ~10-30 small allocs (one
  per non-empty `String`/`Vec`/`PathBuf` in the definition;
  varies with how many module directives are set).
- Rows 13-15 (dparam, expand, rewrap): 1 alloc (the `ModuleRuntime`
  wrapper).
- Row 16 (timeout): 0 on default.
- Row 17 (client args): ~30-40 small allocs and ~15-20 read
  syscalls.
- Rows 18-19: zero.
- Row 20: 1 syscall (`stat`).
- Row 21: 1-5 syscalls (canonicalize the module root).
- Row 22 (privilege): 0 on default.
- Row 23 (`build_server_config`): ~5-10 allocs.
- Row 24 (filter rules): 1 alloc (empty `Vec`).
- Row 25 (landlock): 0 on non-Linux; 3-5 syscalls on supporting
  kernels (no allocations on the hot path because the helper takes
  a borrowed `&[&Path]`).
- Row 26 (stream prep): 3 syscalls (1 `setsockopt`, 2 `dup`).

**Per module-select on default Linux config: ~50-90 heap touches
and ~20-30 syscalls**, dominated by the per-argument allocations in
row 17 and the deep clone in row 12.

When `max connections` is set, add the lock-file roundtrip: rows
9a-9d cost ~7-8 syscalls (`open`, `flock`, `lseek`, `read`,
`ftruncate`, `lseek`, `write`, `close`) and 1-N allocations for the
`BTreeMap` rebuild.

## 2. Upstream comparison (rsync 3.4.1, default config)

Per module-select on the same configuration, upstream walks the
same logical phases but in C with a fixed `line[BIGPATHBUFLEN]`
stack buffer and pre-tokenised argument storage. The relevant
upstream functions are `rsync_module()` (`clientserver.c:692-1110`),
`claim_connection()` (`connection.c:26-47`), `read_args()`
(`io.c:1292-1346`), and `allow_access()` (`access.c:264-292`).

| Category | oc-rsync | upstream 3.4.1 | Delta |
|----------|----------|----------------|-------|
| Module-request read | `BufReader::read_line` into a fresh `String` | `read_line_old(f_in, line, sizeof line, 0)` into the same stack `line[]` that buffered the version reply | +1 alloc on oc-rsync |
| Module lookup | Linear scan over `&[ModuleRuntime]` doing `module.name == request` | `lp_number(line)` - linear scan over numbered modules in the loadparm table | Symmetric; same complexity |
| Host allow/deny | `module.permits(addr, hostname)` - iterates `Vec<HostPattern>` once | `allow_access(addr, &host, i)` - iterates allow/deny lists once | Symmetric |
| Reverse DNS (module path) | `module_peer_hostname` caches on first call; later module-config hostname lookups reuse the cache via the local `Option<Option<String>>` | `if (host == undetermined_hostname && lp_reverse_lookup(i)) host = client_name(client_addr(f_in));` - skips the lookup if the session-level lookup already ran | Symmetric in both: a second `getnameinfo` only fires when the session was undetermined |
| `claim_connection` (when `max connections` is set) | Open lock file -> `flock(LOCK_EX)` -> `seek(0)` -> `read_to_string` -> parse into `BTreeMap` -> `set_len(0)` -> rewrite all entries -> `flush` -> drop (close + unlock). One `BTreeMap<String,u32>` allocated per call | `open(O_RDWR\|O_CREAT)` -> for `i` in `0..max_connections` try `lock_range(fd, i*4, 4)` (`fcntl(F_SETLK)` on a 4-byte region) - on first free slot, returns leaving the lock held to the worker fd. No reads, no writes, no allocations | **+7-8 syscalls and +1-N allocations** on oc-rsync per request when `max connections` is configured |
| `claim_connection` (default, `max connections = 0`) | `try_acquire_connection` short-circuits to an empty guard | `if (max_connections == 0) return 1;` | Symmetric (zero-cost) |
| Per-connection `ModuleDefinition` clone | `module.definition.clone()` deep-copies every config field per request (preparation for `--dparam` overrides) | Upstream mutates global loadparm state per request (`module_id = i`) and reads through `lp_*(i)` accessors. No clone | **+10-30 small allocations** on oc-rsync per request |
| `read_args` | `read_client_arguments` issues one `read_until(b'\0')` per arg, each into a freshly-allocated `Vec<u8>`, then `String::from_utf8_lossy(...).into_owned()` | `read_args` reads each arg into the same stack `line[]` (re-used), then `strdup(buf)` into an `argv` array; argv is `new_array(char *, MAX_ARGS)` (one allocation), strings are individually `strdup`-allocated | Upstream still allocates per-arg strings, but uses a single bulk-realloc'd `argv`, no per-arg `Vec` for the read buffer, and no UTF-8 lossy conversion. **+15-20 small allocations** on oc-rsync per request (the read-buffer `Vec`s and the `String` wrappers) |
| Compact flag parsing | `ParsedServerFlags::parse` - byte-loop over the flag string updating bool fields | `parse_arguments(argc, argv)` runs popt over the full long-form argv (covers both compact and long-form) | Symmetric in cost; oc-rsync's parse is *cheaper* (no popt machinery), but the long-form pass `apply_long_form_args` is a separate linear scan that upstream folds into the same popt call |
| `apply_long_form_args` | Linear scan over `client_args` doing `match arg.as_str()` and `strip_prefix` per known long option | popt builds an option table and matches each arg once | Roughly symmetric; both are O(args x options); the popt table is precomputed |
| Module path validation | `Path::new(&module.path).exists()` (1 `stat`) + `module.path.canonicalize()` (1-5 syscalls in `validate_client_paths_in_module`) | `if (*module_dir == '\0')` text check; `normalize_path()` for chroot path string-manip only (no filesystem call) | **+2-6 syscalls** on oc-rsync per request |
| Privilege / chroot (default no-chroot, no uid/gid) | `apply_module_privilege_restrictions` short-circuits | Same: no-op when nothing configured | Symmetric |
| Landlock engagement | `is_supported()` probe (cached after first call via `OnceLock`) + `restrict_to_module_paths` (3-5 syscalls on supporting kernels, no-op stub elsewhere) | No equivalent feature | **+0-5 syscalls** on oc-rsync per request on Linux; oc-rsync wins on safety, pays on latency |
| Stream prep | `set_nodelay(true)` + 2x `try_clone` | Upstream uses `f_in` / `f_out` int fds directly; no `dup` | **+3 syscalls** on oc-rsync per request |

### Aggregate counts (default-config small-files cold-start)

| Metric | oc-rsync | upstream 3.4.1 | Delta |
|--------|----------|----------------|-------|
| Heap allocations during module-select | ~50-90 | ~15-25 (mostly `strdup` per arg) | **+30-70** |
| Syscalls during module-select (excluding the second `read_line` itself, counted in DIS-4.a's tally row 20) | ~20-30 | ~15-20 | **+5-10** |
| Lock acquisitions | 0-1 (atomic CAS only) | 0 (popt + globals are single-threaded per worker) | **+1** at most |
| Lock-file roundtrip when `max connections` is set | 7-8 syscalls + 1 file open + 1 flock | 1 syscall (`open`) + 1 `fcntl(F_SETLK)` per slot tried (typically 1-2) | **+5-6 syscalls** on oc-rsync |

These deltas are wire-compatible: no roundtrip changes, no protocol
divergence. The cost is entirely on the daemon side.

## 3. Top contributors (ranked by estimated wall-clock cost)

Cost estimates assume the DIS-1 small-files cold-start scenario
(500 files, 1 KiB each, loopback, Debian-glibc allocator) on a
default daemon config: no `max connections`, no `hosts allow` /
`hosts deny`, no `auth users`, no log file, no chroot, no refuse
list, no `--dparam`. Upstream is built from
`target/interop/upstream-src/rsync-3.4.1` with the same config.

### 1. Per-arg `Vec` + `String` allocation in `read_client_arguments` (DIS-3 row 11)

`client_args.rs:21-62`. The protocol >= 30 path issues one
`reader.read_until(b'\0', &mut buf)?` per argument where `buf` is a
fresh `Vec<u8>` allocated inside the loop iteration. Each completed
arg then goes through `String::from_utf8_lossy(&buf).into_owned()`,
which produces a second allocation per arg even when the bytes are
clean ASCII (the `Cow::Borrowed` -> `into_owned` path copies).

- Expected cost on the 15-20 args sent by a typical client:
  **~30-40 small heap touches and ~15-20 read syscalls**, against
  upstream's ~15-20 `strdup`s into a single `argv` array and one
  shared `line[]` stack buffer.
- Largest single contributor to module-select wall-clock on the
  default path.
- Fix sketch: thread a reusable `Vec<u8>` (cleared between args)
  through the loop, and skip `from_utf8_lossy` entirely when the
  bytes are valid UTF-8 (use `String::from_utf8(buf.clone())` plus a
  fast pre-check; for daemon protocol the args are always ASCII
  flags and paths so the lossy branch is dead code on the cold-start
  scenario).

### 2. `module.definition.clone()` per request (DIS-3 row 12)

`request.rs:412-413`. The clone exists so that `--dparam` overrides
do not mutate shared module state. On the default path no `--dparam`
arrives, but the clone fires unconditionally. A `ModuleDefinition`
contains many `String` / `Vec<String>` / `Option<PathBuf>` fields
(`name`, `path`, `comment`, `secrets_file`, `hosts_allow`,
`hosts_deny`, `auth_users`, `filter`, `include`, `exclude`, etc.),
each of which deep-clones into its own heap block.

- Expected cost: **~10-30 small allocations per request**,
  ~5-15 us on glibc when uncontended.
- Fix sketch: gate the clone on `!options.is_empty()`. The default
  request has no daemon-options at all, so the clone-on-write here
  is pure waste. When `--dparam` *is* present, keep the current
  behaviour.

### 3. `connection_limiter.rs::acquire` rewrites the whole lock file (DIS-3 row 8, only when `max connections` is set)

`connection_limiter.rs:67-114`. Each acquire:

1. Opens the lock file (`open(2)`).
2. Acquires `flock(LOCK_EX)` for the whole file.
3. `lseek(0)` + `read_to_string` of the full file content.
4. Parses every line into a `BTreeMap<String,u32>` (one alloc per
   module entry).
5. Increments the requested module's slot.
6. `set_len(0)` + `lseek(0)` + rewrites *every* entry.
7. Flushes + drops (closes + releases the flock).

Upstream's `claim_connection` (`connection.c:26-47`) opens the file
once and uses byte-range locks (`fcntl(F_SETLK)` on 4-byte regions)
per slot: each slot tried is one syscall, success returns leaving
the lock held to the worker fd. **No reads. No writes. No
allocations.**

- Expected cost: **+5-6 syscalls and +1 contended global flock** on
  oc-rsync per request when `max connections` is configured.
- Latent hazard at scale: under contention, the `LOCK_EX` on the
  whole file serializes every accept across daemon workers; upstream's
  per-slot locks let workers find their own slot in parallel.
- Default DIS-1 config has no `max connections`, so this row does
  not contribute to the 1.35 s baseline. It is on this list because
  it is the largest design divergence in the phase and the most
  visible contention point in production deployments.
- Fix sketch: switch to the per-slot `fcntl(F_SETLK)` model used by
  upstream. `fs2::FileExt::try_lock_exclusive_range` plus a fallback
  to `fs4` covers Unix and Windows; the lock file format becomes a
  fixed-size array of zero bytes (one 4-byte slot per allowed
  connection) instead of a parsed `module count` text file.

### 4. `validate_client_paths_in_module` and `validate_module_path` filesystem stat (DIS-3 row 13)

`transfer.rs:14-42`, `transfer.rs:53-116`. Even on the default path,
oc-rsync issues one `Path::exists()` (a `stat`) and one
`Path::canonicalize()` (a `lstat` chain - 1-5 syscalls depending on
symlink depth) per request, before any auth or transfer begins.
Upstream performs no syscall here: it relies on `normalize_path()`
(string manipulation) and only fails later when `opendir(module_dir)`
fails inside the transfer engine.

- Expected cost: **2-6 syscalls per request** on the default path.
- Each syscall is sub-microsecond on a warm-cache module root, but
  the canonicalize chain shows up under `strace` and is the only
  non-allocation divergence on the no-`max-connections` path.
- Fix sketch: drop `validate_module_path` and rely on the
  `OpenOptions` failure inside the transfer engine to surface a
  missing module path (upstream's behaviour). Keep
  `validate_client_paths_in_module` but defer its `canonicalize` to
  the point where a `--temp-dir` / `--partial-dir` / `--backup-dir`
  argument has actually been parsed; on the default path that branch
  is never taken so the canonicalize is dead work.

### 5. `setup_transfer_streams` `set_nodelay` + double `try_clone` (DIS-3 row 13)

`transfer.rs:226-251`. Issues `set_nodelay(true)` (1 `setsockopt`)
followed by `stream.try_clone()` twice (two `dup(2)` syscalls) so
the read and write halves can be handed to separate threads.
Upstream uses raw `int` file descriptors (`f_in`, `f_out`) and does
not `dup`.

- Expected cost: **3 syscalls per request**, all on the critical
  path between auth and the first compat byte.
- The `set_nodelay` call is defensible: the listener-level
  configuration may not have applied (e.g. for inherited fds).
  The double `try_clone` is structural - the `run_server_with_handshake`
  contract takes separate read and write streams.
- Fix sketch: low-impact. If profiling confirms these three syscalls
  matter, consider stashing a `set_nodelay = true` invariant on the
  listener and using `&TcpStream` borrowing instead of `try_clone`
  (would require refactoring `run_server_with_handshake` to take an
  `Arc<TcpStream>` plus `Read`/`Write` halves).

## 4. Recommendations (ranked, each one-paragraph; DIS-6 implements)

### R1. Stop deep-cloning `ModuleDefinition` on the default path

`request.rs:412-413`. Wrap the clone behind `!options.is_empty()`.
When `--dparam` is absent (the overwhelming default) the request
should run against the shared `&ModuleRuntime` borrowed from the
`modules: &[ModuleRuntime]` slice. The chroot-path rewrite at
`transfer.rs:586-594` already takes care of the
"path becomes '/' under chroot" case via a *second* clone; once R1
lands, that clone is the only one on the path and runs only when
chroot is configured. Wire-compatibility: unaffected. Expected
single-PR win: ~10-30 allocations off median per request.

### R2. Reuse the per-arg read buffer in `read_client_arguments`

`client_args.rs:21-62`. Replace the per-iteration `let mut buf =
Vec::new();` with a buffer hoisted before the `loop {` block and
cleared (`buf.clear()`) at the top of each iteration. Replace
`String::from_utf8_lossy(&buf).into_owned()` with an explicit
`std::str::from_utf8(&buf).map(str::to_owned)` fast path that falls
back to lossy only on invalid UTF-8. Upstream args are always ASCII
on the default cold-start path so the lossy branch is dead code.
Wire-compatibility: unaffected (the parser sees identical bytes).
Expected single-PR win: ~30-40 allocations off median per request,
plus measurable reduction in allocator jitter.

### R3. Drop the eager `validate_module_path` stat and defer
`canonicalize` in `validate_client_paths_in_module`

`transfer.rs:14-42`, `transfer.rs:53-116`. Upstream relies on the
transfer engine to surface a missing module path via the
`opendir`/`open` error; matching that behaviour saves one `stat` per
request. For `validate_client_paths_in_module`, gate the
`module.path.canonicalize()` behind "did the client actually send a
`--temp-dir` / `--partial-dir` / `--backup-dir`?" - the canonicalize
is only meaningful when there is a candidate path to compare
against. Wire-compatibility: unaffected (the error reporting changes
from `@ERROR: module 'X' path does not exist` at request time to a
later transfer-time error; upstream emits the latter). Expected
single-PR win: 2-6 syscalls off median per request on the default
path.

### R4. Replace the rewrite-whole-file lock with per-slot `fcntl(F_SETLK)` ranges

`connection_limiter.rs:67-114`. Switch to upstream's design: a fixed
file of `4 * max_connections` zero bytes per module, each accept
tries `try_lock_exclusive_range(slot * 4, 4)` until it finds a free
slot or exhausts the range. Use the `fs4` crate (the maintained
successor to `fs2`) which exposes the range API on Linux, macOS,
and Windows. Removes the read/write/parse/serialize cycle and the
whole-file flock contention. Wire-compatibility: unaffected (the
lock-file format is daemon-internal). Expected single-PR win: 5-6
syscalls off median per request when `max connections` is
configured; large p99 win under contention.

### R5. Cache the daemon-side `is_supported()` landlock probe globally (already done) and skip the engagement no-op when there are no roots

`transfer.rs:140-219`. The `is_supported()` probe is already cached
via `OnceLock` inside `fast_io::landlock`; verify under DIS-2 that
this is the case on every kernel build. The remaining engagement
cost (3-5 syscalls per accept on supporting kernels) is by design:
without engagement, SEC-1.p's defense degrades. Listed for
completeness; not a recommended optimization. Cross-link to
`docs/audits/sec-1p-landlock-coverage.md` if/when it exists.

### R6. `set_nodelay` on the listener, not per-accept

`transfer.rs:230`. Set `TCP_NODELAY` once on the listening socket
(inherited by accepted children on Linux/BSD) and drop the
per-accept `setsockopt`. Removes 1 syscall per request.
Wire-compatibility: unaffected. Expected single-PR win: ~1 us per
request.

## 5. Cross-reference: DIS-3 phases covered

This audit covers DIS-3's phase rows:

- **Row 5** - Read client version line + module name. The version
  read itself was audited by DIS-4.a; the module-request read is
  inventory row 1 here.
- **Row 6** - Module lookup (linear scan over `Vec<ModuleRuntime>`).
  Inventory row 5.
- **Row 7** - Host allow/deny + reverse-lookup. Inventory rows 7-8;
  bandwidth-limit application also rides here (row 6).
- **Row 8** - Connection lock + `try_acquire_connection`. Inventory
  rows 9-9e. Dominant divergence (R4).
- **Row 11** - Read client args (null-terminated argv). Inventory
  row 17. Largest contributor on default path (R2).
- **Row 12** - `ServerConfig` build from flag string + long opts.
  Inventory rows 11-13, 23-24. Note: DIS-3 row 12 described the
  parser as "full Clap-style re-parse"; that was incorrect.
  `ParsedServerFlags::parse` is a 30-line byte loop
  (`flags.rs:176-210`); the actual cost on this row comes from the
  `module.definition.clone()` at request time (R1) and the
  `apply_long_form_args` linear scan, not the flag parser itself.
- **Row 13** - Privilege / chroot / Landlock setup. Inventory rows
  16, 22, 25. Landlock engagement is the largest sub-cost on Linux
  (3-5 syscalls); R5 documents why it stays.

### Boundary handoffs

- **Where DIS-4.a leaves off**: DIS-4.a's inventory ends at its
  row 20 (the module-request `read_trimmed_line`). That same line
  is row 1 of this audit. To avoid double-counting, the allocation
  and syscall tallies in section 1 here *do not* include row 1's
  one alloc + one syscall (DIS-4.a already counted them under its
  "Module-request `read_line`" line).
- **Where DIS-4.c picks up**: DIS-4.c starts at `handle_authentication`
  (`transfer.rs:433`, `request.rs:151-183`). When `auth users` is
  configured, this is the next roundtrip (auth challenge + reply).
  When auth is disabled (the DIS-1 cold-start scenario), the only
  cost charged to auth is the `send_daemon_ok` call inside
  `handle_authentication` (`request.rs:157`) which writes the
  cached `@RSYNCD: OK\n` from `LegacyMessageCache`. That cache hoist
  is a DIS-4.a recommendation (R3 in that audit); the OK byte
  itself is on the wire under DIS-4.c's scope.

DIS-3 rows 2, 3, 4, 10 belong to **DIS-4.a** (greeting overhead).
Rows 9, 14, 15, 16 belong to **DIS-4.c** (auth + capability +
checksum seed + filter list). Rows 17-22 belong to **DIS-4.d**
(flist build) and **DIS-4.e** (first-block send).

## 6. Confidence and what DIS-2 should confirm

- **High confidence:** R1 (`ModuleDefinition::clone`), R2 (per-arg
  buffer reuse), R4 (per-slot lock ranges). All three follow
  directly from code reading and from comparing the syscall and
  allocation counts to upstream. Each fix is bounded and
  wire-neutral.
- **Medium confidence:** R3 (filesystem stat removal). The behavior
  change (defer "module path does not exist" error from request
  time to transfer time) matches upstream but is a user-visible
  reporting shift; verify DIS-2 sees the canonicalize syscall
  cluster on `strace -c` before scheduling the fix. R6 (`set_nodelay`
  on listener) is a 1-us micro-fix; verify under `perf stat -e
  syscalls:sys_enter_setsockopt` that the call actually appears per
  accept on the measured kernel.
- **Low confidence:** Landlock engagement cost (R5). The 3-5
  syscalls per accept on supporting kernels is from the `fast_io`
  crate's documented behavior; the actual cost on the DIS-1 harness
  needs `perf trace` to confirm before any "is this our bottleneck?"
  conversation. Probably not, but worth nailing down.

DIS-2 should re-run the harness with `perf record -F 999 -g
--call-graph fp` against `respond_with_module_request` and look for:

- Whether `read_client_arguments` (row 17) accounts for the
  per-accept allocation pattern in the flame graph. If yes, R2 is
  the median-cost dominant fix on the default config.
- Whether `module.definition.clone()` (row 12) shows up as a
  visible `clone3` / `__memcpy_avx_unaligned` cluster. If yes, R1
  is on the critical path; if no, defer.
- Whether the lock-file roundtrip (rows 9a-9d) appears on the
  no-`max-connections` default. It should not; if it does, the
  default has somehow grown a lock-file path and needs reproducer.

## 7. Related audits

- `docs/audits/dis-3-cold-start-phase-decomposition.md` - parent task
  this audit feeds.
- `docs/audits/dis-4a-rsyncd-greeting-overhead.md` - sibling audit;
  shares the boundary at row 1 (the module-request `read_line`).
- `docs/audits/daemon-handshake-overhead.md` - prior inventory and
  mitigation list across the greeting + module-select boundary.
- `docs/audits/binary-startup-overhead.md` - DIS-3 row 1 (out of
  DIS-4 scope).
