# Compatibility Flags Matrix - Internal Audit

This audit traces fifteen CLI flags that influence protocol behaviour through
our three-layer pipeline:

1. **Parse site** in `crates/cli/` - where Clap registers the long flag and
   the parser collects its boolean / string value.
2. **Propagation field** in `crates/core/src/` - where the flag is stored on
   `ClientConfig` (built via `ClientConfigBuilder`) and forwarded to runtime
   layers.
3. **Runtime use site** in `crates/transfer/` or `crates/engine/` - where the
   flag actually gates work (skipping files, choosing write strategies,
   placing partials, etc.).

The "Status" column reflects only this internal wiring:

- **OK** - all three layers present and connected by name; runtime checks
  the propagated value.
- **PARTIAL** - layers present but path is split / fan-out across multiple
  runtime call sites that each reach the value differently, or the field
  has separate sub-options (e.g. `--backup` plus `--backup-dir`/`--suffix`)
  that each need their own propagation.
- **TODO** - one of the three layers is missing or only consumed in a
  surrounding helper without a direct runtime gate on the propagated field.

This document is an internal pipeline audit. It does not cross-reference
upstream rsync source.

## Master matrix

| Flag                     | CLI parse site (`crates/cli/`)                                                  | Core field (`crates/core/src/`)                                                    | Runtime use (`crates/transfer/` / `crates/engine/`)                                                                | Status  |
|--------------------------|---------------------------------------------------------------------------------|------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------|:-------:|
| `--checksum`             | `frontend/command_builder/sections/build_base_command/transfer.rs:115-128`; collected at `frontend/arguments/parser/mod.rs:573` | `client/config/client/mod.rs:107` (`checksum: bool`); builder field `client/config/builder/mod.rs:178`, builder setter `client/config/builder/validation.rs:8` | `engine/src/local_copy/options/integrity.rs:115` (`checksum_enabled`), gated at `engine/src/local_copy/executor/file/copy/mod.rs:168` and `executor/directory/recursive/checksum.rs:87`; transfer-side flag bit `transfer/src/flags.rs:40,232` | OK      |
| `--inplace`              | `frontend/command_builder/sections/transfer_behavior_options.rs:237`; collected at `frontend/arguments/parser/mod.rs:536` (tri-state) | `client/config/client/mod.rs:150` (`inplace: bool`); builder field `client/config/builder/mod.rs:228`, setter `client/config/builder/partials.rs:46` | `engine/src/local_copy/options/staging.rs:180` (`inplace_enabled`); call sites at `engine/src/local_copy/executor/file/copy/transfer/execute.rs:68,131,345`, `executor/file/copy/transfer/special.rs:49-53`; transfer-side `transfer/src/config/mod.rs:30` and `transfer/src/disk_commit/process.rs:232,313` (`begin.is_inplace`) | OK      |
| `--partial`              | `frontend/command_builder/sections/build_base_command/transfer.rs:292`; collected at `frontend/arguments/parser/mod.rs:437,514` | `client/config/client/mod.rs:143` (`partial: bool`); builder field `client/config/builder/mod.rs:221`, setter `client/config/builder/partials.rs:9` | `engine/src/local_copy/options/staging.rs:153` (`partial_enabled`); transfer-side `transfer/src/flags.rs:71,247`; engine `executor/file/partial.rs:59` (`from_options`) and write-strategy gating `executor/file/copy/transfer/write_strategy.rs:73,81` | OK      |
| `--partial-dir`          | `frontend/command_builder/sections/transfer_behavior_options.rs:7`; collected at `frontend/arguments/parser/mod.rs:502-517` | `client/config/client/mod.rs:144` (`partial_dir: Option<PathBuf>`); builder field `client/config/builder/mod.rs:222`, setter `client/config/builder/partials.rs:25` (`partial_directory`) | `engine/src/local_copy/options/staging.rs:97-99,157-158` (`partial_directory_path`); call sites `engine/src/local_copy/executor/cleanup.rs:67-68`, `executor/file/paths.rs:33-43` (`partial_directory_destination_path`); validation `transfer/src/config/builder.rs:441-444` (`--append` vs `--partial-dir`) | OK      |
| `--whole-file`           | `frontend/command_builder/sections/transfer_behavior_options.rs:72`; collected at `frontend/arguments/parser/mod.rs:545` (tri-state) | `client/config/client/mod.rs:106` (`whole_file: Option<bool>`); builder field `client/config/builder/mod.rs:177`, setter `client/config/builder/performance.rs:85,96` | `transfer/src/flags.rs:62,243`; basis selection `transfer/src/receiver/basis.rs:76,229`; pipeline gating `transfer/src/receiver/transfer/pipeline.rs:175,198,217`; receiver `transfer/src/receiver/mod.rs:321` | OK      |
| `--append`               | `frontend/command_builder/sections/transfer_behavior_options.rs:102`; collected at `frontend/arguments/parser/mod.rs:537-543` (with `append-verify` shortcut) | `client/config/client/mod.rs:151` (`append: bool`); builder field `client/config/builder/mod.rs:229`, setter `client/config/builder/partials.rs:54` | `transfer/src/flags.rs:91`; receiver `transfer/src/transfer_ops/mod.rs:222,228` (`append_offset`), `transfer/src/transfer_ops/request.rs:133`; pipeline `transfer/src/receiver/transfer/pipeline.rs:92`; engine staging `engine/src/local_copy/executor/file/append.rs:29-63`; conflict validation `transfer/src/config/builder.rs:441` | OK      |
| `--append-verify`        | `frontend/command_builder/sections/transfer_behavior_options.rs:120`; collected at `frontend/arguments/parser/mod.rs:537,819` | `client/config/client/mod.rs:152` (`append_verify: bool`); builder field `client/config/builder/mod.rs:230`, setter `client/config/builder/partials.rs:65` | `engine/src/local_copy/options/staging.rs:127,193` (`append_verify_enabled`); call sites `engine/src/local_copy/executor/file/append.rs:63`, `executor/file/copy/transfer/execute.rs:66,269`, `executor/file/copy/mod.rs:172,204`. Not exposed to the transfer-pipeline daemon path - verified at engine staging only. | PARTIAL |
| `--update`               | `frontend/command_builder/sections/build_base_command/transfer.rs:178`; collected at `frontend/arguments/parser/mod.rs:671` | `client/config/client/mod.rs:115` (`update: bool`); builder field `client/config/builder/mod.rs:186`, setter `client/config/builder/selection.rs:26` | `engine/src/local_copy/options/integrity.rs:91-92,172` (`update`/`update_enabled`); call sites `engine/src/local_copy/context_impl/options.rs:407`, `executor/file/copy/dry_run.rs:45`, `executor/file/copy/existing.rs:25`; transfer-side `transfer/src/flags.rs:73` | OK      |
| `--modify-window`        | `frontend/command_builder/sections/build_base_command/transfer.rs:185`; collected at `frontend/arguments/parser/mod.rs:193` | `client/config/client/mod.rs:74` (`modify_window: Option<u64>`); builder field `client/config/builder/mod.rs:151`, setter `client/config/builder/selection.rs:10` | `engine/src/local_copy/options/integrity.rs:109,182-183` (`modify_window`); call sites `engine/src/local_copy/context_impl/state.rs:359`, `executor/file/copy/dry_run.rs:50`, `executor/file/copy/transfer/execute.rs:604`, `executor/file/copy/existing.rs:30`, `executor/reference.rs:96`; comparison engine `executor/file/comparison.rs:31,38,113,163` | OK      |
| `--size-only`            | `frontend/command_builder/sections/build_base_command/transfer.rs:151`; collected at `frontend/arguments/parser/mod.rs:574` | `client/config/client/mod.rs:110` (`size_only: bool`); builder field `client/config/builder/mod.rs:181`, setter `client/config/builder/selection.rs:14` | `engine/src/local_copy/options/integrity.rs:44,135` (`size_only`/`size_only_enabled`); validation `engine/src/local_copy/options/builder/validation.rs:10-12` (mutually exclusive with `--checksum`); call sites `engine/src/local_copy/executor/file/comparison.rs:109,135,154`, `executor/file/copy/links.rs:205`, `executor/file/copy/transfer/execute.rs:600` | OK      |
| `--ignore-existing`      | `frontend/command_builder/sections/build_base_command/transfer.rs:164`; collected at `frontend/arguments/parser/mod.rs:669` | `client/config/client/mod.rs:112` (`ignore_existing: bool`); builder field `client/config/builder/mod.rs:183`, setter `client/config/builder/selection.rs:18` | `engine/src/local_copy/options/integrity.rs:60,148` (`ignore_existing`/`ignore_existing_enabled`); call sites `engine/src/local_copy/executor/file/copy/dry_run.rs:67`, `executor/file/copy/existing.rs:48` | OK      |
| `--existing`             | `frontend/command_builder/sections/build_base_command/transfer.rs:170`; collected at `frontend/arguments/parser/mod.rs:670` | `client/config/client/mod.rs:113` (`existing_only: bool`); builder field reachable via builder selection setters `client/config/builder/selection.rs` (`existing_only`) | `engine/src/local_copy/options/integrity.rs:68,154` (`existing_only`/`existing_only_enabled`); call sites `engine/src/local_copy/executor/file/copy/mod.rs:114`, `executor/directory/recursive/mod.rs:82`, `executor/special/device.rs:85`, `executor/special/symlink.rs:177`, `executor/special/fifo.rs:87` | OK      |
| `--backup`               | `frontend/command_builder/sections/transfer_behavior_options.rs:372`; collected at `frontend/arguments/parser/mod.rs:263-267` (auto-enabled by `--backup-dir` / `--suffix`) | `client/config/client/mod.rs:146-148` (`backup: bool`, `backup_dir: Option<PathBuf>`, `backup_suffix: Option<OsString>`); builder field `client/config/builder/mod.rs:224`, setter `client/config/builder/paths.rs:54` | `engine/src/local_copy/options/backup.rs:14,57` (`backup`/`backup_enabled`); engine state `engine/src/local_copy/context_impl/state.rs:466`; gating `engine/src/local_copy/executor/file/copy/transfer/execute.rs:610`; transfer-side wire bit `transfer/src/flags.rs:95`. The auxiliary `backup_dir` / `backup_suffix` fields propagate through the same builder but flow into a separate engine path (backup placement vs the boolean gate). | PARTIAL |
| `--remove-source-files`  | `frontend/command_builder/sections/transfer_behavior_options.rs:88`; collected at `frontend/arguments/parser/mod.rs:534-535` (also accepts deprecated `--remove-sent-files`) | `client/config/client/mod.rs:75` (`remove_source_files: bool`); builder field `client/config/builder/mod.rs:152`, setter `client/config/builder/selection.rs:12` | `engine/src/local_copy/options/limits.rs:30,96-97` (`remove_source_files`/`remove_source_files_enabled`); engine context `engine/src/local_copy/context_impl/options.rs:352-353`; gating `engine/src/local_copy/executor/cleanup.rs:354` | OK      |
| `--temp-dir`             | `frontend/command_builder/sections/transfer_behavior_options.rs:15`; collected at `frontend/arguments/parser/mod.rs:519-520` | `client/config/client/mod.rs:145` (`temp_directory: Option<PathBuf>`); builder field `client/config/builder/mod.rs:223`, setter `client/config/builder/partials.rs:37` (`temp_directory`) | `engine/src/local_copy/options/types.rs:174` (`temp_dir: Option<PathBuf>`); path helper `engine/src/local_copy/executor/file/paths.rs:61-68` (`temporary_destination_path`); guard `engine/src/local_copy/executor/file/guard.rs:147`; gating `engine/src/local_copy/executor/file/copy/transfer/execute.rs:138,349`, `executor/file/copy/transfer/write_strategy.rs:66,73,81,193,211` (`has_temp_directory`). Builder field name (`temp_directory`) differs from the engine struct field name (`temp_dir`) - both reach the same runtime decision but the rename is unobvious. | PARTIAL |

