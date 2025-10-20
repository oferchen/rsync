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
- **Daemon functionality missing**
  - *Impact*: The `rsyncd` binary now exists but reports that daemon support is
    unavailable. Launch attempts exit with code `1` and render a diagnostic via
    `rsync_daemon::run_daemon` explaining that the server mode has not been
    implemented.
  - *Removal plan*: Implement the daemon transport loop, configuration parser,
    and module orchestration described in the mission brief, then replace the
    placeholder diagnostic with real session handling.
- **Transfer engine and metadata pipeline incomplete**
  - *Impact*: Delta transfer, ownership/xattrs/ACLs, filters, and compression are
    unavailable. The new `rsync_meta` crate handles permission and timestamp
    preservation for local copies, but higher-level metadata and remote
    orchestration remain absent, so the core crate cannot yet drive protocol
    negotiation into a real transfer.
  - *Removal plan*: Implement the `engine`, `filters`, and `compress` crates,
    extend `rsync_meta` to cover the remaining metadata, and integrate the
    resulting pipeline with `core` client/daemon orchestration before running
    the parity harness.
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
