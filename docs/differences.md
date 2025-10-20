# Known Differences vs Upstream rsync 3.4.1

This document captures observable gaps between the Rust workspace and upstream
rsync 3.4.1. Each entry describes the user-visible impact today and outlines
what must land to eliminate the difference. Items remain in this list until the
referenced functionality ships and parity is verified by tests or goldens.

## Blocking Differences

- **Client binary implements local copies only**
  - *Impact*: `oc-rsync` performs deterministic local filesystem copies for
    regular files and directory trees. Remote transfers, metadata
    preservation, filters, compression, and progress reporting remain
    unavailable.
  - *Removal plan*: Implement the delta-transfer engine plus supporting crates,
    extend `core::client::run_client` to orchestrate protocol negotiation and
    metadata handling, and validate the resulting behaviour via the parity
    harness.
- **Daemon functionality missing**
  - *Impact*: The `rsyncd` binary now exists but reports that daemon support is
    unavailable. Launch attempts exit with code `1` and render a diagnostic via
    `rsync_daemon::run_daemon` explaining that the server mode has not been
    implemented.
  - *Removal plan*: Implement the daemon transport loop, configuration parser,
    and module orchestration described in the mission brief, then replace the
    placeholder diagnostic with real session handling.
- **Transfer engine and metadata pipeline missing**
  - *Impact*: Delta transfer, metadata preservation, filters, and compression are
    unavailable. The `core` crate currently only exposes message formatting
    helpers, so there is no orchestration that wires negotiation into an actual
    file transfer.
  - *Removal plan*: Implement the `engine`, `meta`, `filters`, and `compress`
    crates with exhaustive unit/integration coverage, extend `core` with the
    client/daemon orchestration APIs, then connect them through the workspace
    facade before re-running the parity harness.
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
