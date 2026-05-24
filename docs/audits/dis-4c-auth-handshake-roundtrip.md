# DIS-4.c: auth handshake roundtrip count

Focused audit of the daemon authentication path: from the end of the
module-select handoff that DIS-4.b owns through the compat exchange,
capability negotiation, multiplex-output activation, and filter-list
receive that close out the handshake before the sender begins building
the file list. The greeting (DIS-4.a), module lookup and arg parsing
(DIS-4.b), file-list build (DIS-4.d) and first-block send (DIS-4.e) are
sibling audits and out of scope here.

This audit narrows the DIS-3 phase-table rows **9, 14, 15, 16** to
file-and-line evidence, counts syscalls, allocations and crypto
operations versus upstream rsync 3.4.1, and ranks fixes for DIS-6 to
schedule. It also quantifies the anonymous-module bypass case where
`auth users` is empty and the entire roundtrip on rows 9 collapses to
a one-line `@RSYNCD: OK\n` write.

Sources cited (all paths relative to worktree root):

- `crates/daemon/src/daemon/sections/module_access/authentication.rs`
  (`perform_module_authentication`, `generate_auth_challenge`,
  `verify_secret_response`, `send_auth_failed`,
  `check_secrets_file_permissions`)
- `crates/daemon/src/daemon/sections/module_access/request.rs`
  (`handle_authentication`, `send_daemon_ok`)
- `crates/daemon/src/daemon/sections/module_access/transfer.rs`
  (`process_approved_module` lines 433-436 hand off into auth)
- `crates/daemon/src/daemon/sections/legacy_messages.rs`
  (`LegacyMessageCache::write`, `write_ok`, `write_exit`)
- `crates/daemon/src/daemon/sections/greeting.rs`
  (`read_trimmed_line`)
- `crates/daemon/src/auth.rs` (public surface re-export)
- `crates/daemon/src/daemon/module_state/auth.rs` (`AuthUser`,
  `UserAccessLevel`)
- `crates/daemon/src/daemon/module_state/definition.rs`
  (`requires_authentication`, `get_auth_user`)
- `crates/platform/src/secrets.rs`
  (`check_secrets_file_permissions`)
- `crates/core/src/auth/mod.rs`
  (`verify_daemon_auth_response`, `compute_daemon_auth_response`,
  `digests_for_response`, `constant_time_eq`)
- `crates/protocol/src/legacy/lines.rs::format_legacy_daemon_message`
  (`AUTHREQD` / `OK` / `EXIT` wire framing)
- `crates/transfer/src/lib.rs::run_server_with_handshake`
  (compat exchange, multiplex activation, filter list receive)
- `crates/transfer/src/setup/mod.rs::setup_protocol`,
  `crates/transfer/src/setup/negotiator.rs`
- upstream: `target/interop/upstream-src/rsync-3.4.1/authenticate.c`
  (`auth_server`, `gen_challenge`, `generate_hash`, `check_secret`)
- upstream: `target/interop/upstream-src/rsync-3.4.1/compat.c:847`
  (`negotiate_daemon_auth`)
- upstream: `target/interop/upstream-src/rsync-3.4.1/clientserver.c:758`
  (entry into `auth_server`) and `:1130-1156` (multiplex_out start)
- upstream: `target/interop/upstream-src/rsync-3.4.1/main.c:1245`
  (`setup_protocol`), `:1258` (`recv_filter_list`)

## 1. Auth-handshake-phase inventory (per accepted connection)

Numbered in protocol order, starting where DIS-4.b finished. Each row
cites file:line for the operation and tags syscalls (`S`), heap
allocations (`A`), lock or atomic acquisitions (`L`), and crypto
operations (`C`). Rows tagged `auth-only` execute only when
`auth users` is set on the module; rows tagged `always` execute on
every connection.

