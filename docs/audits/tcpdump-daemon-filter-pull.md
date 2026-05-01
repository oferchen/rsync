# Audit: tcpdump-style wire comparison of daemon-side filter rules during a pull

Status: documentation only - no code changes.
Tracking issue: #1697.
Upstream reference: `target/interop/upstream-src/rsync-3.4.1/`.

**Last verified:** 2026-05-01 against `crates/protocol/src/filters/wire.rs`,
`crates/transfer/src/receiver/transfer.rs`,
`crates/daemon/src/daemon/sections/module_access/helpers.rs`,
`crates/transfer/src/receiver/mod.rs`, and the upstream sources cited
inline.

## Overview

When an operator connects a client to an `rsync://` daemon module that has
filter directives configured (`filter = ...`, `exclude = ...`,
`include = ...`, `exclude from = ...`, `include from = ...`), one might
expect those rules to traverse the wire so the client can be told what is
being filtered. Both upstream rsync 3.4.1 and oc-rsync deliberately do the
opposite: the daemon prepends those rules on its own receiver side and
they are NEVER transmitted. From a tcpdump perspective the wire is
indistinguishable from a daemon configured with no filter directives at
all.

This audit reconstructs that flow from both code bases, states the
wire-equivalence claim plainly, and gives the operator a hands-on
reproduction recipe so the property can be verified locally with
`tcpdump`.

The scope is the pull direction (`rsync rsync://host/mod/ ./`) where the
daemon is the sender and the client is the receiver. The push direction
follows the same pattern - daemon rules are never serialised - but the
roles are swapped and the relevant filter list is consulted on the daemon
sender side instead.

Cross references:

- `docs/daemon/filter-precedence.md` - precedence rules for daemon-config
  filters, client CLI filters, and per-directory `.rsync-filter` merges.
- `docs/filter-coverage-matrix.md` - test coverage matrix per filter rule
  type, including the daemon-side directives and pull/push variants
  exercised by the interop harness.
- `docs/audits/rsync-filter-inheritance.md` - inheritance and dir-merge
  semantics audit.

---

## 1. Upstream rsync 3.4.1 wire flow

All file references are absolute line numbers in
`target/interop/upstream-src/rsync-3.4.1/`.

### 1.1 Where daemon filter rules are loaded

When a client opens a session against a daemon module, the daemon process
runs `rsync_module()` in `clientserver.c:692`. After authentication,
`rsync_module()` builds a *separate* filter list called
`daemon_filter_list` from the module's configured directives at
`clientserver.c:874-893`:

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

Three properties of this code matter for the wire question:

1. The destination is `daemon_filter_list`, a global distinct from the
   ordinary `filter_list` that holds CLI rules.
2. The order is fixed: `filter`, `include from`, `include`, `exclude from`,
   `exclude`. This is the order in which patterns are evaluated, since
   downstream consumers walk the list head-to-tail.
3. The flags `XFLG_ABS_IF_SLASH` and `XFLG_DIR2WILD3` are set: a leading
   `/` anchors the pattern at the module root, and a directory pattern
   like `secrets/` is rewritten to `secrets/***` so an exclude covers all
   children.

### 1.2 Where the wire filter exchange happens

The wire-level filter exchange happens in `exclude.c`:

- Sender: `send_filter_list()` at `exclude.c:1645`. This is invoked by
  the client side of a pull (the client is sender of its own filters; the
  daemon is sender of file data but receiver of filters). For the daemon
  module case during a pull, the daemon's process - acting as server -
  reaches `recv_filter_list()` at `exclude.c:1672`.
- The serialiser is `send_rules()` at `exclude.c:1589`. It iterates
  `flp->head` and emits one rule at a time:

  ```c
  write_int(f_out, plen + len + dlen);     // 4-byte length prefix
  if (plen) write_buf(f_out, p, plen);     // prefix bytes ("- ", "+ ", ...)
  write_buf(f_out, ent->pattern, len);     // pattern bytes
  if (dlen) write_byte(f_out, '/');        // directory trailer
  ```

  Followed by a final terminator at `exclude.c:1661`:

  ```c
  if (f_out >= 0) write_int(f_out, 0);
  ```

