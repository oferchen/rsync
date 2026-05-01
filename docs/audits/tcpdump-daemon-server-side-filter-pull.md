# Audit: tcpdump comparison for daemon-side server filter pull

Status: documentation only - no code changes.
Tracking issue: #1697.
Upstream reference: `target/interop/upstream-src/rsync-3.4.1/`.

**Last verified:** 2026-05-01 against
`crates/daemon/src/daemon/sections/module_access/helpers.rs`,
`crates/daemon/src/daemon/sections/module_access/transfer.rs`,
`crates/transfer/src/receiver/transfer.rs`,
`crates/transfer/src/generator/filters.rs`,
`crates/transfer/src/generator/file_list/walk.rs`,
`crates/transfer/src/lib.rs`,
`crates/protocol/src/filters/wire.rs`,
`crates/protocol/src/filters/prefix.rs`, and the upstream sources cited
inline.

## Headline finding

**No active wire-byte divergence today.** A daemon-side server filter
(`filter` / `exclude` / `include` / `exclude from` / `include from` in a
module section of `oc-rsyncd.conf`) does not produce any bytes on the
wire in either upstream rsync 3.4.1 or oc-rsync. Server-side rules are
applied locally by the daemon as the file list is built; the client sees
nothing more than a file list with the filtered entries omitted. The
remaining wire surface (filter exchange, file list, delta, goodbye) is
identical to a daemon module configured without any filter directives,
which was already shown to be byte-equivalent by PR #3513
(`docs/audits/tcpdump-daemon-proto28-29.md`). The hangs and
mis-applications fixed by PRs #1698, #1703-#1705, and #1707-#1715 closed
the last known regressions in this path; no remaining gap was found
during this audit.