| # | Operation | Where | Cost tag | DIS-3 row | Bypass behaviour |
|---|-----------|-------|----------|-----------|------------------|
| 1 | `module.requires_authentication()` short-circuit on empty `auth_users` | `request.rs:156`, `definition.rs:191-193` | 0 | 9 | always - the predicate fires first |
| 2 | Bypass path: `send_daemon_ok` writes the cached `@RSYNCD: OK\n` and flushes | `request.rs:157`, `request.rs:83-90`, `legacy_messages.rs:52-58` | 1 S (`write`), 1 S (`flush`; no-op on `TcpStream`) | 9 | bypass only |
| 3 | `generate_auth_challenge`: 32-byte stack buffer, peer-IP `to_string` (`String` alloc) copied in, `SystemTime::now() -> duration_since(UNIX_EPOCH)` (1 `clock_gettime`), `process::id()` (cached after first call) | `authentication.rs:103-135` | 1 S (`clock_gettime`), 1 A (`String` for IP), 1 C (MD5 or MD4 hash of 32 bytes), 1 A (`Vec<u8>` from `to_vec`), 1 A (base64 `String`) | 9 | skipped |
| 4 | Write `@RSYNCD: AUTHREQD <challenge>\n` via `LegacyMessageCache::write`. The challenge is dynamic, so the cache falls through to `format_legacy_daemon_message` and returns an `Owned(String)`. | `authentication.rs:44-50`, `legacy_messages.rs:42-50`, `lines.rs:282-284` | 1 A (`String::with_capacity(LEGACY_DAEMON_PREFIX.len() + 32)`), 1 S (`write_all`), 1 S (`flush`; no-op on `TcpStream`) | 9 | skipped |
| 5 | Read client `<username> <response>\n` line via `read_trimmed_line` (1 fresh `String` alloc, 1 `BufReader::read_line` -> at most 1 `read(2)` to kernel) | `authentication.rs:54`, `greeting.rs:49-62` | 1 A, 1 S | 9 | skipped |
| 6 | Split response on first ASCII whitespace; trim leading whitespace from digest segment (zero-alloc on borrowed `&str`) | `authentication.rs:61-65` | 0 | 9 | skipped |
| 7 | `module.get_auth_user(username)` - linear scan over `auth_users: Vec<AuthUser>`; typically 1-5 entries | `request.rs (caller)`, `definition.rs:196-200` | 0 S, 0 A | 9 | skipped |
| 8 | `verify_secret_response` (rows 8a-8d) | `authentication.rs:155-193` | branch | 9 | skipped |
| 8a | `check_secrets_file_permissions` (`strict_modes` default true): `fs::metadata` -> `stat(2)` + 1 `nix::unistd::getuid` (cached) | `platform/src/secrets.rs:27-58`, `authentication.rs:167-169` | 1 S (`stat`), 0 A | 9 | skipped |
| 8b | `fs::read_to_string(secrets_path)` - one `open(2)`, one `fstat(2)`, one or more `read(2)`, one `close(2)`; allocates the file contents as a `String` | `authentication.rs:171` | 3-4 S, 1 A | 9 | skipped |
| 8c | Per non-comment line: `split_once(':')`, `user == username` compare. Default secrets file has 1-5 entries. | `authentication.rs:173-189` | 0 S, 0 A | 9 | skipped |
| 8d | On matched user: `verify_daemon_auth_response` -> `digests_for_response` to pick candidate digest by base64 length, then `compute_daemon_auth_response` (1 hash) plus `constant_time_eq` (byte-folded XOR) | `core/src/auth/mod.rs:230-257`, `:185-194`, `:269-284` | 1 C (digest of secret+challenge: MD5 default at protocol >= 30, optionally MD4/SHA-1/SHA-256/SHA-512 by response length), 1 A (`Vec<u8>` digest), 1 A (base64 `String`) | 9 | skipped |
| 9 | If granted: `send_daemon_ok` writes the cached `@RSYNCD: OK\n` and flushes | `request.rs:179`, `legacy_messages.rs:52-58` | 1 S (`write`), 1 S (`flush`; no-op) | 9 | skipped (rows 2 and 9 are mutually exclusive) |
| 9R | If denied: `send_auth_failed` formats `@ERROR: auth failed on module {module}\n`, writes payload, newline, cached `@RSYNCD: EXIT\n`, then `flush` | `authentication.rs:207-219`, `daemon.rs:123` (`AUTH_FAILED_PAYLOAD`) | 3 A (`String::replace` allocates a fresh `String`; `sanitize_module_identifier` may allocate), 4 S (`write`, `write`, `write`, `flush`) | 9 | skipped |
| 10 | `setup_protocol_with(stdout, stdin, config, &RsyncNegotiator)` - dyn-dispatch through `ProtocolNegotiator`; exchanges compat flags and negotiates checksum/compression algorithms in raw (pre-multiplex) mode | `transfer/src/lib.rs:411`, `setup/mod.rs:67-100`, `setup/negotiator.rs` | dyn-dispatch +`Box<dyn>` allocations on the negotiator; varint exchange triggers 2-4 `read(2)`/`write(2)` syscalls plus 2-6 small `Vec<u8>` allocations for the negotiated-algorithm tables; raw `stdout.flush()` afterwards | 14 | always |
| 11 | `requires_multiplex_output(client_mode=false, role, protocol, compat_flags)` - branches on `protocol.supports_multiplex_io()`; for protocol 32 returns `true` | `transfer/src/lib.rs:234-248`, `:478-486` | 0 S, 0 A | 15 | always |
| 12 | `ServerReader::new_plain(io::BufReader::with_capacity(64 * 1024, chained_stdin))` - allocates a fresh 64 KiB heap buffer for the multiplexed input ring | `transfer/src/lib.rs:473-474` | 1 A (64 KiB) | 15 | always |
| 13 | `ServerWriter::new_plain(stdout)` then `writer.activate_multiplex()?` - allocates a 64 KiB output ring buffer matching upstream `iobuf_out` | `transfer/src/lib.rs:476`, `:485` | 1 A (64 KiB), 0 S (no syscall until the first frame is written) | 15 | always |
| 14 | Daemon-mode server: `should_send_filter_list = false`. The sender role then calls into `recv_filter_list` from the per-role handler (mirrors upstream `main.c:1258`); receiver role enters `do_server_recv` and reads the filter list from the wire before flist build. | `transfer/src/lib.rs:493-501`, `:554-573` | 1+ S (`read(2)` for filter-list bytes; framing is `i32` length-prefixed varints), 1-N A (one `FilterRule` per rule; default config is the empty list -> a single `0i32` zero-length write+read) | 16 | always |
| 15 | Optional `MSG_IO_TIMEOUT` send (`protocol >= 31` + non-zero `io_timeout`) - 4-byte message via `writer.send_message(MessageCode::IoTimeout, ...)` | `transfer/src/lib.rs:524-532` | 0-1 S (buffered into the multiplex ring, no syscall until flush), 0 A | 16 | always, but zero work on default `timeout = 0` |