The list passed to `send_rules()` at `exclude.c:1658` is `&filter_list`,
not `&daemon_filter_list`:

```c
send_rules(f_out, &filter_list);
```

This is the load-bearing line. `daemon_filter_list` is **never** passed
to `send_rules()` anywhere in the upstream tree. A grep for the symbol
in the 3.4.1 source confirms it is consulted by `name_is_excluded()` and
the deletion path, never by any IO routine that writes to a socket.

### 1.3 Where daemon rules are evaluated

After the wire exchange, the daemon-side receiver of the file list
consults `daemon_filter_list` directly through `name_is_excluded()` (in
`exclude.c`) before honouring the file. The client never observes that
list - only the post-filter file set, which is indistinguishable from a
file set produced by a server that genuinely lacked those files.

### 1.4 Concrete example for `filter = - *.tmp + *.rs - *`

For a module with `filter = - *.tmp + *.rs - *`, `parse_filter_str()`
with `FILTRULE_WORD_SPLIT` produces three entries on `daemon_filter_list`:

1. `- *.tmp`
2. `+ *.rs`
3. `- *`

Each entry has its byte-image computed lazily by `get_rule_prefix()` if
ever needed for transmission. But because the only call site that would
serialise them is `send_rules()` invoked on `&filter_list`, the bytes
never reach the socket. On the wire during the filter-exchange phase the
client sees only its own `--filter` / `--include` / `--exclude`
arguments, terminated by the 4-byte zero. With no client-side filter
arguments, the entire filter-exchange phase is exactly four bytes:
`00 00 00 00`.

---

## 2. oc-rsync wire flow

All line numbers are as they exist on master at the time of this audit
(2026-05-01).

### 2.1 Where daemon filter rules are loaded

`build_daemon_filter_rules()` at
`crates/daemon/src/daemon/sections/module_access/helpers.rs:223` mirrors
the upstream order exactly:

1. `filter` (with `FILTRULE_WORD_SPLIT` semantics, line 233)
2. `include from` (one pattern per line from a file, line 244)
3. `include` (bare patterns, word-split, line 254)
4. `exclude from` (one pattern per line from a file, line 263)
5. `exclude` (bare patterns, word-split, line 273)

The function returns a `Vec<FilterRuleWireFormat>` that is stored on the
receiver as `config.daemon_filter_rules`. `FilterRuleWireFormat` is the
in-memory representation defined at
`crates/protocol/src/filters/wire.rs:64` and it is the same type used by
the wire encoder, but storing rules in this shape is purely an
implementation choice - the rules in this `Vec` are never serialised.

### 2.2 Where the wire filter exchange happens

The receiver's filter-exchange code is in
`crates/transfer/src/receiver/transfer.rs:856-879`:

```rust
if self.should_read_filter_list() {
    let wire_rules = read_filter_list(&mut reader, self.protocol).map_err(|e| { ... })?;

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
    // ... build a FilterChain and store it for use during deletion ...
}
```

The gating predicate `should_read_filter_list()` at
`crates/transfer/src/receiver/mod.rs:447` matches upstream's
`recv_filter_list()` precondition exactly: a daemon receiver only reads
the list when `delete` or `prune_empty_dirs` is set; a client receiver
skips reading entirely.

The wire decoder is `read_filter_list()` at
`crates/protocol/src/filters/wire.rs:181`. It reads a sequence of
4-byte little-endian length prefixes, each followed by that many rule
bytes, terminated by a length of 0. The corresponding encoder is
`write_filter_list()` at `crates/protocol/src/filters/wire.rs:217`.

### 2.3 The daemon rules never enter the encoder

