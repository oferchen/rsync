# Known Differences vs Upstream rsync 3.4.1

This document captures observable gaps between the Rust workspace and upstream
rsync 3.4.1. Each entry describes the user-visible impact today and outlines
what must land to eliminate the difference. Items remain in this list until the
referenced functionality ships and parity is verified by tests or goldens.

## Blocking Differences

- **`oc-rsync` client uses native local copies and a fallback for remote transfers**
  - *Impact*: `oc-rsync` performs deterministic local filesystem copies for
    regular files, directory trees, symbolic links, and FIFOs while preserving
    permissions and timestamps. It can proactively create parent directories via
    `--mkpath` even when implied directories are disabled. Append-only updates via
    `--append` and `--append-verify` reuse the native verification logic when the
    destination already contains a prefix of the incoming file. A `--dry-run` flag
    validates transfers without
    mutating the destination, and `--delete`, `--delete-after`, and
    `--delete-delay` remove destination entries that
    are absent from the source. The client can contact an `rsync://` daemon to
    list available modules and, when remote operands are supplied, spawns the
    system `rsync` binary (configurable via `OC_RSYNC_FALLBACK`) so full network
    functionality remains available while the native transport and delta engine
    are built. Filter handling via `--exclude`/`--exclude-from`/`--include`/
    `--include-from` and `--filter` with `+`/`-` actions, `show`/`hide`,
    `protect`/`risk`, `exclude-if-present=FILE`, and `merge`/`dir-merge`
    directives (including their `.`/`:` shorthands) mirrors rsync's glob
    semantics for local copies, but the broader filter/merge language is still
    missing. Progress reporting now
    emits streaming, carriage-return updates akin to upstream `rsync` while the
    richer statistics and delta-transfer metrics remain pending. The `--stats`
    flag emits a deterministic summary covering the counters implemented by the local copy
    engine while the richer upstream metrics remain pending delta-transfer
    support. The compatibility wrapper installed as `rsync` delegates to the
    same front-end and therefore inherits these limitations.
  - *Removal plan*: Implement the delta-transfer engine plus supporting crates,
    extend `core::client::run_client` to orchestrate protocol negotiation and
    comprehensive metadata handling, remove the fallback dependency, and
    validate the resulting behaviour via the parity harness.
- **Daemon functionality incomplete (`oc-rsyncd` and compatibility wrapper)**
  - *Impact*: The `oc-rsyncd` binary binds a TCP listener, performs the legacy
    `@RSYNCD:` handshake, and lists modules defined via `--module` arguments or
    a subset of `rsyncd.conf` supplied through `--config`. When the upstream
    `rsync` binary is available, the daemon now delegates authenticated module
    sessions to it by default so clients retain end-to-end transfers while the
    native data path is completed. Delegation can be disabled explicitly via
    `OC_RSYNC_DAEMON_FALLBACK=0`/`false` (or the shared `OC_RSYNC_FALLBACK`
    override); when disabled or when the helper binary is missing the daemon
    explains that transfers are unavailable after completing authentication.
    Authentication and authorization flows are in place, and module-level
    `use chroot` directives are parsed with absolute-path enforcement, but real
    module serving and the broader directive matrix remain unimplemented when
    delegation is not possible. The compatibility wrapper installed as `rsyncd`
    forwards to the same implementation and therefore inherits these
    limitations.
  - *Removal plan*: Implement the daemon transport loop, configuration parser,
    and module orchestration described in the mission brief so negotiated
    sessions can progress beyond the initial diagnostic.
- **Transfer engine integration incomplete**
  - *Impact*: The `rsync_engine` crate provides deterministic local copies for
    regular files, directories, symbolic links, device nodes, FIFOs, extended
    attributes, and (when enabled) POSIX ACLs. Delta-token generation and
    application are available via
    [`DeltaGenerator`](../crates/engine/src/delta/generator.rs) and
    [`apply_delta`](../crates/engine/src/delta/script.rs), and local copies honour
    compression toggles for bandwidth limiting and statistics through
    [`CountingZlibEncoder`](../crates/engine/src/local_copy.rs). The client still
    spawns the system `rsync` for network transfers because the delta pipeline
    has not yet been wired into the protocol and transport layers, and the
    broader filter grammar remains a work in progress.
  - *Removal plan*: Wire the delta pipeline into the client/daemon transfer
    flow, finish the remaining filter semantics, and validate the combined
    behaviour via the parity harness before dropping the fallback dependency.
- **Interop harness and packaging automation incomplete**
  - *Impact*: There is still no exit-code oracle, goldens, or CI interop matrix.
    Packaging metadata for `cargo-deb`/`cargo-rpm` now installs both binaries, a
    hardened systemd unit, and example configuration files. SBOM generation is
    covered by `cargo xtask sbom`, but CI validation and automated package
    checks remain pending.
  - *Removal plan*: Stand up the parity harness (`tests/goldens`), CI workflows,
    and finish the packaging pipeline by wiring the remaining package checks and
    install tests once higher-level crates are available.

All remaining behaviour currently matches the limited scope implemented in the
`protocol` crate; additional differences will be documented here as they are
observed.