The phase ends at row 15: the next operation is `GeneratorContext::run`
(sender role) or `ReceiverContext::run` (receiver role). Both are owned
by DIS-4.d (flist build) and DIS-4.e (first-block send) respectively.

### Allocation tally per connection

Counting rows 1-15:

**Anonymous (bypass) case**, `auth users` not set:

- Row 1 (`requires_authentication`): 0.
- Row 2 (`send_daemon_ok`): 0 alloc (cached `&[u8]`), 1-2 syscalls
  (`write` + no-op `flush`).
- Rows 3-9R: not executed.
- Row 10 (`setup_protocol`): 2-6 small `Vec<u8>` plus `Box<dyn>`
  allocations for negotiated algorithm tables.
- Row 12 (multiplex input ring): 1 A (64 KiB).
- Row 13 (multiplex output ring): 1 A (64 KiB).
- Row 14 (filter list receive, empty default): 1 S (`read(2)` of the
  trailing zero), 0 A.

**Total bypass: ~3-7 heap touches, ~3-5 syscalls past the OK write.**

**Authenticated case**, default `strict_modes = true`, one user in
secrets file:

- Rows 1-2: 0 alloc, 0 syscall (bypass branch not taken).
- Row 3 (challenge): 1 `clock_gettime`, 3 allocs (IP `String`,
  digest `Vec<u8>`, base64 `String`), 1 hash compute (MD5 of 32 bytes).
- Row 4 (write AUTHREQD): 1 alloc (`String` for the formatted line),
  1 `write`, 1 no-op `flush`.
- Row 5 (read username+response): 1 alloc, 1 `read`.
- Row 8a (`stat` secrets): 1 syscall.
- Row 8b (`read_to_string` secrets): ~3-4 syscalls
  (`open`+`fstat`+`read`+`close`), 1 alloc.
- Row 8d (compute + constant-time compare): 1 hash compute (MD5 of
  secret-length + 22-byte challenge), 2 allocs (digest `Vec<u8>`,
  base64 `String`).
- Row 9 (`send_daemon_ok` on grant): 0 alloc, 1-2 syscalls.
- Rows 10-15 (post-auth setup, multiplex, filter list): same ~3-7
  heap touches and ~3-5 syscalls as the bypass case.

**Total authenticated: ~11-15 heap touches, ~10-13 syscalls, 2 hash
computes (one for the challenge, one for the verification).**

## 2. Upstream comparison (rsync 3.4.1, default config)

Upstream walks the same logical phases but in C with stack buffers and
no per-connection algorithm-table allocations after the daemon has
started. Per accepted connection, upstream pays:

| Category | oc-rsync | upstream 3.4.1 | Delta |
|----------|----------|----------------|-------|
| Bypass branch (no `auth users`) | Two `Arc`-clone-free predicate calls plus one cached `@RSYNCD: OK\n` write | One `if (!users || !*users) return ""` early-out in `auth_server` (`authenticate.c:239-240`); the `@RSYNCD: OK\n` is written by the caller in `clientserver.c` via `io_printf` | Symmetric on the bypass path. Both implementations exchange zero auth bytes. |
| Challenge generation | `Md5::new() -> update(32 bytes) -> finalize()` (or MD4 for protocol < 30), with peer IP `to_string` plus a `String::to_vec` and a base64 `String` alloc | `gen_challenge` (`authenticate.c:61-81`): `sys_gettimeofday` into a 32-byte stack buffer, `sum_init/sum_update/sum_end` into a stack `digest[MAX_DIGEST_LEN]`, `base64_encode` into the caller's stack `challenge[MAX_DIGEST_LEN*2]` | +3 heap allocs on oc-rsync (peer-IP `String`, digest `Vec<u8>`, base64 `String`); same syscall and crypto count |
| Negotiation of digest algorithm | Implicit at the response-length disambiguation step (`digests_for_response`) at verify time; the daemon never sends a digest-choice line on the wire because the digest list is already advertised in the `@RSYNCD:` greeting (`session_runtime.rs:224` calls `legacy_daemon_greeting`) | `negotiate_daemon_auth(f_out, 0)` (`compat.c:847`) is called inside `auth_server` *before* `gen_challenge` and runs `parse_negotiate_str` over the daemon's own `daemon_auth_choices` setting. With no client-side advertised digest (rsync versions before 3.2.7 advertised nothing), upstream falls back to `protocol_version >= 30 ? "md5" : "md4"` (`compat.c:858-861`). | oc-rsync moved the choice to the greeting (DIS-4.a row 15-16); upstream does it inside the auth path. **Net difference: zero on-wire bytes from negotiation in either implementation** for a same-protocol client (the table is loaded from local config in both cases). |
| AUTHREQD write | `LegacyMessageCache::write` falls through to `format_legacy_daemon_message`, allocating a fresh `String::with_capacity(LEGACY_DAEMON_PREFIX.len() + 32)` plus the boxed render | `io_printf(f_out, "%s%s\n", leader, challenge)` (`authenticate.c:245`) writes into the ringbuffer with zero heap allocation | +1 heap alloc on oc-rsync |
| Read username+response | `BufReader::read_line` into a fresh `String::new()` allocated by `read_trimmed_line` | `read_line_old(f_in, line, sizeof line, 0)` into a `BIGPATHBUFLEN` stack buffer (`authenticate.c:247`) | +1 heap alloc on oc-rsync |
| Secrets file lookup | `fs::read_to_string(secrets_path)` slurps the whole file; iterates lines; computes hash only for the matched user | `fopen(fname, "r")` + `fgets(line, sizeof line, fh)` in a `while` loop (`authenticate.c:113`, `:141`), computing `generate_hash` per *non-empty non-comment* line until it finds the user (`:156`) | Symmetric on the *match* case (both compute one verification hash). Upstream computes one hash per matching-prefix candidate line; oc-rsync only computes the hash once it has a full username equality. **oc-rsync is faster** for secrets files with many users sharing prefix prefixes; symmetric for typical 1-5-entry files. **+1 buffer alloc on oc-rsync** (the slurped `String`) versus upstream's stack `line[1024]`. |
| `strict_modes` permission check | `fs::metadata(path)` (one `stat(2)`) before `read_to_string` (which also does an `open` + `fstat`) | `do_fstat(fileno(fh), &st)` (`authenticate.c:116`) on the already-opened `FILE*` | **+1 extra `stat(2)` on oc-rsync** (the `metadata` call is redundant once `read_to_string` opens the file - `fstat` on the fd would suffice) |
| Verify comparison | Constant-time XOR fold (`constant_time_eq`) | `strcmp(pass, pass2) == 0` (`authenticate.c:157`) | Symmetric crypto cost; oc-rsync is timing-attack-resistant (intentional security improvement, documented in `core/src/auth/mod.rs:223-228`) |
| Auth-failure framing | `AUTH_FAILED_PAYLOAD.replace("{module}", ...)` -> `write` + `\n` + cached `EXIT` + `flush` | `io_printf(f_out, "@ERROR: auth failed on module %s\n", name)` (`clientserver.c:762`) - no explicit EXIT in upstream; the caller returns `-1` and the connection closes | Symmetric byte output for the `@ERROR` line; **oc-rsync emits one extra `@RSYNCD: EXIT\n` line** that upstream does not. Wire-compatibility note: clients tolerate trailing data on a closed connection, but the extra bytes are not in upstream's reply. **Worth re-checking against upstream client behaviour under `-vv`.** |
| Compat exchange (`setup_protocol`) | `setup::setup_protocol` with a dyn-dispatch `ProtocolNegotiator` trait object; allocates 2-6 small `Vec<u8>` for the negotiated algorithm tables (`negotiator.rs`) | `setup_protocol(f_out, f_in)` (`compat.c:572-644`): all state is in module-level `static` storage; zero heap allocation; same wire bytes | **+3-7 heap allocs on oc-rsync** |
| Multiplex output activation | `ServerWriter::new_plain(stdout)` + `activate_multiplex()` allocates a 64 KiB ring buffer | `io_start_multiplex_out(f_out)` (`io.c`): `iobuf_out` is a single global `static` array allocated once at process start and reused across the daemon's `fork()`ed children | **+64 KiB heap alloc per connection** on oc-rsync. Upstream reuses a process-static buffer. |
| Multiplex input activation | `ServerReader::new_plain(io::BufReader::with_capacity(64 * 1024, ...))` allocates a 64 KiB ring buffer | `io_start_multiplex_in(f_in)` (`io.c`): same `iobuf` static reused | **+64 KiB heap alloc per connection** on oc-rsync. |
| Receive filter list | Read trailing `0i32` (empty default filter list) into the receiver's `FilterChain` builder via `protocol::filters::read_filter_list` | `recv_filter_list(f_in)` (`exclude.c`) reads the same `i32` length-prefixed loop | Symmetric on the empty-list case; both pay one `read(2)` for the zero length and zero allocations. |
| `MSG_IO_TIMEOUT` send | Conditional on `protocol >= 31` + non-zero `io_timeout`; emits one 4-byte `MSG_IO_TIMEOUT` frame buffered in the multiplex ring | Same conditional at `clientserver.c:1130-1156` and `main.c:1249-1250`; emits one `send_msg_int` of equivalent payload | Symmetric. |