The complete set of call sites that write filter rules to the wire is
the call to `write_filter_list()` from the sender side (the client, in a
pull). `daemon_filter_rules` are read from `config.daemon_filter_rules`
in `transfer.rs:870` and merged with `wire_rules` only **after**
`read_filter_list` has returned, into a local `combined` variable that
is then handed to `parse_wire_filters_for_receiver` for matcher
construction (`transfer.rs:884`). The `combined` value is never written
back to a socket. There is no code path in oc-rsync that calls
`write_filter_list` on `daemon_filter_rules`.

### 2.4 Concrete example for `filter = - *.tmp + *.rs - *`

`build_daemon_filter_rules()` produces a `Vec<FilterRuleWireFormat>` of
length 3, in the same order as upstream:

1. `FilterRuleWireFormat { rule_type: Exclude, pattern: "*.tmp", .. }`
2. `FilterRuleWireFormat { rule_type: Include, pattern: "*.rs", .. }`
3. `FilterRuleWireFormat { rule_type: Exclude, pattern: "*", .. }`

These three values live entirely in the daemon process's heap. When the
client connects and the receiver reaches `should_read_filter_list()`, the
client's `wire_rules` are read from the socket; the daemon rules are
prepended in memory; the resulting `FilterChain` consults the prepended
rules first when matching paths during the file-list and deletion phases.
Nothing in this flow writes those three entries to a socket.

---

## 3. Wire-equivalence claim

> In both upstream rsync 3.4.1 and oc-rsync, daemon-side filter rules
> (`filter`, `exclude`, `include`, `exclude from`, `include from`
> directives in `oc-rsyncd.conf` / `rsyncd.conf`) are NEVER transmitted
> across the wire. Only the client's CLI filter rules cross the wire
> during the filter-exchange phase. A client cannot observe a daemon's
> filter list via tcpdump.

This is by design and is a security-relevant property: it allows
operators to use daemon filters to mandate exclusions that clients cannot
discover, override, or bypass. Adding a daemon `exclude` does not
require a protocol bump or capability flag, since the wire format is
unchanged.

The audit found no deviation between the two implementations. The list
holding daemon rules is distinct from the list serialised onto the
socket in both code bases, and the only call sites that walk the
serialised list use the wrong list (the client's `filter_list` /
`wire_rules`) for any tcpdump-observable transmission.

---

## 4. Reproduction recipe

The following recipe verifies the property hands-on with a single host
running the daemon on `localhost:8730` and an upstream rsync client
issuing a pull.

### 4.1 Daemon configuration

Create a temporary directory layout and `oc-rsyncd.conf`:

```sh
mkdir -p /tmp/audit-1697/{srv,run}
printf 'visible.rs\nsecret.tmp\n' > /tmp/audit-1697/srv/listing
echo 'visible' > /tmp/audit-1697/srv/visible.rs
echo 'secret'  > /tmp/audit-1697/srv/secret.tmp

cat > /tmp/audit-1697/oc-rsyncd.conf <<'EOF'
port = 8730
pid file = /tmp/audit-1697/run/oc-rsyncd.pid
use chroot = no

[mod]
    path = /tmp/audit-1697/srv
    read only = true
    filter = - *.tmp + *.rs - *
EOF
```

### 4.2 Start the daemon

```sh
oc-rsync --daemon --no-detach --config=/tmp/audit-1697/oc-rsyncd.conf
```

(or substitute the upstream `rsync --daemon --no-detach --config=...`
binary with the same config to compare implementations).

### 4.3 Capture the filter-exchange phase

In a second shell, start `tcpdump`. Match the daemon's TCP port (rsync
defaults to 873; we are using 8730 in this recipe to avoid root):

```sh
sudo tcpdump -i lo0 -nn -X -s 0 'tcp port 8730' -w /tmp/audit-1697/cap.pcap
```

`tcp port 873` is the canonical filter expression for the default rsync
daemon port; substitute the port your daemon is listening on. On Linux
the loopback interface is `lo`; on macOS use `lo0`.

### 4.4 Run the client

In a third shell, run the upstream rsync client against the module:

```sh
rsync -av rsync://localhost:8730/mod/ /tmp/audit-1697/dest/
```

Stop tcpdump (`Ctrl-C`) once the transfer completes.

