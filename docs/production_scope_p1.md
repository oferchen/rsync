# Production Scope P1 (Ship Bar)

This document freezes the mandatory scope that must reach green status before the Rust implementation can be considered production ready. The entries mirror upstream rsync 3.4.1 (protocol 32) behavior and are verified exclusively through observed parity with the upstream project while tracking the branded **oc-rsync 3.4.1-rust** release line.

> **Binary naming note**: The production scope targets the canonical
> `oc-rsync` client and `oc-rsyncd` daemon binaries. Compatibility
> symlinks may still be shipped for downstream tooling, but the native
> Rust executables own the canonical names and configuration layout.

## Platforms
- Linux x86_64
- Linux aarch64

## Transports & Roles
- Client and daemon support for protocols 32 through 28
- SSH stdio transport
- `rsync://` TCP daemon transport

## Core Command-Line Flags
- `-avP` (including the aggregation of `-a`, `-v`, and progress `-P` exactly as upstream renders it)
- `--delete`
- `--exclude`, `--include`, `--filter`
- `--partial`
- `--inplace`
- `--checksum`
- `-z`
- `--bwlimit`
- `--numeric-ids`
- `--owner`
- `--group`
- `--perms`
- `--times`

## Filesystem Objects & Data Handling
- Regular files
- Directories
- Symbolic links
- Hard links
- Device nodes
- FIFOs
- Sparse file handling

## Metadata Fidelity
- UID and GID
- Permission bits
- Nanosecond-resolution modification times
- Symlink targets
- Extended attributes (when compiled in)
- POSIX ACLs (when compiled in)

## Daemon Configuration Parity
- Module definitions
- `auth users`
- `secrets file` (0600 permissions enforced)
- `hosts allow` / `hosts deny`
- `read only`
- `uid` / `gid`
- `numeric ids`
- `chroot`
- `timeout`
- `refuse options`

## User-Facing Messages
- `--help` output
- `--version` output (including `oc-rsync 3.4.1-rust` branding and compiled feature list)
- Error and progress messages with byte-for-byte parity

## Interoperability
- Upstream rsync releases 3.0.9, 3.1.3, and 3.4.1 over loopback `rsync://`
- Matching stdout, stderr, exit codes, and resulting filesystem state

## Quality Gates
- Test coverage ≥ 95% (lines and blocks)
- CI jobs green across lint, tests, packaging, and parity checks
- Exit-code oracle and golden parity harnesses green against upstream references

## Packaging & Artifacts
- Debian package via `cargo-deb`
- RPM package via `cargo-rpm`
- Systemd `oc-rsyncd.service` unit (ships with an alias for legacy unit names when required)
- Default daemon configuration installed at `/etc/oc-rsyncd/oc-rsyncd.conf` with secrets stored in `/etc/oc-rsyncd/oc-rsyncd.secrets`
- CycloneDX SBOM at `target/sbom/rsync.cdx.json`
- Cross-compiled release binaries for Linux (x86_64, aarch64), macOS (x86_64, aarch64), and Windows (x86_64) produced by the CI matrix (Windows x86/aarch64 targets remain disabled to avoid conflicting toolchains)

## Deterministic Test Environment
- `LC_ALL=C`
- `TZ=UTC`
- `COLUMNS=80`
- `RSYNC_TEST_FIXED_TIME=1700000000`
- `UMASK=022`