**Allocation diff:**

- **Bypass case**: oc-rsync ~3-7 allocs per connection (multiplex
  rings + setup_protocol algorithm tables); upstream ~0 (all
  process-static). Net delta is **~+3-7 allocations per connection
  past the OK write** plus **+128 KiB resident per connection** that
  upstream amortises across fork-shared static storage.
- **Authenticated case**: oc-rsync ~11-15 allocs per connection;
  upstream ~0-1 (the `strdup(users)` and `strdup(line)` in
  `auth_server`). Net delta is **~+11-15 allocations per connection**
  plus the **+128 KiB multiplex-ring resident set**.

**Syscall diff (steady state, authenticated case):**

- Both implementations: 1 `clock_gettime`, 1 `write` (AUTHREQD),
  1 `read` (username+response), 1 `open`+`read`+`close` on the
  secrets file, 1 `write` (OK or `@ERROR`), and the post-auth
  compat exchange and filter-list reads.
- **oc-rsync adds one redundant `stat(2)`** at row 8a (`fs::metadata`
  inside `check_secrets_file_permissions` before `fs::read_to_string`
  re-opens the file). Upstream uses `fstat` on the already-open `FILE*`.
- **oc-rsync writes one extra line** (`@RSYNCD: EXIT\n`) on the
  auth-failure path that upstream omits. Not strictly a syscall
  diff (the write is one buffered `write_all` on the cached EXIT
  bytes) but it is one extra `write(2)` on the wire.

**Roundtrip count diff:**

- **Bypass case (anonymous module)**: oc-rsync 0 roundtrips on rows
  9; upstream 0 roundtrips. **Net delta: 0.**
- **Authenticated case**: oc-rsync 1 roundtrip (`AUTHREQD` -> client
  response); upstream 1 roundtrip on the same wire bytes. **Net
  delta: 0 application-level roundtrips.**
- The pre-auth digest-list advertisement on oc-rsync (`@RSYNCD: 32.0
  sha512 sha256 sha1 md5 md4\n`) folds into the greeting and is
  accounted for under DIS-4.a row 3; it does not add a separate
  roundtrip because the daemon sends it as part of the greeting and
  the client reads it in the same `recv()` window as the version line.

**Crypto-op diff:**

