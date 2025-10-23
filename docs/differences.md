# Known Differences vs Upstream rsync 3.4.1

This document captures observable gaps between the Rust workspace and upstream
rsync 3.4.1. Each entry describes the user-visible impact today and outlines
what must land to eliminate the difference. Items remain in this list until the
referenced functionality ships and parity is verified by tests or goldens.

## Blocking Differences

- **Client binary uses native local copies and a fallback for remote transfers**
  - *Impact*: `oc-rsync` performs deterministic local filesystem copies for
    regular files, directory trees, symbolic links, and FIFOs while preserving
    permissions and timestamps. A `--dry-run` flag validates transfers without
    mutating the destination, and `--delete` removes destination entries that
    are absent from the source. The client can contact an `rsync://` daemon to
    list available modules and, when remote operands are supplied, spawns the
    system `rsync` binary (configurable via `OC_RSYNC_FALLBACK`) so full network
    functionality remains available while the native transport and delta engine
    are built. Filter handling via `--exclude`/`--exclude-from`/`--include`/
    `--include-from` and `--filter` with `+`/`-` actions, `exclude-if-present=FILE`, and `merge FILE`
    directives mirrors rsync's glob semantics for local copies, but the
    broader filter/merge language is still missing. Progress reporting now
    emits streaming, carriage-return updates akin to upstream `rsync` while the
    richer statistics and delta-transfer metrics remain pending. The `--stats`
    flag emits a deterministic summary covering the counters implemented by the local copy
    engine while the richer upstream metrics remain pending delta-transfer
    support.
  - *Removal plan*: Implement the delta-transfer engine plus supporting crates,
    extend `core::client::run_client` to orchestrate protocol negotiation and
    comprehensive metadata handling, remove the fallback dependency, and
    validate the resulting behaviour via the parity harness.
- **Daemon functionality incomplete**
  - *Impact*: The `oc-rsyncd` binary binds a TCP listener, performs the legacy
    `@RSYNCD:` handshake, and lists modules defined via `--module` arguments or
    a subset of `rsyncd.conf` supplied through `--config` before explaining that
    transfers are unavailable. Authentication and authorization flows are in
    place, and module-level `use chroot` directives are parsed with absolute-path
    enforcement, but real module serving and the broader directive matrix remain
    unimplemented.
  - *Removal plan*: Implement the daemon transport loop, configuration parser,
    and module orchestration described in the mission brief so negotiated
    sessions can progress beyond the initial diagnostic.
- **Transfer engine and metadata pipeline incomplete**
  - *Impact*: The `rsync_engine` crate provides deterministic local copies for
    regular files, directories, symbolic links, device nodes, FIFOs, extended
    attributes, and (when the feature is enabled) POSIX ACLs. Delta transfer
    and compression remain unavailable. Remote orchestration is still missing,
    preventing the client from negotiating network transports.
  - *Removal plan*: Extend the engine with delta-transfer support and integrate
    full filter semantics alongside `compress` and enhanced metadata handling
    from `rsync_meta`.
    Wire the resulting pipeline into both client and daemon orchestration and
    validate it via the parity harness.
- **Interop harness and packaging automation incomplete**
  - *Impact*: There is still no exit-code oracle, goldens, or CI interop matrix.
    Packaging metadata for `cargo-deb`/`cargo-rpm` now installs both binaries, a
    hardened systemd unit, and example configuration files, but SBOM generation
    and CI validation remain pending.
  - *Removal plan*: Stand up the parity harness (`tests/goldens`), CI workflows,
    and finish the packaging pipeline by wiring SBOM generation and automated
    install tests once higher-level crates are available.

All remaining behaviour currently matches the limited scope implemented in the
`protocol` crate; additional differences will be documented here as they are
observed.
