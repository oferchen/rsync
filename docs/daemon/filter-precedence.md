# Daemon Filter Precedence

Filter rules in oc-rsync's daemon mode come from three sources. Rules from
each source are concatenated into a single ordered list and then evaluated
first-match-wins by `FilterChain`. Because daemon-config rules sit at the
front of that list, they cannot be overridden by anything the client sends.

This document describes the order, the wire mechanics, and the resulting
operator-visible behavior. It is wire-equivalent to upstream rsync 3.4.1.

**Last verified:** 2026-05-01 against `crates/daemon/src/daemon/sections/module_access/helpers.rs`,
`crates/transfer/src/receiver/transfer.rs`, and
`crates/protocol/src/filters/wire.rs`.

---

## Sources, in evaluation order

| # | Source                              | Configured in              | Per-request? | Can override #1? |
|---|-------------------------------------|----------------------------|--------------|------------------|
| 1 | Daemon-module filters               | `oc-rsyncd.conf` (module)  | No (server)  | -                |
| 2 | Client CLI filters                  | `--filter` / `--include` / `--exclude` / `--include-from` / `--exclude-from` | Yes | No |
| 3 | Per-directory `.rsync-filter` merge | Loaded only when a `dir-merge` rule from #1 or #2 references it | Yes (during walk) | No |

The receiver builds the evaluated list as `[#1, #2]` and hands it to
`FilterChain`. Source #3 is layered on at directory-entry time by
`FilterChain` itself, but only when a `dir-merge` (`:`) rule is already
present in the chain.

---

## Source 1: Daemon module filters

Set in the module section of `oc-rsyncd.conf`:

```ini
[backups]
    path = /srv/backups
    read only = false
    # Mandatory rules - the client cannot override these.
    filter = - *.tmp + *.rs - *
    include = src/ tests/
    exclude = .git/ target/
    include from = /etc/oc-rsyncd/include.txt
    exclude from = /etc/oc-rsyncd/exclude.txt
```

`build_daemon_filter_rules()` in
[`crates/daemon/src/daemon/sections/module_access/helpers.rs:223`](../../crates/daemon/src/daemon/sections/module_access/helpers.rs)
parses these into a `Vec<FilterRuleWireFormat>` in the upstream-mandated order:

1. `filter`        - full filter syntax, word-split
2. `include from`  - one pattern per line from file (treated as `include`)
3. `include`       - bare patterns, word-split
4. `exclude from`  - one pattern per line from file (treated as `exclude`)
5. `exclude`       - bare patterns, word-split

This mirrors `clientserver.c:874-893` (`rsync_module()`) in upstream rsync 3.4.1.
`filter` lines support `FILTRULE_WORD_SPLIT`, so a single line can carry
multiple rules (`+ *.txt + *.rs - *` is three rules).

The result is stored on the receiver as `config.daemon_filter_rules` and is
**always evaluated before any client-supplied rule**.

---

## Source 2: Client CLI filters

The client sends its `--filter` / `--include` / `--exclude` /
`--include-from` / `--exclude-from` rules over the wire as a single filter
list during the filter-exchange phase of the protocol.

Wire format (see
[`crates/protocol/src/filters/wire.rs:181`](../../crates/protocol/src/filters/wire.rs)):

- One rule at a time: a 4-byte little-endian length prefix followed by the
  rule bytes (NOT a varint - upstream uses `read_int()` / `write_int()`,
  matching `exclude.c:1658`).
- A 4-byte zero terminates the list.
- For protocol < 29, only old-style prefixes (`+ `, `- `, `!`) are accepted;
  modifier characters are rejected.

The receiver reads these via `read_filter_list()` into `wire_rules`.

---

## Source 3: `.rsync-filter` merge files

Per-directory `.rsync-filter` files are loaded only when a `dir-merge` (`:`)
rule from sources #1 or #2 instructs the engine to do so. Without such a
rule, `.rsync-filter` files on disk are ignored.

When a `dir-merge` rule is present, `FilterChain::enter_directory()` reads
the file as the walk descends, applies its rules under the configured
modifiers (`n` no-inherit, `e` exclude-self, `+`/`-` default action), and
pops them as the walk ascends. This is layered on top of sources #1 and #2
- it cannot remove or shadow them, only add to them in narrower scopes.

---

## How they combine

The receiver builds the combined list verbatim (no merging, no dedup):

```rust
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
```

[`crates/transfer/src/receiver/transfer.rs:868-879`](../../crates/transfer/src/receiver/transfer.rs)

Evaluation is first-match-wins against this ordered list. Because daemon
rules are prepended, any path matched by a daemon rule reaches its verdict
before the client's rules are consulted.

### Worked example

Module config:

```ini
[mod]
    exclude = secrets/
```

Client invocation:

```sh
rsync -av --include='secrets/' --include='secrets/api.key' rsync://host/mod/ ./
```

Combined chain (daemon first):

```
- secrets/        # from module config
+ secrets/        # from client --include
+ secrets/api.key # from client --include
```

`secrets/` matches the daemon `- secrets/` rule before the client's
`+ secrets/` is consulted. The directory is excluded; the client cannot
override it. This is the security-relevant property of daemon filters.

---

## Wire format note

Daemon-side rules are not sent over the wire. The client sends only its own
rules (source #2), and the daemon prepends its module-local rules
(source #1) on the receiver side. This means:

- The client cannot see the daemon's filter list.
- The daemon's filter list is not auditable from the client side.
- Adding a daemon `exclude` does not require a protocol bump or capability
  flag.

The wire payload uses `write_int()` / `read_int()` (4-byte little-endian
integer length, zero terminator). See
[`crates/protocol/src/filters/wire.rs:217`](../../crates/protocol/src/filters/wire.rs)
and upstream `exclude.c:1658` (`send_filter_list()`).

For protocol < 29 the daemon strips modifier characters when serializing
rules originating from source #1, since the client cannot parse them
(`XFLG_OLD_PREFIXES`).

---

## Common pitfalls

- **Operators expect `--include` to override daemon `exclude`.** It does
  not. The daemon's rule is evaluated first. Use a daemon-side `+` rule
  in `filter` if you need to allow a path that a daemon `exclude` would
  reject.
- **`.rsync-filter` only takes effect when a `dir-merge` rule activates it.**
  Dropping a `.rsync-filter` into a module path with no corresponding
  `dir-merge` rule in the daemon or client config is a no-op.
- **`exclude from` and `include from` are paths, not patterns.** They name
  files on the server's filesystem from which patterns are read, one per
  line. Empty lines and `#` / `;` comments are skipped.
- **Order within source #1 is fixed.** `filter` rules always take effect
  before `include`/`exclude` lines. To make an `exclude` win over a
  `filter +`, encode the negation directly in `filter`.
- **`!` (clear) does not span sources.** A client-sent `!` clears only the
  scope it was added to; it cannot clear daemon-config rules.

---

## See also

- `docs/filter-coverage-matrix.md` - test coverage per filter rule type.
- `docs/audits/rsync-filter-inheritance.md` - inheritance / `dir-merge`
  semantics audit (#2050).
- Upstream: `clientserver.c:rsync_module()`, `exclude.c:send_filter_list()`,
  `exclude.c:parse_filter_str()` in
  `target/interop/upstream-src/rsync-3.4.1/`.