- **Bypass case**: 0 crypto ops on both sides.
- **Authenticated case**: 2 hash computes on both sides (one in
  `gen_challenge`/`generate_auth_challenge`, one in
  `check_secret`/`verify_secret_response`). **Net delta: 0.**

## 3. Top contributors (ranked by estimated wall-clock cost)

Cost estimates assume the DIS-1 small-files cold-start scenario
(500 files, 1 KiB each, loopback, Debian-glibc allocator) running in
the rsync-profile container. Because the DIS-1 corpus has no `auth
users` set, the cold-start gap attributable to DIS-4.c is bounded by
rows 10-14 (compat exchange + multiplex activation + filter-list
read), all of which run on every connection regardless of auth.

### 1. Per-connection 64 KiB multiplex ring buffers (DIS-3 row 15)

`transfer/src/lib.rs:473-474` and `:476`. Every accepted connection
allocates two 64 KiB ring buffers (one input via
`BufReader::with_capacity`, one output via
`ServerWriter::activate_multiplex`). Upstream uses a single pair of
`static` `iobuf_in` / `iobuf_out` arrays that fork-shared children
inherit. On the cold-start path the allocator returns these from the
free list when the daemon is warm; the first call after start hits
glibc's arena and triggers an `mmap` for the second 64 KiB chunk.

- Expected cost: **~5-15 us per accept** on a warm allocator,
  **~50-200 us** on the cold first-connection.
- Allocation contributes the dominant share of the auth-phase
  wall-clock on the cold-start measurement; the 128 KiB resident
  growth per connection is the largest fixed overhead in this audit.

### 2. `setup_protocol` dyn-dispatch + algorithm table allocations (DIS-3 row 14)

`transfer/src/lib.rs:411`, `setup/mod.rs:67-100`,
`setup/negotiator.rs`. The negotiator is wrapped in a
`&dyn ProtocolNegotiator` trait object and the algorithm-list state
is rebuilt per connection as small `Vec<u8>` allocations. Upstream's
`setup_protocol` writes the same wire bytes from module-level static
storage with zero heap activity.

- Expected cost: **~10-30 us per accept** (vtable dispatch is
  negligible; the allocation count is the dominant cost on glibc).
- Wire-compatibility: byte-identical to upstream (same negotiation
  bytes regardless of the implementation detail).

### 3. Redundant `stat(2)` on secrets-file permission check (auth-only, DIS-3 row 9)

`platform/src/secrets.rs:27-58` (`check_secrets_file_permissions`
calls `fs::metadata`), then `authentication.rs:171` calls
`fs::read_to_string` which does its own `open`+`fstat`. Two `stat`
calls reach the kernel where upstream uses one `fstat` on the
already-open `FILE*` (`authenticate.c:116`).

- Expected cost: **~2-5 us per authenticated accept** on a warm
  page cache; **~50-200 us** on a cold cache (one extra disk seek).
- Cosmetic on the cold-start corpus (no auth set), but a measurable
  regression for daemons with auth turned on under load.
- Fix is a one-liner: collapse the permission check and read into a
  single `File::open` + `Metadata::from(file.metadata())` chain that
  reuses the open fd.

### 4. `LegacyMessageCache` AUTHREQD render allocates per call (auth-only, DIS-3 row 9)

`authentication.rs:44-50`, `legacy_messages.rs:42-50`,
`lines.rs:282-284`. The cache only flyweights the `OK` and `EXIT`
strings; every dynamic message (including `AUTHREQD <challenge>`)
falls through to `format_legacy_daemon_message`, which allocates a
fresh `String` of capacity 32+prefix. Upstream writes the same line
via `io_printf` straight into the ringbuffer.

- Expected cost: **~1-3 us per authenticated accept**, dominated by
  the allocation rather than the formatting compute.
- Wire-compatibility: byte-identical.

### 5. Auth-failure path emits an extra `@RSYNCD: EXIT\n` line

`authentication.rs:217` calls `messages.write_exit` after the
`@ERROR: auth failed on module ...` payload. Upstream `clientserver.c:762`
writes only the `@ERROR` line and lets the connection close. The
extra line is harmless on tolerant clients but is wire-divergent
from upstream and should be verified against the upstream client's
`-vv` log output before being kept.

- Expected cost (auth-failed path): **~1-2 us per rejection**,
  negligible; flagged as a divergence to confirm, not a perf
  contributor.

## 4. Recommendations (ranked; DIS-6 implements)

### R1. Pool the 64 KiB multiplex input/output ring buffers