### 4.5 Decode the filter-exchange bytes

Open `/tmp/audit-1697/cap.pcap` in Wireshark or replay it through
`tcpdump -r`. The handshake bytes (the `@RSYNCD: 32` greeting and
authentication exchange) appear first; after the module is selected and
the protocol exchange completes, look for the filter-exchange phase.

For a client invocation that passes no `--filter` / `--include` /
`--exclude` arguments, the entire filter-exchange phase is exactly four
bytes:

```
00 00 00 00
```

That is `write_int(0)` per upstream `exclude.c:1661`, and
`write_filter_list(writer, &[], protocol)` in oc-rsync emits the same
four bytes (verified by the unit test `empty_filter_list_roundtrip` in
`crates/protocol/src/filters/wire.rs`).

If the client *does* pass filter arguments, each rule is encoded as a
4-byte little-endian length followed by the rule bytes; the sequence
ends with the `00 00 00 00` terminator. For example,
`--exclude='*.log'` (a 7-byte rule serialised as `- *.log`) produces:

```
07 00 00 00          # length: 7 bytes
2d 20 2a 2e 6c 6f 67 # "- *.log"
00 00 00 00          # terminator
```

The daemon's configured `filter = - *.tmp + *.rs - *` rules do **not**
appear in the capture. The `*.tmp` file is omitted from the file list
that the daemon constructs, so the client does not request it; but no
wire byte ever announces the rule that caused the omission. The client's
view of the module is indistinguishable from a module that simply does
not contain `secret.tmp`.

### 4.6 Repeat against upstream rsync

Rerun the same `tcpdump` capture with the upstream rsync 3.4.1 daemon
configured identically. The wire trace is byte-identical: the
filter-exchange phase contains only the client's rules followed by the
4-byte zero terminator, and the daemon's `filter` directive contributes
no observable bytes.

---

## 5. Documentation discrepancy noted (out of scope)

While reading the surrounding tooling for this audit, the following
discrepancy was observed in `tools/ci/known_failures.conf:81`:

> ```
> # versions (e.g., rsync 3.0.9 speaks protocol 28, rsync 3.1.3 speaks 30+).
> ```

Upstream rsync 3.0.9 actually advertises `PROTOCOL_VERSION = 30` (with a
minimum-accepted version of 28), not 28. The 3.4.1 source for reference
defines `PROTOCOL_VERSION 32` and `MIN_PROTOCOL_VERSION 20` at
`target/interop/upstream-src/rsync-3.4.1/rsync.h:114,147`; the 3.0.9
header has the analogous macros set to 30 and 28 respectively. The
comment in `known_failures.conf` understates the version 3.0.9 actually
speaks, though the surrounding logic - which gates known-failure cases
on `forced_proto <= 29` - is correct because it is keyed off the forced
protocol value, not the advertised version.

This audit notes the discrepancy for future cleanup but does not attempt
to fix it. The intended scope here is wire behaviour for daemon filter
rules; the comment correction belongs in a separate documentation
follow-up.

---

## 6. Summary

| Question | Answer |
|----------|--------|
| Are daemon-side filter rules transmitted on the wire during a pull? | No, in both upstream rsync 3.4.1 and oc-rsync. |
| Where are they applied instead? | On the daemon's receiver side, before evaluating the file list and before delete decisions. |
| Can a client observe them via tcpdump? | No. The wire trace is identical to a daemon with no filter directives configured. |
| Is the wire format unchanged when daemon filters are added? | Yes. Adding a daemon `exclude` requires no protocol bump and no capability flag. |
| What does the empty filter-exchange phase look like? | Exactly four bytes: `00 00 00 00` (the `write_int(0)` terminator). |
| Where is this enforced in upstream? | `exclude.c:1645-1668` (`send_filter_list`) walks `&filter_list`, never `&daemon_filter_list`. |
| Where is this enforced in oc-rsync? | `crates/transfer/src/receiver/transfer.rs:856-879` reads only `wire_rules` from the socket; `daemon_filter_rules` are merged in memory only. |
