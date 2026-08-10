# metadata

Metadata preservation helpers - permissions, timestamps, ownership, ACLs,
xattrs, and special file creation.

## Purpose

`metadata` centralises filesystem metadata operations for the workspace,
reproducing upstream rsync semantics for permission bits, nanosecond timestamps,
uid/gid ownership, ACLs, extended attributes, and special file nodes (FIFOs,
devices, symlinks). Higher layers wire these helpers into transfer pipelines so
metadata handling remains consistent across client and daemon roles.

## Key Public Types

- `apply_file_metadata` - set permissions and timestamps on regular files
- `apply_directory_metadata` - mirror metadata for directories
- `apply_symlink_metadata` - timestamps on symlinks without following the target
- `create_fifo` / `create_device_node` - materialise special files
- `MetadataError` - context-rich error with path, operation, and underlying I/O error
- `IdLookup` - uid/gid name resolution (getpwnam_r/getgrnam_r)
- `StatCache` - caching stat results to reduce syscall overhead
- `apply_batch` - parallel metadata application via rayon

## Dependencies (upstream)

`protocol`, `logging`, `filetime`, `rustix`, `libc`

## Dependents (downstream)

`engine`, `transfer`, `daemon`, `core`, `cli`, `batch`

## Features

- `xattr` - extended attribute preservation (Linux/macOS via `xattr` crate)
- `acl` - POSIX ACL support (Linux/macOS/FreeBSD via `exacl` crate)

## Platform Notes

- **Unix**: Full metadata fidelity - permissions, ownership, timestamps,
  xattrs, ACLs, device nodes, FIFOs, symlinks
- **macOS**: Apple-specific metadata via `apple-fs` crate (birthtime, flags)
- **Windows**: Read-only flag, NTFS DACLs via `windows` crate, limited
  permission round-trip
