# Parity Status — oc-rsync vs rsync 3.4.1

This report summarises the current behavioural coverage of oc-rsync relative to upstream rsync 3.4.1. Each section identifies the implemented scope, notable interoperability guarantees, and the primary gaps that remain.

## File data transfer

* oc-rsync’s native engine focuses on deterministic local filesystem copies that cover regular files, directories, symbolic links, FIFOs, devices, hard links, and sparse regions. Metadata application follows upstream ordering so attributes are written after content copies.【F:crates/engine/src/local_copy/mod.rs†L1-L24】
* Remote transfers are expected to run through the native Rust transport stack; delegation to upstream `rsync` is prohibited by the mission brief. The CLI help banner reflects this by documenting only the Rust-backed paths.【F:crates/cli/src/frontend/help.rs†L19-L70】
* Gaps: the end-to-end delta-transfer pipeline for cross-host copies is still under active development. Until that work lands, networked transfers remain a parity gap that must be closed without relying on fallback binaries.

## Metadata preservation

* `LocalCopyOptions` wires the preservation toggles implied by `-a` (`--owner`, `--group`, `--perms`, `--times`, `--devices`, `--specials`) and exposes delete timing controls that mirror upstream semantics.【F:crates/engine/src/local_copy/mod.rs†L12-L20】【F:crates/engine/src/local_copy/options/deletion.rs†L1-L85】
* Extended attributes and ACLs are synchronised via the shared `metadata` crate when the corresponding features are enabled and the caller requested them. Filters are honoured so xattr rules mirror upstream include/exclude behaviour.【F:crates/engine/src/local_copy/metadata_sync.rs†L1-L44】
* Ownership and permission toggles negotiated by the CLI feed through to the engine, with clap enforcing the same mutual exclusions that upstream uses (for example, only one `--usermap` or `--groupmap` value is accepted).【F:crates/cli/src/frontend/arguments/parser.rs†L183-L213】
* Gaps: Windows ACL/xattr plumbing and numeric ID handling still track upstream expectations but require additional validation on the native pipeline across platforms.

## Compression and checksums

* Compression negotiation supports zlib by default with optional LZ4 and Zstandard encoders, matching upstream’s selectable algorithm set when the relevant Cargo features are present.【F:crates/engine/src/local_copy/compressor.rs†L1-L52】
* CLI parsing honours `--compress`, `--compress-level`, `--compress-choice`, and `--skip-compress`, flipping the engine settings along the native path.【F:crates/cli/src/frontend/arguments/parser.rs†L153-L166】【F:crates/cli/src/frontend/arguments/parser.rs†L349-L464】
* Strong checksum selection flows through `--checksum-choice` with xxHash variants exposed just like upstream. The rolling checksum remains shared with the in-progress networked delta pipeline so both local and remote flows converge on the Rust implementation.
* Gaps: native compression for networked transfers still needs validation against upstream across protocol versions; the parity harness should add fixtures that compare compressed and uncompressed runs once the remote path is wired end-to-end.

## Delete and pruning semantics

* The engine recognises all delete timing options (`--delete-before`, `--delete-during`, `--delete-delay`, `--delete-after`) and enforces the same mutual exclusivity checks surfaced by the CLI parser.【F:crates/cli/src/frontend/arguments/parser.rs†L118-L147】【F:crates/engine/src/local_copy/options/deletion.rs†L1-L85】
* `--delete-excluded` and `--max-delete` propagate through `LocalCopyOptions`, allowing the local executor to cap removal counts and include excluded entries when requested.【F:crates/engine/src/local_copy/options/deletion.rs†L50-L75】
* Directory handling honours `--prune-empty-dirs` and `--mkpath`, ensuring directory creation and cleanup mirror upstream once filters are applied.【F:crates/cli/src/frontend/command_builder/sections/build_base_command.rs†L169-L214】【F:crates/engine/src/local_copy/options/path_behavior.rs†L98-L104】
* Gaps: cross-host deletion timing remains unvalidated on the native pipeline; parity validation should include mixed local/remote trees as the network stack lands.

## Remote and daemon support

* The CLI exposes the full remote shell and rsync daemon surface so operators can talk to upstream daemons without spawning external `rsync` binaries. Flags such as `--rsh`, `--rsync-path`, `--remote-option`, and authentication helpers are parsed and must be handled by the native transport layer.【F:crates/cli/src/frontend/help.rs†L29-L58】
* Daemon mode (`--daemon`) is branded for oc-rsync and routes to the unified binary entry-point defined in the workspace metadata; configuration paths align with the workspace branding guidance.【F:crates/cli/src/frontend/help.rs†L19-L44】【F:crates/core/src/version/mod.rs†L1-L44】
* Gaps: native server and daemon negotiation still need full parity validation. The parity harness needs end-to-end rsync:// fixtures to lock down behaviour across protocol versions.

## Next steps

1. Integrate the delta-transfer pipeline so native sender/receiver roles cover remote copies without invoking any external binaries.
2. Extend metadata preservation tests to cover ACL/xattr combinations across platforms and document any remaining divergences.
3. Build the parity harness described in the mission brief so YAML status entries can be regression-tested against upstream behaviour.