`transfer/src/lib.rs:473-476`. Pool both the `BufReader` input buffer
and the multiplex output buffer behind a
`crossbeam_queue::ArrayQueue<Box<[u8; 65536]>>` sized to the daemon's
`max connections`. Hand out at session start, return via RAII on
session end. Identical pattern to DIS-4.a R4 (greeting `BufReader`
pool). Removes the dominant **128 KiB per-connection resident
growth** in this audit and eliminates 2 large `mmap`-class allocations
on the first connection after start. Wire-compatibility: unaffected
(same ring semantics, different storage).

### R2. Cache the negotiated algorithm tables in a `OnceLock`

`transfer/src/setup/negotiator.rs`. The algorithm tables for protocol
32 are fixed at compile time; the only per-connection variability is
the `(compress, checksum)` choice from the client's compat-flags
exchange. Pull the table-building logic behind a `LazyLock<...>` so
the per-connection cost collapses to a single `(Strategy, Strategy)`
tuple decode. Reduces row-10 allocation count from 2-6 to 0-1.
Wire-compatibility: byte-identical.

### R3. Collapse the secrets-file `stat` + `open` into a single fd

`platform/src/secrets.rs:27-58` and
`authentication.rs:167-171`. Replace `fs::metadata` + `fs::read_to_string`
with `File::open(secrets_path)` once, then call
`file.metadata()?` for the permission check and `io::read_to_string(file)`
for the content read. Eliminates the redundant `stat(2)` and matches
upstream's `fstat`-on-the-`FILE*` pattern. Wire-compatibility:
unaffected.

### R4. Render AUTHREQD into a stack buffer or pooled `String`

`authentication.rs:44-50`. The AUTHREQD line is bounded at
`@RSYNCD: AUTHREQD ` (18 bytes) + the base64 challenge (22 bytes for
MD4/MD5, 86 for SHA-512) + `\n` = at most 107 bytes. Write directly
into a stack `[u8; 128]` via `write!` on a `&mut [u8]` cursor, or
reuse a thread-local `String` cleared between sessions. Removes 1
heap touch per authenticated connection. Wire-compatibility:
byte-identical.

### R5. Re-verify the auth-failed `@RSYNCD: EXIT\n` emission

`authentication.rs:215-218`. Run an interop test against upstream
3.4.1 with a bad password and capture the exact bytes the daemon
sends back. If upstream omits the EXIT line (as the C source
suggests), drop it from `send_auth_failed` to stay byte-identical.
If upstream does emit it (under some build configuration the audit
missed), keep it and add a golden-byte test in
`crates/protocol/tests/golden/` so future refactors do not
regress. Wire-compatibility: TBD (this is the test).

### R6. Defer the `setup_protocol` dyn-dispatch behind a generic

`crates/transfer/src/setup/mod.rs:67-100`. `setup_protocol_with` is
already generic over `&'a dyn ProtocolNegotiator`. Monomorphise the
hot path by inlining `RsyncNegotiator` at the `setup_protocol` entry
(replace `&RsyncNegotiator` with a generic `N: ProtocolNegotiator`).
The dyn-dispatch itself is negligible (one vtable lookup), but the
boxed-trait API forces several small allocations inside the negotiator
that monomorphisation can elide. Confirm under `perf record` before
landing. Wire-compatibility: unaffected.

## 5. Cross-reference: DIS-3 phases covered

This audit covers DIS-3's phase rows:

- **Row 9** - Authentication path (skipped when `auth users` not
  set) -> R3, R4, R5. Bypass case: rows 1-2 above
  (`requires_authentication()` short-circuit + cached `@RSYNCD: OK\n`
  write); authenticated case: rows 3-9R above.
- **Row 14** - `setup_protocol` (compat flags + capability +
  checksum seed) -> R2, R6.
- **Row 15** - Multiplex output activation -> R1.
- **Row 16** - Receive filter list - reviewed; rows 14 above
  (`recv_filter_list` from the per-role handler). On the default
  cold-start corpus (no `--filter`/`--include`/`--exclude` sent by
  the client), the filter list is a single trailing `0i32` and is
  byte-symmetric with upstream. No dedicated recommendation needed.
  Tracked as a future investigation if the filter-list parsing path
  ever shows up under filter-heavy workloads.

DIS-3 rows 5-8 and 11-13 belong to **DIS-4.b** (module-select
roundtrip), which begins where row 5 picks up the client's module
name and ends where row 13 finishes privilege/chroot/Landlock
setup just before this audit's row 1. DIS-3 rows 17-19 belong to
**DIS-4.d** (flist build): the first operation after this audit's
row 15 is `GeneratorContext::run` -> `build_file_list` (sender
role) in `crates/transfer/src/generator/file_list/mod.rs:52`.
DIS-3 rows 20-22 belong to **DIS-4.e** (first-block send):
`send_file_list` and the first NDX request.

