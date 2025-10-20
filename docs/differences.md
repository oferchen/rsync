# Known Differences vs Upstream rsync 3.4.1

This document captures observable gaps between the Rust workspace and upstream
rsync 3.4.1. Each entry describes the user-visible impact today and outlines
what must land to eliminate the difference. Items remain in this list until the
referenced functionality ships and parity is verified by tests or goldens.

## Blocking Differences

- **Client binary implements local copies only**
  - *Impact*: `oc-rsync` performs deterministic local filesystem copies for
    regular files, directory trees, and symbolic links while preserving
    permissions and timestamps. Remote transfers, ownership/xattrs/ACLs,
    filters, compression, and progress reporting remain unavailable.
  - *Removal plan*: Implement the delta-transfer engine plus supporting crates,
    extend `core::client::run_client` to orchestrate protocol negotiation and
    comprehensive metadata handling, and validate the resulting behaviour via
    the parity harness.
- **Daemon functionality incomplete**
  - *Impact*: The `rsyncd` binary binds a TCP listener, performs the legacy
    `@RSYNCD:` handshake, and responds with an `@ERROR` message explaining that
    module serving is unavailable. Real module configuration, authentication,
    and file transfers remain unimplemented.
  - *Removal plan*: Implement the daemon transport loop, configuration parser,
    and module orchestration described in the mission brief so negotiated
    sessions can progress beyond the initial diagnostic.
- **Transfer engine and metadata pipeline incomplete**
  - *Impact*: The `rsync_engine` crate provides deterministic local copies for
    regular files, directories, and symbolic links, but delta transfer,
    ownership/xattrs/ACLs, filters, and compression remain unavailable. Remote
    orchestration is still missing, preventing the client from negotiating
    network transports.
  - *Removal plan*: Extend the engine with delta-transfer support and integrate
    `filters`, `compress`, and enhanced metadata handling from `rsync_meta`.
    Wire the resulting pipeline into both client and daemon orchestration and
    validate it via the parity harness.
- **No interop or packaging automation**
  - *Impact*: There is no exit-code oracle, goldens, CI interop matrix, or
    packaging artifacts, preventing validation against upstream and distribution
    deliverables.
  - *Removal plan*: Stand up the parity harness (`tests/goldens`), CI workflows,
    and packaging targets (deb/rpm, SBOM, systemd unit) defined in the mission
    brief once higher-level crates are available.

All remaining behaviour currently matches the limited scope implemented in the
`protocol` crate; additional differences will be documented here as they are
observed.