This audit is the wire-trace complement of `docs/daemon/filter-precedence.md`
(PR #1888), which documents the precedence/evaluation order. Where that
document answers "in what order are rules combined", this one answers
"what bytes appear on the wire when those rules are in effect".

---

## Scenario

The wire flow being compared is:

```
[client]              [daemon]
   |                     |
   |--- TCP connect ---->|
   |<--- @RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n
   |--- @RSYNCD: 32.0\n -->|
   |--- mod\n ----------->|        # module name
   |<--- @RSYNCD: OK\n ---|
   |--- arg list (-e.LsfxCIvu, --server, --sender, -av, ., .) -->|
   |--- empty line -------->|     # end of args
   |<-- compat-flags varint
   |<-- checksum-seed
   |--- (optional) MSG_DATA filter list (delete-related) -->|
   |<-- file list           # daemon's flist - rules already applied
   |<-- delta stream
   |--- NDX_DONE          # goodbye
   |<-- NDX_DONE / stats
```

`oc-rsyncd.conf` provides the daemon's filter directives:

```ini
port = 8730
use chroot = no

[mod]
    path = /tmp/audit-1697/srv
    read only = true
    # Server-side rules (NOT visible on the wire)
    filter = - *.tmp + *.rs - *.bak
    include = src/ tests/
    exclude = .git/ target/ node_modules/
    include from = /etc/oc-rsyncd/include.txt
    exclude from = /etc/oc-rsyncd/exclude.txt
```

Client invocation (pull, with `--archive` and an arbitrary CLI filter):

```sh
rsync -av --filter='+ src/foo.rs' --exclude='*.log' \
    rsync://host:8730/mod/ /tmp/dest/
```

Roles:

- **Client** = receiver (`am_sender == 0`).
- **Daemon** = sender (`am_sender == 1`, oc-rsync `ServerRole::Generator`).

The server-side filter rules from the module config are loaded into a
process-local list (`daemon_filter_list` upstream,
`config.daemon_filter_rules` in oc-rsync) and consulted as the daemon
walks the source tree. They never serialise out of `send_filter_list()`
because that function only sends the daemon's own client-set
`filter_list` - which is empty on the daemon process.

---

## Wire phases

Below is each phase of the session annotated with whether the
server-side filter rules have any wire-visible effect.

| Phase                | Daemon-side filter influence on the bytes? | Rationale |
|----------------------|---------------------------------------------|-----------|
| 1. TCP handshake     | No                                          | Pre-protocol. |
| 2. Daemon greeting   | No                                          | Filter rules not yet loaded. |
| 3. Client greeting   | No                                          | Client has no knowledge of the daemon's filter list. |
| 4. MOTD / module sel | No                                          | Pre-`rsync_module()` - rules not loaded yet. |
| 5. Auth (if any)     | No                                          | Filter rules loaded only after auth succeeds. |
| 6. Argv exchange     | No                                          | Rules are local config; not sent as args. |
| 7. compat-flags / seed | No                                        | Capability-level only; daemon list does not affect compat flags. |
| 8. Filter exchange   | No                                          | Daemon reads the client's CLI-supplied rules; daemon's own list is never written. |
| 9. File list         | **Indirectly**                              | Filtered-out entries are simply absent from the list. The framing is identical. |
| 10. Delta stream     | **Indirectly**                              | Only the unfiltered files appear; the delta block-matching itself is unchanged. |
| 11. Goodbye / stats  | No                                          | Stats reflect the actually-transferred set, but the framing is unchanged. |

The two phases marked *Indirectly* are the only places a tcpdump
operator could detect that filtering happened: by comparing the file
count in the flist or the byte volume of the delta stream against an
unfiltered baseline. The byte format of those phases is unchanged.

---

## Byte-level comparison

The byte-format comparison below covers every phase where a regression
*could* introduce a divergence from upstream's behaviour.

### Phase 1-2. TCP and greeting

Pre-`rsync_module()`. No daemon filter rules are loaded yet. Bytes are
identical to the proto-28/29 audit (`docs/audits/tcpdump-daemon-proto28-29.md`
sections 2.1-2.2) and to a daemon module with no filter directives.

### Phase 3-5. Module selection and auth

Upstream loads `daemon_filter_list` only after authentication
succeeds, at `target/interop/upstream-src/rsync-3.4.1/clientserver.c:874-893`:

```c
p = lp_filter(module_id);
parse_filter_str(&daemon_filter_list, p, rule_template(FILTRULE_WORD_SPLIT),
    XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3);

p = lp_include_from(module_id);
parse_filter_file(&daemon_filter_list, p, rule_template(FILTRULE_INCLUDE),
    XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3 | XFLG_OLD_PREFIXES | XFLG_FATAL_ERRORS);

p = lp_include(module_id);
parse_filter_str(&daemon_filter_list, p,
    rule_template(FILTRULE_INCLUDE | FILTRULE_WORD_SPLIT),
    XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3 | XFLG_OLD_PREFIXES);

p = lp_exclude_from(module_id);
parse_filter_file(&daemon_filter_list, p, rule_template(0),
    XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3 | XFLG_OLD_PREFIXES | XFLG_FATAL_ERRORS);

p = lp_exclude(module_id);
parse_filter_str(&daemon_filter_list, p, rule_template(FILTRULE_WORD_SPLIT),
    XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3 | XFLG_OLD_PREFIXES);
```

oc-rsync mirrors the same five-step construction, in the same order, in
`build_daemon_filter_rules()` at
`crates/daemon/src/daemon/sections/module_access/helpers.rs:223-280`:

1. `module.filter` -> `parse_daemon_filter_token()` per word-split token.
2. `module.include_from` -> read patterns from file, append as include rules.
3. `module.include` -> bare patterns (whitespace-split), append as include rules.
4. `module.exclude_from` -> read patterns from file, append as exclude rules.
5. `module.exclude` -> bare patterns (whitespace-split), append as exclude rules.

The list is produced as `Vec<FilterRuleWireFormat>` and stashed onto the
session's `ServerConfig` at
`crates/daemon/src/daemon/sections/module_access/transfer.rs:417-424`:

```rust
match build_daemon_filter_rules(module) {
    Ok(rules) => config.daemon_filter_rules = rules,
    Err(err) => { /* @ERROR + exit */ }
}
```

The rules never leave the daemon process. Wire-byte impact: zero.

### Phase 6. Argv exchange

The arg list a daemon-mode pull sends is fixed by client-side code
paths (`crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs`)
plus `--server --sender` and the source/dest sentinels. None of those
arg lines are derived from the daemon's filter directives.

A regression that leaked the module's filter strings into the daemon's
arg view would surface immediately because the daemon does not write
its own argv to the client - it only reads. There is no symmetric
write path to leak through.

### Phase 7. Compat-flags and checksum seed

Same wire bytes as a no-filter daemon. The daemon's filter list is not
referenced when computing `compat_flags`. Reference:
`target/interop/upstream-src/rsync-3.4.1/compat.c:710-745`
(`compat_flags = allow_inc_recurse ? CF_INC_RECURSE : 0` etc.) and
oc-rsync `crates/protocol/src/version/protocol_version/capabilities.rs`
where every compat-flag bit is gated on protocol version, not filter
state.

### Phase 8. Filter exchange (the hot zone)

This is where one might *expect* the daemon's filter list to traverse
the wire. It does not. The client always sends its own
`--filter` / `--include` / `--exclude` rules (only when
`receiver_wants_list` is set, i.e. with `--delete` or
`--prune-empty-dirs`). The daemon reads those rules off the wire and,
on the **server-sender** side (i.e. a pull), prepends its own
`daemon_filter_list` to them in process memory.

Upstream wire path (sender-side, server mode):
`target/interop/upstream-src/rsync-3.4.1/main.c:1252-1259`:

```c
if (am_sender) {
    keep_dirlinks = 0;
    if (need_messages_from_generator)
        io_start_multiplex_in(f_in);
    else
        io_start_buffering_in(f_in);
    recv_filter_list(f_in);
    do_server_sender(f_in, f_out, argc, argv);
}
```

`recv_filter_list()` at
`target/interop/upstream-src/rsync-3.4.1/exclude.c:1672-1698` reads
4-byte LE length prefixes via `read_int(f_in)` and parses each rule
into the process-local `filter_list`. Critically,
**`daemon_filter_list` is never written to `f_out` in this path** - it
is consulted directly by `path_is_daemon_excluded()`
(`flist.c:252-273`) and `name_is_excluded()` (`exclude.c:1010-1015`).

oc-rsync's daemon-sender wire path is in
`crates/transfer/src/generator/transfer.rs:728-729`:

```rust
// upstream: main.c:1258 - recv_filter_list() in server mode
self.receive_filter_list_if_server(&mut reader)?;
```

which calls `receive_filter_list_if_server()` in
`crates/transfer/src/generator/filters.rs:42-87`:

```rust
// Server mode: read filter list from client (MULTIPLEXED for protocol >= 30)
let wire_rules = read_filter_list(reader, self.protocol)?;

// upstream: clientserver.c:rsync_module() - daemon_filter_list is applied
// on top of client filters. Daemon rules take precedence (prepended).
let daemon_rules = &self.config.daemon_filter_rules;
let combined = if daemon_rules.is_empty() {
    wire_rules
} else if wire_rules.is_empty() {
    daemon_rules.clone()
} else {
    let mut combined = daemon_rules.clone();
    combined.extend(wire_rules);
    combined
};

// Convert wire format to FilterChain
if !combined.is_empty() {
    let (filter_set, merge_configs) = self.parse_received_filters(&combined)?;
    self.filter_chain = FilterChain::new(filter_set);
    for config in merge_configs {
        self.filter_chain.add_merge_config(config);
    }
}
```

The combined list is held only in `self.filter_chain` - a process-local
data structure. There is no `write_filter_list()` call on the daemon
sender path. The wire bytes for phase 8 are therefore bit-for-bit
identical between upstream and oc-rsync, and identical to a daemon with
no filter directives.

The receiver-side equivalent path (when the daemon is the receiver in a
push transfer) is at `crates/transfer/src/receiver/transfer.rs:856-901`
and is structurally the same: read client rules off the wire via
`read_filter_list()`, prepend `self.config.daemon_filter_rules`, build
a `FilterChain`. Again, no write path.

#### Sub-case: receiver_wants_list is false

For a plain pull without `--delete` or `--prune-empty-dirs`, the client
does not send a filter list at all. From
`crates/transfer/src/lib.rs:509-522`:

```rust
// upstream: exclude.c:1650 - am_sender && !receiver_wants_list skips sending.
let receiver_wants_filter_list = config.flags.delete || config.flags.prune_empty_dirs;

let should_send_filter_list = if config.connection.client_mode {
    match config.role {
        ServerRole::Generator => receiver_wants_filter_list,
        ServerRole::Receiver => true,
    }
} else {
    false
};
```

Upstream's identical guard is in `exclude.c:1647-1651`:

```c
int receiver_wants_list = prune_empty_dirs
    || (delete_mode && (!delete_excluded || protocol_version >= 29));

if (local_server || (am_sender && !receiver_wants_list))
    f_out = -1;
```

In this case the daemon-sender's `recv_filter_list(f_in)` reads only
the 4-byte `0` terminator from the wire, and oc-rsync's
`read_filter_list()` does the same: it loops on `read_i32_le()` until
it sees `0`. Confirmed at
`crates/protocol/src/filters/wire.rs:181-210`:

```rust
loop {
    let len = read_i32_le(reader)?;
    if len == 0 {
        break; // Terminator
    }
    ...
}
```

The wire bytes for phase 8 are exactly four zero bytes - regardless of
whether the daemon module has any filter directives configured.

### Phase 9. File list (where the filtering becomes observable)

Upstream applies `daemon_filter_list` while building the file list, in
two callsites:

1. `flist.c:1262` (`generator.c` for the file-by-file send loop's
   feedback channel):
   ```c
   if (daemon_filter_list.head && (*fname != '.' || fname[1])) {
       if (check_filter(&daemon_filter_list, FLOG, fname, is_dir) < 0) {
           if (is_dir < 0)
               return;
           ...
       }
   }
   ```
2. `flist.c:252-273` (`path_is_daemon_excluded()`), called from inside
   `make_file()` and from the file-stat path.

Filtered entries simply do not appear in the wire-transmitted file
list. The wire framing - xflags byte (proto < 30) or xflags varint
(proto >= 30), name length, name bytes, size, mtime, mode, uid, gid -
is unchanged. Reference: upstream `flist.c:597-619` (legacy encoding)
and oc-rsync `crates/protocol/src/flist/write/metadata.rs:130-138` for
the byte format itself; both are documented in
`docs/audits/tcpdump-daemon-proto28-29.md` section 2.8.

oc-rsync applies the equivalent filter check during the walk in
`crates/transfer/src/generator/file_list/walk.rs:103-108`:

```rust
// upstream: flist.c:1332 - is_excluded() applied during make_file()
// FilterChain evaluates per-directory scoped rules (innermost first)
// then global rules. If no rules are configured, allows() returns true.
if !self.filter_chain.allows(&relative, metadata.is_dir()) {
    return Ok(());
}
```

The `filter_chain` here is the same chain populated by
`receive_filter_list_if_server()`, which prepended the daemon rules.
Because the daemon rules sit at the front of the chain and evaluation
is first-match-wins (see `docs/daemon/filter-precedence.md`), any path
matched by a daemon rule reaches its verdict before the client's
CLI-supplied rules are even consulted. This is the security-relevant
property: a client cannot un-exclude a daemon-excluded path.

The byte-level effect on the wire is solely "absent entry". No new byte
sequences, no flagging of which entries were filtered. A tcpdump-only
observer cannot distinguish "this entry was filtered" from "this entry
never existed in the source tree". Both implementations behave this way.

### Phase 10. Delta stream

Per-file delta blocks (`MSG_DATA` frames carrying `MATCH` / `DATA`
tokens) are emitted only for files that survived phase 9's filter. The
framing is unchanged. Reference: upstream `token.c:send_token()` and
oc-rsync `crates/protocol/src/wire/multiplex/`.

A regression that filtered the wrong files (e.g. failing to exclude a
daemon-excluded path) would show as an unexpected `MSG_DATA` frame with
the path in its payload during the file-name preamble. The tests that
guard this are in
`crates/daemon/src/daemon/sections/module_access/tests.rs`.

### Phase 11. Goodbye and stats

Stats counters (`total_size`, `total_transferred_size`, etc.) reflect
the actually-transferred file set, so they will differ between a
filtered and an unfiltered run. The framing is unchanged - the stats
are sent as a sequence of varints (proto >= 30) or 4-byte LE int64s
(proto < 30) per upstream `main.c:report()` and oc-rsync's
stats serialiser.

This is intentional: a tcpdump observer comparing two captures can see
the byte-count delta between filtered and unfiltered, but cannot tell
the *reason* for the delta from the wire alone.

---

## Divergence sites

None.

The audit traced every site where `daemon_filter_list` (upstream) or
`daemon_filter_rules` (oc-rsync) is read or evaluated:

| Site                                | Upstream                          | oc-rsync                                                                            | Wire impact |
|-------------------------------------|-----------------------------------|-------------------------------------------------------------------------------------|-------------|
| Construction at module bind         | `clientserver.c:874-893`          | `crates/daemon/src/daemon/sections/module_access/helpers.rs:223-280`                | None - process-local. |
| Storage on session config           | `daemon_filter_list` global       | `ServerConfig::daemon_filter_rules` (set at `module_access/transfer.rs:417`)        | None. |
| Filter-exchange merge (sender side) | `recv_filter_list()` in `exclude.c:1672` reads client list; daemon list consulted separately at use sites | `crates/transfer/src/generator/filters.rs:42-87` reads client list, prepends `daemon_filter_rules` to build the `FilterChain` | None - merge happens in process memory after the read. |
| Filter-exchange merge (receiver side) | Same `recv_filter_list()` plus consultation in `path_is_daemon_excluded()` | `crates/transfer/src/receiver/transfer.rs:856-901` - same pattern as the sender side | None. |
| Application during file-list build  | `flist.c:252-273` (`path_is_daemon_excluded`), `flist.c:1262` (generator), `exclude.c:1010-1015` (`name_is_excluded`) | `crates/transfer/src/generator/file_list/walk.rs:103-108` (`filter_chain.allows`), and the same `FilterChain` consulted by the receiver during deletion-candidate enumeration | Indirect: filtered entries are absent from the file list. Framing identical. |
| Application during deletion         | `generator.c:delete_in_dir()` -> `is_excluded()` | `crates/transfer/src/receiver` (deletion path consults `filter_chain`)              | Indirect: filtered paths are not deleted. |

Every read/evaluation site is server-local. No serialisation site
exists. The `write_filter_list()` function in
`crates/protocol/src/filters/wire.rs:217` and `send_filter_list()` /
`send_rules()` upstream (`exclude.c:1645-1669` and `1589-1642`) are
only called from the *client* `send_filter_list()` path, never from
the daemon-side rule loader.

### Anti-divergence guards

Even though no divergence exists today, the following code-level
properties are the load-bearing invariants. A regression in any of them
would surface as a wire-byte mismatch:

1. **`build_daemon_filter_rules()` returns a `Vec<FilterRuleWireFormat>`,
   never serialises it.** Callers in
   `module_access/transfer.rs:417` only assign to
   `config.daemon_filter_rules`. Greppable invariant: no
   `write_filter_list(.., daemon_filter_rules, ..)` callsite anywhere
   in the tree (verify with
   `Grep "write_filter_list" crates/`).

2. **Filter list reads on the daemon are unconditional reads, not
   reads-then-echo.** `read_filter_list()` returns a `Vec`; nothing
   downstream mirrors that vector back onto the writer. Verified in
   `crates/transfer/src/generator/filters.rs:62-87` (sender side) and
   `crates/transfer/src/receiver/transfer.rs:856-901` (receiver side).

3. **Daemon rules are prepended in memory, never via
   `serialize_rule()`.** The merge in `generator/filters.rs:67-75` and
   `receiver/transfer.rs:870-879` extends a `Vec`; it does not invoke
   the wire serialiser at
   `crates/protocol/src/filters/wire.rs:440-455`.

4. **For protocol < 29, daemon rules carrying modifier characters
   would fail to serialise.** This is irrelevant on the wire because
   the rules are never serialised, but it is relevant to the
   evaluation pipeline: `parse_daemon_filter_token()` at
   `crates/daemon/src/daemon/sections/module_access/helpers.rs:394-449`
   does not produce modifier-bearing rules in the first place. The
   `build_pattern_rule()` constructor at `helpers.rs:491-513` only
   sets `anchored` / `directory_only` flags from pattern shape, never
   `sender_side`, `receiver_side`, or `perishable`. So even if a
   downstream regression accidentally tried to serialise daemon rules,
   the protocol-< 29 `build_old_prefix()` at
   `crates/protocol/src/filters/prefix.rs:39-87` would accept them
   (or reject with `None` for unrepresentable rules, surfacing as an
   `io::ErrorKind::InvalidData` rather than a silent wire divergence).

5. **`should_send_filter_list` is false on the server side.** The
   guard at `crates/transfer/src/lib.rs:515-522` makes the daemon
   process never enter the `send_filter_list` branch:
   ```rust
   let should_send_filter_list = if config.connection.client_mode {
       match config.role {
           ServerRole::Generator => receiver_wants_filter_list,
           ServerRole::Receiver => true,
       }
   } else {
       false  // server -> never sends the filter list
   };
   ```
   This matches upstream `exclude.c:1650`
   (`if (local_server || (am_sender && !receiver_wants_list)) f_out = -1;`).

---

## What this audit doesn't cover

- **No live tcpdump output.** This is a code audit; the byte-level
  properties are derived from the upstream and oc-rsync source rather
  than from a packet capture. The reproduction recipe in
  `docs/audits/tcpdump-daemon-proto28-29.md` section 1 applies
  unchanged - swap in a daemon module with filter directives, capture
  to a pcap, and compare against an unfiltered baseline. The
  comparison should yield identical bytes in phases 1-8 and 11, plus a
  difference in phases 9-10 that consists entirely of
  filtered-entry-shaped omissions.

- **Client-supplied CLI filters.** The `--filter` / `--include` /
  `--exclude` / `--filter-from` / `--include-from` / `--exclude-from`
  flags on the client side are out of scope here. Their wire format is
  documented in `docs/audits/tcpdump-daemon-filter-pull.md` (PR
  c508a08a, the existing pull-direction audit) and their precedence
  relative to daemon-side rules is documented in
  `docs/daemon/filter-precedence.md` (PR #1888).

- **`.rsync-filter` / dir-merge files.** Per-directory merge files
  are loaded only when a `dir-merge` rule from sources #1 or #2 is
  active (see `docs/daemon/filter-precedence.md` source #3). Their
  wire-trace properties match the daemon rules - never serialised,
  applied locally during `walk_path()`. Audit:
  `docs/audits/rsync-filter-inheritance.md`.

- **Push direction.** The push flow is the structural mirror of pull.
  Daemon = receiver, client = sender. Daemon rules still never appear
  on the wire; they are consulted via the same `FilterChain` in the
  receiver's deletion-candidate enumeration and during the writer-side
  acceptance check. The push direction is covered by the `--delete`
  / write-only test cases in `crates/daemon/src/daemon/sections/module_access/tests.rs`.

- **Filter-rule parsing failures.** If `module.exclude_from`
  references a file that fails to read,
  `read_patterns_from_file()` returns an `io::Error` and
  `process_approved_module()` sends `@ERROR: failed to load module
  filter rules: ...` and exits before any transfer bytes flow
  (`crates/daemon/src/daemon/sections/module_access/transfer.rs:419-424`).
  The error path is covered by upstream's `XFLG_FATAL_ERRORS` flag at
  `clientserver.c:880` and is functionally identical, but the
  per-byte text of the `@ERROR` line differs (oc-rsync prefixes with
  `@ERROR: failed to load module filter rules:` whereas upstream's
  text is `@ERROR: failed to read filter file ...`). That message
  text is intentionally informational, not part of the protocol.

- **Daemon configs with `lp_filter()` returning a syntactically
  invalid string.** Upstream's `parse_filter_str()` `exit_cleanup()`s
  the daemon process at `exclude.c:1623-1628` if it cannot serialise
  a parsed rule. oc-rsync's `parse_daemon_filter_token()` returns
  `None` for unrecognised tokens (silently skipping, matching
  upstream's lenient *parsing*; the divergence is only at the
  serialise stage which never runs for daemon-side rules). Operator
  visibility of malformed config is via `oc-rsyncd --check-config`,
  not via the wire.

---

## References

### Upstream rsync 3.4.1

All paths are absolute under `target/interop/upstream-src/rsync-3.4.1/`.

- `clientserver.c:874-893` - `rsync_module()` builds
  `daemon_filter_list` from the module's filter directives.
- `exclude.c:49-52` - global declarations of `filter_list`,
  `cvs_filter_list`, `daemon_filter_list`, `implied_filter_list`.
- `exclude.c:1589-1642` - `send_rules()` per-rule serialisation
  (called only from `send_filter_list()` and the `local_server`
  trim path).
- `exclude.c:1645-1669` - `send_filter_list()`. The `am_sender &&
  !receiver_wants_list` guard at `:1650` makes the daemon-sender
  silent.
- `exclude.c:1672-1698` - `recv_filter_list()`. Reads
  `read_int(f_in)` length prefixes; never references
  `daemon_filter_list`.
- `exclude.c:1010-1015` - `name_is_excluded()` consults
  `daemon_filter_list` locally.
- `flist.c:252-273` - `path_is_daemon_excluded()` consults
  `daemon_filter_list` locally during file-list construction.
- `flist.c:1262` - generator-side daemon-filter consultation.
- `main.c:1252-1259` - daemon-sender wire path
  (`recv_filter_list(f_in)` then `do_server_sender`).
- `main.c:1304-1308` - client-sender wire path
  (`send_filter_list(f_out)`).

### oc-rsync

- `crates/daemon/src/daemon/sections/module_access/helpers.rs:223-280` -
  `build_daemon_filter_rules()`.
- `crates/daemon/src/daemon/sections/module_access/helpers.rs:394-449` -
  `parse_daemon_filter_token()`.
- `crates/daemon/src/daemon/sections/module_access/helpers.rs:491-513` -
  `build_pattern_rule()`.
- `crates/daemon/src/daemon/sections/module_access/transfer.rs:417-424` -
  daemon rules attached to `ServerConfig::daemon_filter_rules`.
- `crates/transfer/src/generator/filters.rs:42-87` - sender-side
  filter merge (read client rules, prepend daemon rules).
- `crates/transfer/src/generator/file_list/walk.rs:103-108` -
  filter chain consulted during file-list walk.
- `crates/transfer/src/receiver/transfer.rs:856-901` - receiver-side
  filter merge.
- `crates/transfer/src/receiver/mod.rs:447-450` -
  `should_read_filter_list()` (mirror of upstream's
  `receiver_wants_list`).
- `crates/transfer/src/lib.rs:509-522` - `should_send_filter_list`
  guard.
- `crates/protocol/src/filters/wire.rs:181-230` -
  `read_filter_list()` / `write_filter_list()`.
- `crates/protocol/src/filters/wire.rs:253-258` -
  `parse_wire_rule()` branch on `uses_old_prefixes()`.
- `crates/protocol/src/filters/prefix.rs:24-87` -
  `build_rule_prefix()` / `build_old_prefix()`.
- `crates/protocol/src/version/protocol_version/capabilities.rs` -
  protocol-version capability gates including `uses_old_prefixes()`.

### Related documentation

- `docs/audits/tcpdump-daemon-proto28-29.md` (PR #3513) - the
  proto-version sibling audit. Establishes that the entire daemon
  session is byte-equivalent to upstream at protocols 28 and 29.
- `docs/audits/tcpdump-daemon-filter-pull.md` (PR c508a08a) -
  earlier daemon-filter pull audit covering the modern protocol;
  this document is the deeper, server-side-rule-focused complement.
- `docs/daemon/filter-precedence.md` (PR #1888) - precedence of
  daemon-config rules vs client-CLI rules vs `.rsync-filter`.
- `docs/audits/rsync-filter-inheritance.md` - `dir-merge` /
  `.rsync-filter` inheritance semantics.
- `docs/filter-coverage-matrix.md` - test coverage matrix per
  filter rule type.
