# Production Scope P1 — Release Readiness Gate

The following criteria must be **green** before this project can be considered "production ready." Each item is testable and maps to upstream rsync 3.4.1 behavior (protocol 32) to guarantee functional identity.

## Platforms
- Linux x86_64
- Linux aarch64

## Functionality
- Client and daemon operation supporting protocols 32 through 28 over both SSH stdio transport and `rsync://` TCP connections.
- Core flag coverage: `-avP`, `--delete`, `--exclude`, `--include`, `--filter`, `--partial`, `--inplace`, `--checksum`, `-z`, `--bwlimit`, `--numeric-ids`, `--owner`, `--group`, `--perms`, `--times`.
- File type handling: regular files, directories, symlinks, hard links, device nodes, FIFOs, sparse files.
- Metadata fidelity: uid, gid, permissions, nanosecond mtime, symlink targets, extended attributes, and POSIX ACLs when compiled with support.
- Daemon parity with upstream `rsyncd.conf`: modules, `auth users`, `secrets`, `hosts allow` / `hosts deny`, `read only`, `uid`, `gid`, `numeric ids`, `chroot`, `timeout`, `refuse options`.
- User-visible messages, help output, version banners, and error text match upstream byte-for-byte.
- Interoperability passes against upstream rsync 3.0.9, 3.1.3, and 3.4.1 over loopback.

## Quality Gates
- Code coverage (lines and blocks) ≥ 95% via `cargo llvm-cov`.
- Packaging artifacts produced and validated: `.deb`, `.rpm`, systemd unit, and CycloneDX SBOM.

