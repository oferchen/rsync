# Known Differences vs Upstream rsync 3.4.1

This document captures observable gaps between the Rust workspace and upstream
rsync 3.4.1. Each entry describes the user-visible impact today and outlines
what must land to eliminate the difference. Items remain in this list until the
referenced functionality ships and parity is verified by tests or goldens.

## Blocking Differences

- **No client or daemon binaries**
  - *Impact*: Users cannot execute transfers because `bin/rsync` and `bin/rsyncd`
    are absent.
  - *Removal plan*: Introduce the `cli` and `daemon` crates described in the
    workspace layout, ensure they invoke the shared `core` facade, and verify
    CLI/daemon parity via snapshot and interop tests.
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
