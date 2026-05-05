# Audit: `--iconv` parse path - where `IconvSetting` lands and stops

Tracking: oc-rsync task #1909.

This audit is **complementary** to
[`iconv-inert.md`](./iconv-inert.md). That document maps the entire iconv
stack (file-list reader/writer hooks, wire encode/decode, capability
negotiation, daemon plumbing, filter matching) and identifies a single
critical bridge gap. This document narrows the focus to the **CLI parse
path itself**: the linear chain of symbols `--iconv=LOCAL,REMOTE` walks
from `clap` argument matching to the final byte of `ClientConfig` it
mutates, and the precise call site at which the value loses all
downstream visibility on the local-copy path.

The companion audits cover what `--iconv` *should* do once the value
arrives. This one covers the producer side: how the value arrives, where
it sits, and where it stops being read.

## Scope

In scope:

- The argument-parser entry that creates `Option<OsString>` from
  `--iconv=...`.
- The translator that turns that string into `IconvSetting`.
- The struct fields that store it and the builders that propagate it.
- The exact final call site that reads `ClientConfig.iconv` (or its
  derivative) on each transfer path.
- The four wire-up points (per #1910-#1914) where the resolved
  converter would need to land but currently does not.

Out of scope (covered elsewhere):

- File-list reader/writer behaviour with a converter present
  (`iconv-pipeline.md`, Findings 1-3).
- Capability advertisement gating
  (`iconv-pipeline.md`, Finding 5; `iconv-feature-design.md`).
- Daemon module `iconv = <charset>` directive
  (`iconv-inert.md`, Required Components item 4).
- Library choice and feature gating (`iconv-feature-design.md`).

## Companion audit summary

`iconv-inert.md` was written before PR #3458 wired the
`IconvSetting -> FilenameConverter` bridge for SSH/daemon transfers. It
documents the (then-complete) dead-end: every layer compiled, every
hook was present, but nothing produced a `FilenameConverter`. PR #3458
landed the bridge function (`IconvSetting::resolve_converter`) and one
production caller (`apply_common_server_flags`) covering the SSH and
daemon paths. The local-copy path and the filter-rule path were not
included. Read `iconv-inert.md` for the full pipeline gap inventory and
the per-task remediation table; read this audit for the narrow producer
trace and the residual dead-ends.

`iconv-pipeline.md` is the upstream-callsite gap inventory (every
`iconvbufs` call in `flist.c`, `io.c`, `rsync.c`, `log.c`, `compat.c`
mapped to its oc-rsync counterpart). `iconv-feature-design.md` covers
option semantics, library choice (`encoding_rs` vs system iconv), and
feature gating.

## Parse path trace

Each step below is the only production hop between its predecessor and
successor on the parse path. Test-only call sites are omitted.

### Step 1: clap argument definition

- `crates/cli/src/frontend/command_builder/sections/connection_and_logging_options.rs:142-152` -
  `Arg::new("iconv").long("iconv").value_name("CONVERT_SPEC")`,
  `num_args(1)`, `OsStringValueParser`, `conflicts_with("no-iconv")`.
- `crates/cli/src/frontend/command_builder/sections/connection_and_logging_options.rs:154-159` -
  `Arg::new("no-iconv")`, `ArgAction::SetTrue`,
  `conflicts_with("iconv")`.

### Step 2: clap match extraction

- `crates/cli/src/frontend/arguments/parser/mod.rs:318` -
  `let iconv = matches.remove_one::<OsString>("iconv");`
- `crates/cli/src/frontend/arguments/parser/mod.rs:319` -
  `let no_iconv = matches.get_flag("no-iconv");`

### Step 3: typed `ParsedArgs` storage

- `crates/cli/src/frontend/arguments/parsed_args/mod.rs:454` -
  `pub iconv: Option<OsString>`.
- `crates/cli/src/frontend/arguments/parsed_args/mod.rs:457` -
  `pub no_iconv: bool`.
- Populated at `crates/cli/src/frontend/arguments/parser/mod.rs:674,775`.

### Step 4: workflow destructure

- `crates/cli/src/frontend/execution/drive/workflow/run.rs:102` -
  destructures `iconv` from `ParsedArgs`.
- `crates/cli/src/frontend/execution/drive/workflow/run.rs:203` -
  destructures `no_iconv` from `ParsedArgs`.

### Step 5: parse to `IconvSetting`

- `crates/cli/src/frontend/execution/drive/workflow/run.rs:267` -
  `let iconv_setting = match resolve_iconv_setting(iconv.as_deref(), no_iconv) { ... };`
- `crates/cli/src/frontend/execution/options/iconv.rs:29` -
  `pub(crate) fn resolve_iconv_setting(spec, disable) -> Result<IconvSetting, Message>`.
- `crates/core/src/client/config/iconv.rs:25` -
  `IconvSetting::parse(spec)` validates the `LOCAL,REMOTE` form,
  recognises `.` (`LocaleDefault`) and `-` (`Disabled`), rejects empty
  / missing-half input.
- `crates/cli/src/frontend/execution/options/iconv.rs:60-74` -
  `accept_parsed_setting` rejects an explicit setting at parse time when
  the `iconv` Cargo feature is off (closes #1915). For the rest of the
  parse path the value is feature-independent.

### Step 6: stage on `ConfigInputs`

- `crates/cli/src/frontend/execution/drive/workflow/run.rs:753` -
  `iconv: iconv_setting,` is the only producer site for this field.
- `crates/cli/src/frontend/execution/drive/config.rs:132` -
  `pub(crate) iconv: IconvSetting`.

### Step 7: propagate to `ClientConfigBuilder`

- `crates/cli/src/frontend/execution/drive/config.rs:253` -
  `.iconv(inputs.iconv.clone())` invokes
  `ClientConfigBuilder::iconv`. This is the only production call site
  of the setter outside the `core` test suite.
- `crates/core/src/client/config/builder/network.rs:93` -
  `ClientConfigBuilder::iconv(setting: IconvSetting) -> Self`.
- `crates/core/src/client/config/builder/mod.rs:240` -
  `iconv: IconvSetting` field on the builder.

### Step 8: land on `ClientConfig`

- `crates/core/src/client/config/client/mod.rs:169` -
  `pub(super) iconv: IconvSetting` on `ClientConfig`.
- `crates/core/src/client/config/client/mod.rs:323` - default
  `iconv: IconvSetting::Unspecified`.
- `crates/core/src/client/config/client/mod.rs:355` -
  `pub const fn iconv(&self) -> &IconvSetting` accessor.

This is the terminus of the parse path. Everything above is parsing
and propagation; everything below is consumption.

## Consumption sites and the dead-end

`config.iconv()` has two production reads. One forwards the setting to
the remote peer. One bridges it to a `FilenameConverter` for SSH and
daemon transfers. There is no third read.

### Consumer 1 (out-of-process): re-emit on the wire

- `crates/core/src/client/config/iconv.rs:78` -
  `IconvSetting::cli_value(&self) -> Option<String>` re-renders the
  setting back into `--iconv=...` for the remote argv. This is what
  the SSH / daemon side picks up and applies to *its* file lists. The
  local process does not touch the value beyond re-rendering.

### Consumer 2 (in-process, partial): SSH and daemon `ServerConfig`

- `crates/core/src/client/remote/flags.rs:228` (PR #3458) -
  `server_config.connection.iconv = config.iconv().resolve_converter();`
  inside `apply_common_server_flags`. This is the only in-process call
  site that reads `ClientConfig.iconv` and translates it to a
  `FilenameConverter`.
- `crates/core/src/client/config/iconv.rs:130` -
  `IconvSetting::resolve_converter(&self) -> Option<FilenameConverter>`
  performs the translation: `Unspecified | Disabled -> None`,
  `LocaleDefault -> Some(converter_from_locale())`,
  `Explicit { local, remote } -> FilenameConverter::new(local, remote)`
  with a `tracing::warn!` fallback to `None` for unsupported labels.

`apply_common_server_flags` is invoked by every SSH and daemon entry
point:

- `crates/core/src/client/remote/ssh_transfer.rs:665,687` (SSH receiver
  and sender).
- `crates/core/src/client/remote/embedded_ssh_transfer.rs:436,456`
  (embedded SSH receiver and sender).
- `crates/core/src/client/remote/daemon_transfer/orchestration/server_config.rs:32,75`
  (daemon receiver and sender).

The resulting `Option<FilenameConverter>` is consumed by
`crates/transfer/src/receiver/mod.rs:369` (file-list reader) and
`crates/transfer/src/generator/mod.rs:564` (file-list writer) via
`with_iconv(converter.clone())`.

### The dead-end: local-copy path

The local-copy path **never reads** `config.iconv()`. The transfer of
control on this path is:

- `crates/core/src/client/run/mod.rs:248` - `LocalCopyPlan::from_operands(config.transfer_args())`.
- `crates/core/src/client/run/mod.rs:274` - `let filter_program = filters::compile_filter_program(config.filter_rules())?;`
  (no iconv parameter).
- `crates/core/src/client/run/mod.rs:275` - `let mut options = build_local_copy_options(&config, filter_program);`.
- `crates/core/src/client/run/mod.rs:359` - `LocalCopyOptionsBuilder::build`
  composes `LocalCopyOptions` from `&ClientConfig` field by field.
  Recursion, deletion, limits, bandwidth, compression, metadata,
  behavioural flags, paths, time, reference dirs, and filter program
  are all read; `iconv` is not.

**Final `IconvSetting`-aware site on the local-copy path:**
`crates/core/src/client/run/mod.rs:275`. After
`build_local_copy_options(&config, filter_program)` returns, `&config`
is no longer threaded into the engine, and the engine has no field for
the converter to land on. Repo-wide grep across `crates/engine/` for
`iconv|FilenameConverter|EncodingConverter` returns zero hits, and
`LocalCopyOptions` exposes no setter for a converter. The parse path
dead-ends here for every local copy.

The same gap applies to filter-rule path matching even on the SSH and
daemon paths: `compile_filter_program` (`run/mod.rs:274` for
local-copy; analogous sites elsewhere) and `FilterChain::new`
(`crates/transfer/src/receiver/transfer.rs:895`,
`crates/transfer/src/generator/filters.rs:53,80`) take only filter
specs, never a converter, so the filter chain matches whatever bytes
the engine hands it. Repo-wide grep across `crates/filters/` for
`iconv|FilenameConverter|EncodingConverter` returns zero hits.

## Four wire-up points

Per the open issue chain #1910-#1914, the resolved converter would need
to reach four independent destinations. Two now have a producer (via
`apply_common_server_flags`); two remain dead.

### (a) `FilenameConverter` trait / impl - #1910

The type and constructors already exist; this issue is now a confirm
and document task.

- `crates/protocol/src/iconv/mod.rs:33` - `FilenameConverter`,
  `EncodingConverter`, `EncodingPair`, `converter_from_locale`
  re-exports. Constructors: `FilenameConverter::new(local, remote)`,
  `FilenameConverter::identity()`, `FilenameConverter::new_lenient()`.
- Bridge: `crates/core/src/client/config/iconv.rs:130` -
  `IconvSetting::resolve_converter` (PR #3458).
- Status: present, exercised by tests in
  `crates/core/src/client/remote/daemon_transfer/orchestration/tests.rs:694-770`.

### (b) Sender file-list emit - #1912

- Hook: `crates/protocol/src/flist/write/encoding.rs:303` -
  `apply_encoding_conversion()` runs `FilenameConverter::local_to_remote`
  when the writer has a converter set.
- Producer site: `crates/transfer/src/generator/mod.rs:564` -
  `if let Some(ref converter) = self.config.connection.iconv { writer = writer.with_iconv(converter.clone()); }`.
- Status: wired for SSH and daemon (via `apply_common_server_flags`).
  Local-copy uses the engine, not the generator, so this hook is not
  reached on the local path (`engine` has no equivalent).
- See `iconv-pipeline.md` Findings 1 and 2 for the symlink-target
  sub-gap and the missing `sender_symlink_iconv` gate.

### (c) Receiver file-list ingest - #1913

- Hook: `crates/protocol/src/flist/read/name.rs:101` -
  `apply_encoding_conversion()` runs `FilenameConverter::remote_to_local`
  when the reader has a converter set.
- Producer site: `crates/transfer/src/receiver/mod.rs:369` -
  `if let Some(ref converter) = self.config.connection.iconv { reader = reader.with_iconv(converter.clone()); }`.
- Status: wired for SSH and daemon (via `apply_common_server_flags`).
  Local-copy bypasses this code path entirely.

### (d) Filter-rule path matching - #1914

- Required wire-up: `crates/transfer/src/generator/filters.rs:53,80`
  (`FilterChain::new(filter_set)`) and
  `crates/transfer/src/receiver/transfer.rs:895`
  (`FilterChain::new(filter_set)`); also
  `crates/core/src/client/run/filters.rs::compile_filter_program`
  (called from `crates/core/src/client/run/mod.rs:274` for the
  local-copy path) and `crates/filters/src/chain.rs:204`
  (`FilterChain::new`).
- Status: **dead.** Repo-wide grep across `crates/filters/` for
  `iconv|FilenameConverter|EncodingConverter` returns zero hits. The
  chain matches against raw remote-side bytes, so user-supplied
  include/exclude patterns silently mismatch transcoded names whenever
  iconv is active.
- This is the same gap noted in `iconv-inert.md` "Filter-side: zero
  awareness" and in `iconv-pipeline.md` Finding 4 (filter consumer side).

## Summary table

| Stage | Site | Reads | State |
|---|---|---|---|
| 1. clap arg | `command_builder/sections/connection_and_logging_options.rs:142,154` | argv | active |
| 2. clap match | `arguments/parser/mod.rs:318-319` | clap matches | active |
| 3. `ParsedArgs` | `arguments/parsed_args/mod.rs:454,457` | typed value | active |
| 4. workflow destructure | `execution/drive/workflow/run.rs:102,203` | `ParsedArgs` | active |
| 5. `IconvSetting` parse | `execution/drive/workflow/run.rs:267` -> `execution/options/iconv.rs:29` -> `core/.../iconv.rs:25` | `Option<OsString>` | active |
| 6. `ConfigInputs` | `execution/drive/workflow/run.rs:753` -> `execution/drive/config.rs:132` | `IconvSetting` | active |
| 7. `ClientConfigBuilder` | `execution/drive/config.rs:253` -> `core/.../builder/network.rs:93` -> `builder/mod.rs:240` | `IconvSetting` | active |
| 8. `ClientConfig` | `core/.../config/client/mod.rs:169,355` | `IconvSetting` | active |
| 9a. wire re-emit | `core/.../config/iconv.rs:78` (`cli_value`) | `&IconvSetting` | active (out-of-process) |
| 9b. SSH/daemon bridge | `core/.../remote/flags.rs:228` (`apply_common_server_flags`) | `&IconvSetting` -> `Option<FilenameConverter>` | active (PR #3458) |
| 10. local-copy options | `core/.../run/mod.rs:275` (`build_local_copy_options`) | does not read `config.iconv()` | **dead end** |
| 11. filter compilation | `core/.../run/mod.rs:274` (`compile_filter_program`) and `transfer/.../FilterChain::new` | does not accept a converter | **dead end** |

## Upstream references

Source tree (when present):
`target/interop/upstream-src/rsync-3.4.1/`. Fetch instructions live in
the project conventions document.

- `flist.c::iconv_filename` and the `iconvbufs(ic_send/ic_recv, ...)`
  call-outs at `flist.c:738-754, 1127-1150, 1579-1603, 1605-1621` -
  upstream's per-entry transcoding for filenames and symlink targets,
  the canonical reference for the producer-side application of the
  parsed `iconv_opt`.
- `options.c::recv_iconv_settings` (and the `parse_iconv` /
  `setup_iconv` neighbours) - upstream's parser for
  `--iconv=LOCAL,REMOTE`, configures the global `ic_send` / `ic_recv`
  converter pair. This is the producer side oc-rsync mirrors via
  `IconvSetting::parse` and `IconvSetting::resolve_converter`.
- `exclude.c` - upstream's filter-rule engine. Filter-rule strings
  themselves are *not* iconv-converted in upstream; rather, transcoded
  filenames are matched against locale-encoded patterns because the
  user types patterns in the local charset and `flist.c` has already
  produced local-charset names by the time `exclude.c` runs. The
  oc-rsync analogue requires the same invariant: filter matching must
  see post-transcode names. Today neither
  `crates/transfer/src/{generator/filters.rs,receiver/transfer.rs}`
  nor `crates/filters/src/chain.rs` are aware of an
  `Option<FilenameConverter>`.
- `compat.c:716-718` - gates `CF_SYMLINK_ICONV` on local iconv being
  configured. Mirrors the producer-side signal that oc-rsync currently
  derives from `ClientConfig.iconv` only at
  `core/.../remote/flags.rs:228` and would need to reuse for capability
  emission (see `iconv-pipeline.md` Finding 5).

## Conclusion

The parse path itself is healthy: `--iconv` flows through eight typed
hops from clap to `ClientConfig.iconv` without dropping or aliasing the
value. PR #3458 closed the SSH / daemon bridge by adding a single read
in `apply_common_server_flags`. The two remaining producer-side gaps
are both downstream of `ClientConfig`:

1. The local-copy path
   (`crates/core/src/client/run/mod.rs:275`,
   `LocalCopyOptionsBuilder::build`) never reads `config.iconv()`.
2. The filter-rule path
   (`crates/core/src/client/run/mod.rs:274`,
   `crates/transfer/src/{generator/filters.rs:53,80,receiver/transfer.rs:895}`,
   `crates/filters/src/chain.rs`) accepts no
   `Option<FilenameConverter>`.

Closing those two gaps (per #1914 for filters; the local-copy gap is
implicit in the engine-side scope of #1912 and #1913) is what remains
to make the parsed `IconvSetting` value flow uniformly across every
transfer mode.
