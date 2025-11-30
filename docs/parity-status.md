# Parity Status — oc-rsync vs rsync 3.4.1

This report summarises the current behavioural coverage of oc-rsync relative to upstream rsync 3.4.1. Each section identifies the implemented scope, notable interoperability guarantees, and the primary gaps that remain.

## File data transfer

* oc-rsync’s native engine focuses on deterministic local filesystem copies that cover regular files, directories, symbolic links, FIFOs, devices, hard links, and sparse regions. Metadata application follows upstream ordering so attributes are written after content copies.【F:crates/engine/src/local_copy/mod.rs†L1-L24】
* The user-facing CLI explicitly documents that this snapshot only accepts local sources; remote transfers are currently unsupported until the native transport and server paths land. Remote transport flags remain parsed so the CLI can surface precise diagnostics without delegating to a system `rsync` binary.【F:crates/cli/src/frontend/help.rs†L19-L70】
* Remote execution paths do not spawn the system `rsync` binary. Instead, they fail with branded diagnostics until the native sender/receiver pipeline is wired up end-to-end.
* Gaps: delta-transfer scheduling between native sender/receiver roles is still in development, so cross-host copies are unavailable. Local support for batch mode (`--write-batch`/`--read-batch`) also remains unimplemented.

## Metadata preservation

* `LocalCopyOptions` wires the preservation toggles implied by `-a` (`--owner`, `--group`, `--perms`, `--times`, `--devices`, `--specials`) and exposes delete timing controls that mirror upstream semantics.【F:crates/engine/src/local_copy/mod.rs†L12-L20】【F:crates/engine/src/local_copy/options/deletion.rs†L1-L85】
* Extended attributes and ACLs are synchronised via the shared `metadata` crate when the corresponding features are enabled and the caller requested them. Filters are honoured so xattr rules mirror upstream include/exclude behaviour.【F:crates/engine/src/local_copy/metadata_sync.rs†L1-L44】
* Ownership and permission toggles negotiated by the CLI feed through to the engine, with clap enforcing the same mutual exclusions that upstream uses (for example, only one `--usermap` or `--groupmap` value is accepted).【F:crates/cli/src/frontend/arguments/parser.rs†L183-L213】
* Gaps: Windows ACL/xattr plumbing and numeric ID handling remain incomplete pending native platform work.

## Compression and checksums

* Compression negotiation supports zlib by default with optional LZ4 and Zstandard encoders, matching upstream’s selectable algorithm set when the relevant Cargo features are present.【F:crates/engine/src/local_copy/compressor.rs†L1-L52】
* CLI parsing honours `--compress`, `--compress-level`, `--compress-choice`, and `--skip-compress`, flipping the engine settings for local transfers. Remote compression negotiation will be enabled alongside the native transport implementation.【F:crates/cli/src/frontend/arguments/parser.rs†L153-L166】【F:crates/cli/src/frontend/arguments/parser.rs†L349-L464】
* Strong checksum selection flows through `--checksum-choice` with xxHash variants exposed just like upstream. The rolling checksum remains ready for networked delta once the transport pipeline is in place.
* Gaps: native compression currently covers local copies; remote transfers are unavailable until the network engine lands. The parity harness should add fixtures that compare compressed and uncompressed runs once network mode lands.

## Delete and pruning semantics

* The engine recognises all delete timing options (`--delete-before`, `--delete-during`, `--delete-delay`, `--delete-after`) and enforces the same mutual exclusivity checks surfaced by the CLI parser.【F:crates/cli/src/frontend/arguments/parser.rs†L118-L147】【F:crates/engine/src/local_copy/options/deletion.rs†L1-L85】
* `--delete-excluded` and `--max-delete` propagate through `LocalCopyOptions`, allowing the local executor to cap removal counts and include excluded entries when requested.【F:crates/engine/src/local_copy/options/deletion.rs†L50-L75】
* Directory handling honours `--prune-empty-dirs` and `--mkpath`, ensuring directory creation and cleanup mirror upstream once filters are applied.【F:crates/cli/src/frontend/command_builder/sections/build_base_command.rs†L169-L214】【F:crates/engine/src/local_copy/options/path_behavior.rs†L98-L104】
* Gaps: cross-host deletion timing cannot be validated until remote transfers are implemented natively.

## Remote and daemon support

* The CLI exposes the full remote shell and rsync daemon surface so operators can talk to upstream daemons today. Flags such as `--rsh`, `--rsync-path`, `--remote-option`, and authentication helpers are forwarded unchanged to the fallback invocation.【F:crates/cli/src/frontend/help.rs†L29-L58】【F:crates/core/src/client/fallback/runner/command_builder.rs†L319-L501】
* Daemon mode (`--daemon`) is branded for oc-rsync but currently delegates to the unified binary entry-point defined in the workspace metadata; configuration paths align with the workspace branding guidance.【F:crates/cli/src/frontend/help.rs†L19-L44】【F:crates/core/src/version/mod.rs†L1-L44】
* Gaps: native server mode still relies on the fallback implementation for network I/O and module negotiation. The parity harness needs end-to-end rsync:// fixtures to lock down behaviour before unifying the Rust daemon with upstream.
* Removal plan: the mission brief requires eliminating delegation to the system `rsync` binary. Native server/daemon entrypoints must replace fallback invocations, with CI coverage that proves oc-rsync can operate when no upstream `rsync` is installed. As the native pipeline lands, the `remote_fallback_*` CLI tests and docs must be rewritten to exercise the Rust server instead of expecting delegation.

## Next steps

1. Integrate the delta-transfer pipeline so native sender/receiver roles cover remote copies without invoking external binaries.
2. Extend metadata preservation tests to cover ACL/xattr combinations across platforms and document any remaining divergences.
3. Build the parity harness described in the mission brief so YAML status entries can be regression-tested against upstream behaviour.