## Why three flags are PARTIAL

- **`--append-verify`** - the verify variant only fans out into the engine
  staging path (`local_copy::executor::file::append`) and is not consumed by
  the daemon transfer pipeline. The plain `--append` path is what
  `transfer/src/transfer_ops/mod.rs` uses. The verify-after-write step lives
  exclusively on the local-copy side.
- **`--backup`** - the boolean is wired straight through, but the related
  options `--backup-dir` (`client/config/builder/paths.rs:54`,
  `executor/file/copy/transfer/execute.rs:610`) and `--suffix` are separate
  fields that travel to runtime independently. Auditors looking at "is
  `--backup` plumbed?" must check three propagation fields, not one. Parser
  also auto-promotes `backup = true` whenever `--backup-dir` or `--suffix`
  is present without `--backup`, which is non-obvious from the field name.
- **`--temp-dir`** - the builder setter is named `temp_directory` while the
  engine struct names the field `temp_dir`. Same value, but `grep temp_dir`
  in `crates/core/src` returns zero hits, which can mislead auditors. The
  rename happens in `client/config/builder/mod.rs:317-end` during `build()`.

## Tri-state vs binary flags

Five of the audited flags are tri-state (positive / negative-with-`--no-X` /
unset), parsed via `tri_state_flag_positive_first`:

- `--checksum` / `--no-checksum` (parser line 573)
- `--inplace` / `--no-inplace` (parser line 536)
- `--whole-file` / `--no-whole-file` (parser line 545)
- `--partial` / `--no-partial` (parser lines 437, 514)

These are stored as `Option<bool>` in the builder (or as `bool` after
`unwrap_or(default)` resolution). `--whole-file` is the only one that stays
`Option<bool>` all the way into `ClientConfig` (line 106), because the
runtime needs to distinguish "user did not pass `--whole-file`" from "user
passed `--no-whole-file`". The other tri-states collapse to `bool` at config
build time.

The rest are plain boolean flags or value-bearing options. Flags listed in
the `--checksum` row use a separate `checksum-choice` value flag (parsed at
`frontend/arguments/parser/mod.rs:577,596`) that is independent of the
boolean `--checksum` gate audited here.

## Cross-flag invariants enforced at config build time

These propagation-level checks prevent invalid combinations from reaching
the runtime:

- `--inplace` vs `--delay-updates` - `crates/core/src/client/config/builder/mod.rs:287`.
- `--append` vs `--partial-dir` - `crates/transfer/src/config/builder.rs:441-444`.
- `--size-only` vs `--checksum` - `crates/engine/src/local_copy/options/builder/validation.rs:10-12`.
- `--append` implies `--inplace` - propagated through `client/config/builder/partials.rs:54`.
- `--partial-dir` implies `--partial` - `client/config/builder/partials.rs:25-27`.
- `--append-verify` implies `--append` - `client/config/builder/partials.rs:65-70`.
- `--backup-dir` / `--suffix` imply `--backup` - parser auto-promotion at
  `frontend/arguments/parser/mod.rs:266-267`.

These chained implications make some flags unreachable individually:
`--append-verify` always sets `append`; `--partial-dir` always sets
`partial`. Any audit comparing "is `--X` set" must therefore consult the
post-build `ClientConfig` getters, not the raw parser output.
