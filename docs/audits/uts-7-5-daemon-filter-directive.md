# UTS-7.5 — daemon `filter` / `exclude` / `include` directive wire-up

Status: covered end-to-end as of this PR. The runtime injection of daemon-side
filter directives (`filter`, `exclude`, `include`, `exclude from`,
`include from`) into the transfer's filter chain is in place; this note
documents *where* the injection happens, *what order* the directives are
applied, and *why* daemon rules take precedence over client-sent filters.

## Injection point

| Stage | Location | Responsibility |
| --- | --- | --- |
| Parser | `crates/daemon/src/daemon/sections/config_parsing/module_directives.rs` | Stores `filter` / `exclude` / `include` lines on `ModuleDefinitionBuilder` as `Vec<String>`; `exclude from` / `include from` as resolved `PathBuf`. |
| Defaults | `crates/daemon/src/daemon/sections/module_definition/finish.rs` | Per-module directives override global defaults wholesale; an empty per-module list falls back to the global section's value. Mirrors upstream `loadparm.c` `P_LOCAL` semantics. |
| Compilation | `crates/daemon/src/daemon/sections/module_access/helpers.rs::build_daemon_filter_rules` | Converts the stored strings into `Vec<FilterRuleWireFormat>`. Handles word-split, keyword forms (`include`/`exclude`/`hide`/`show`/`protect`/`risk`/`clear`), and `XFLG_DIR2WILD3` (`dir/` → `dir/***`) for directory-only excludes. Reads pattern files line-by-line, skipping `#`/`;` comments and blank lines. |
| Wire-up | `crates/daemon/src/daemon/sections/module_access/transfer.rs:720` | Right after `build_server_config` succeeds and before any worker spawn, the compiled rules are written to `ServerConfig::daemon_filter_rules`. A pattern-file read failure aborts the session with `@ERROR: failed to load module filter rules: …`, matching upstream's `XFLG_FATAL_ERRORS` behaviour at `clientserver.c:881`. |
| Receiver consumption | `crates/transfer/src/receiver/transfer/setup.rs:74-86` (deletion chain) and `crates/transfer/src/receiver/mod.rs:299` (`daemon_filter_set`, consulted at `build_files_to_transfer` and `create_directories`). | Daemon rules are prepended to client-sent wire rules before the `FilterSet`/`FilterChain` is built. A separate `daemon_filter_set` is held on `ReceiverContext` so receiver-side checks at file-list build and directory creation time consult the daemon rules even when no client filters arrived. |
| Generator consumption | `crates/transfer/src/generator/filters.rs:64-86` | Same prepend-then-build pattern in server-mode `receive_filter_list_if_server`. |

The directive ordering inside the compiled rule list matches upstream
`clientserver.c:874-893` exactly:

```
1. filter        (parse_filter_str, FILTRULE_WORD_SPLIT)
2. include_from  (parse_filter_file, FILTRULE_INCLUDE | FILTRULE_FATAL_ERRORS)
3. include       (parse_filter_str, FILTRULE_INCLUDE | FILTRULE_WORD_SPLIT)
4. exclude_from  (parse_filter_file, no flags + FILTRULE_FATAL_ERRORS)
5. exclude       (parse_filter_str, FILTRULE_WORD_SPLIT)
```

Order is asserted by `build_daemon_filter_rules_ordering_filter_include_exclude_files`
in `crates/daemon/src/daemon/sections/module_access/tests.rs`.

## Precedence: daemon rules first

Daemon-config rules are prepended to client-sent rules, not appended. Both
sides of the transfer follow the same merge strategy:

- **Receiver** — `crates/transfer/src/receiver/transfer/setup.rs:76-85`:

  ```rust
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

- **Generator** (server-mode) — `crates/transfer/src/generator/filters.rs:66-75`:
  the same prepend pattern, behind a `client_mode` guard so client-side
  filter construction is unaffected.

Why prepend rather than append? In rsync's filter semantics the first
matching rule wins. Upstream maintains a separate `daemon_filter_list`
distinct from the client `filter_list` and consults it before any client
filter via `check_filter()` calls in `receiver.c:711`, `generator.c:1273`,
and the directory-listing path. Prepending into a single combined chain
delivers the same observable behaviour: a daemon `- *.log` evaluated first
will short-circuit a later client `+ *.log`, exactly as upstream's
`check_filter(&daemon_filter_list)` short-circuits before the client list
is consulted.

The `XFLG_DIR2WILD3` transformation (`dir/` → `dir/***` for exclude rules
only) is applied at compile time so the on-wire representation matches what
upstream emits when it would have set the `FILTRULE_DIRECTORY` flag, and
the include path keeps the trailing slash because upstream gates the
transformation on `!FILTRULE_INCLUDE` (`exclude.c:212`).

## Regression coverage

| Layer | Test | Asserts |
| --- | --- | --- |
| Parser | `crates/daemon/src/daemon/sections/config_parsing/tests.rs` | Storage of `filter`/`exclude`/`include`/`exclude from`/`include from` on the builder. |
| Builder finish | `crates/daemon/src/daemon/sections/module_definition/tests.rs` | Defaults fall through; per-module override replaces global. |
| Compilation | `crates/daemon/src/daemon/sections/module_access/tests.rs` (`build_daemon_filter_rules_*`) | 15 cases covering empty/exclude/include/filter syntax, word-split, file load, missing-file failure, directive order, anchored, dir2wild3 transform, dir-only include preservation, and keyword forms (`exclude *.bak`). |
| Receiver wire-up | `crates/transfer/src/receiver/tests/errors_and_timeouts/daemon_filter_tests.rs` | `daemon_filter_rules` from `ServerConfig` compiles into the receiver's `daemon_filter_set` and rejects matching paths. |
| End-to-end | `tests/integration_daemon_filter_directives.rs` (this PR) | Spawns a real in-process daemon, pushes mixed `.txt`/`.log` payload from `oc-rsync` subprocess, and asserts excluded files never land on disk under `filter = - *.log`, `exclude = *.log`, and `exclude from = <patterns-file>`. |

## Upstream citations

- `clientserver.c:rsync_module()` — daemon-filter list construction
  (3.4.4 source: `target/interop/upstream-src/rsync-3.4.4/clientserver.c:876-895`).
- `exclude.c::parse_filter_str` — `FILTRULE_WORD_SPLIT` semantics.
- `exclude.c::parse_filter_file` — file-loaded patterns honour `#`/`;`
  comments and blank lines.
- `exclude.c:211-217` — `XFLG_DIR2WILD3` transformation gate
  (`BITS_SETnUNSET(FILTRULE_DIRECTORY, FILTRULE_INCLUDE)`).
- `receiver.c:711-714` and `generator.c:1273-1275` — `check_filter()` call
  sites that consult `daemon_filter_list` before per-file actions.