## 6. Anonymous-module bypass (`auth users` empty)

When the module definition has an empty `auth_users` vector
(`definition.rs:191-193`), the entire auth handshake collapses to:

1. One `requires_authentication()` predicate call - `0` syscalls,
   `0` allocations, `0` crypto ops (`request.rs:156`).
2. One `send_daemon_ok` write of the cached `@RSYNCD: OK\n` bytes -
   one `write(2)` of 13 bytes, one no-op `TcpStream::flush`,
   `0` allocations (`request.rs:157-158`,
   `legacy_messages.rs:52-58`).
3. Direct handoff to `process_approved_module` continuation at
   `transfer.rs:438` (early exec, then arg parsing, then
   `setup_protocol`).

Bypass-case roundtrip count past the OK write: **0 application-level
roundtrips contributed by this audit's rows 1-9R.** Rows 10-15 still
execute and account for the **3-7 heap touches and 3-5 syscalls**
detailed in section 1. The 128 KiB resident growth (R1) is paid on
every connection, authenticated or not.

Upstream behaviour matches: `auth_server` at `authenticate.c:239-240`
short-circuits with `return ""` on empty `users`, and the caller in
`clientserver.c` proceeds to `setup_protocol` (`main.c:1245`) and the
multiplex/filter-list path identically. **Bypass-case roundtrip-count
diff between oc-rsync and upstream is zero**; the diff lives entirely
in rows 10-15 and is allocation-dominated (R1, R2, R6).

## 7. Confidence and what DIS-2 should confirm

- **High confidence**: R1 (multiplex ring buffer pooling), R3
  (redundant `stat`), R4 (AUTHREQD `String` alloc). All three are
  code-readable; the costs follow directly from counting
  allocations and syscalls.
- **Medium confidence**: R2 (algorithm-table caching) and R6
  (dyn-dispatch monomorphisation). The vtable cost is negligible
  on its own; the win comes from the *inlinable* allocation paths
  inside `RsyncNegotiator`. Needs a flame graph against
  `setup_protocol` to confirm the alloc-cluster size.
- **Low confidence**: R5 (auth-failed EXIT line wire divergence).
  The C source does not emit EXIT, but the upstream test fixtures
  in `target/interop/upstream-src/rsync-3.4.1/` may have an
  end-of-connection handler in `clientserver.c` that this audit
  did not trace. Verify with an actual upstream-vs-oc-rsync
  packet capture before landing the removal.

DIS-2 should re-run the harness with `perf record -F 999 -g
--call-graph fp` against `run_server_with_handshake` and look for:

- Whether the 64 KiB multiplex buffer allocations are visible in
  the flame graph as a `glibc malloc`/`mmap` cluster. If yes, R1
  alone explains the bulk of the row-15 cost and stacks cleanly
  with the DIS-4.a `BufReader` pool work.
- Whether the `setup_protocol` allocation pattern shows up as a
  separate cluster. If it is concentrated in
  `ProtocolNegotiator::negotiate_*`, R2 is the right next move.
- For an `auth users`-configured daemon (the secondary measurement
  scenario), whether the secrets-file `stat` + `open` show up as
  two separate `__x64_sys_newstat` / `__x64_sys_openat` frames.
  Confirming this would validate R3.

## 8. Related audits

- `docs/audits/dis-3-cold-start-phase-decomposition.md` - parent
  task this audit feeds.
- `docs/audits/dis-4a-rsyncd-greeting-overhead.md` - sibling audit
  covering DIS-3 rows 2-4 and 10 (greeting build + admission).
- `docs/audits/dis-4b-module-select-roundtrip.md` - sibling audit
  covering DIS-3 rows 5-8 and 11-13 (module lookup, host check,
  arg parsing).
- `docs/audits/dis-4d-flist-build-cold-start.md` - downstream audit
  covering DIS-3 rows 17-19 (flist build), which this audit hands
  off to.
- `docs/audits/dis-4e-first-block-send-latency.md` - downstream
  audit covering DIS-3 rows 20-22 (first-block send).
- `docs/audits/daemon-handshake-overhead.md` - prior inventory and
  mitigation list; R1 realigns with its proposed `BufReader` pool
  and extends the pattern to the multiplex rings.
