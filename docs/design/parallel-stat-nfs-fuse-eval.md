# Evaluate PARALLEL_STAT_THRESHOLD for NFS/FUSE workloads (#1084)

Status: design draft. Tracks issue #1084. Cross-references the profile
work in #1083 (pending data) for the local-fs baseline.

## 1. Current default

`PARALLEL_STAT_THRESHOLD = 64` lives in the receiver and gates the dual
sequential/rayon path used to refresh `stat()` metadata before delta
generation. The constant was tuned against tmpfs and ext4, where a
single `stat()` resolves in tens of microseconds and 64 entries is a
reasonable break-even point for rayon scheduling overhead.

The threshold is wrong on high-latency filesystems:

- NFSv3/NFSv4 cold-cache `stat()` regularly exceeds 100 ms per call.
- FUSE userspace passthroughs (sshfs, rclone-mount, gcsfuse) sit in the
  1-50 ms range, dominated by IPC and remote round-trips.
- SMB/CIFS over WAN behaves like NFS but with deeper queue effects.

At those latencies the rayon scheduling cost (microseconds) is
negligible, so we want parallelism much earlier - eight files is enough
to amortise. Keeping the threshold at 64 leaves >5 s of wall-clock on
the table for a 64-file refresh on a 100 ms-per-stat NFS share.

## 2. Filesystem detection

Linux exposes the filesystem class through `statfs(2)` and the
`f_type` magic number. We need:

- `NFS_SUPER_MAGIC = 0x6969`
- `FUSE_SUPER_MAGIC = 0x65735546`
- `SMB_SUPER_MAGIC = 0x517B` / `CIFS_MAGIC_NUMBER = 0xFF534D42`
- `BTRFS_SUPER_MAGIC = 0x9123683E`
- `EXT4` shares `EXT2_SUPER_MAGIC = 0xEF53`
- `TMPFS_MAGIC = 0x01021994`

macOS uses `statfs.f_fstypename` (string: `"nfs"`, `"smbfs"`,
`"osxfuse"`, `"apfs"`). Windows exposes
`GetVolumeInformationW` -> `lpFileSystemNameBuffer` (`"NTFS"`, `"ReFS"`,
`"FAT32"`) plus `WNetGetUniversalNameW` for SMB shares.

The detection helper belongs in `fast_io::fs_kind` (new submodule)
alongside the existing platform abstractions. `core` and the receiver
consume it through a safe enum:

```rust
pub enum FsKind { Local, Nfs, Smb, Fuse, Unknown }
```

## 3. Per-FS threshold table

| Filesystem class       | Threshold | Rationale                          |
|------------------------|-----------|------------------------------------|
| tmpfs / btrfs / ext4   | 64        | local fast, current baseline       |
| APFS / NTFS / ReFS     | 64        | local fast, parity with Linux      |
| NFS / SMB / sshfs      | 8         | 100 ms+ per stat, parallelise early|
| FUSE generic           | 16        | 1-50 ms range, mid-tier            |
| Unknown / network bind | 16        | conservative middle ground         |

Values are starting points; #1083 will produce empirical data to refine
them. Threshold lookup must be O(1) in the hot path.

## 4. Implementation sketch

```rust
pub struct ParallelStatThreshold(pub usize);

pub fn detect_fs_kind(path: &Path) -> FsKind { /* statfs / fstatfs */ }

pub fn threshold_for(kind: FsKind) -> ParallelStatThreshold {
    match kind {
        FsKind::Nfs | FsKind::Smb => ParallelStatThreshold(8),
        FsKind::Fuse              => ParallelStatThreshold(16),
        FsKind::Unknown           => ParallelStatThreshold(16),
        FsKind::Local             => ParallelStatThreshold(64),
    }
}
```

Per-mount cache: `OnceLock<DashMap<DeviceId, FsKind>>` keyed on
`statfs.f_fsid` (Unix) or volume serial (Windows). Population is lazy
on first stat per device. Cache is scoped to one transfer; we drop it
on session teardown to avoid stale entries when mounts change.

Receiver call site replaces the literal `64` with `threshold.0`,
resolved once per directory descent and reused for siblings.

## 5. Risks

- **Bind mounts and overlay**: `/var/lib/docker/overlay2` reports
  `OVERLAYFS_SUPER_MAGIC` even when the lower layer is NFS; we treat it
  as `Unknown` and accept the conservative threshold of 16. autofs
  similarly hides the real backing fs until first traversal.
- **Magic number drift**: Linux occasionally renumbers experimental
  filesystems (early bcachefs). Unknown magics fall back to `Unknown`,
  not to `Local`, to avoid pessimising rare setups.
- **Per-directory mount changes**: a transfer that crosses mount
  boundaries (NFS -> tmpfs) needs detection per directory, not per
  session. The cache key must be the device id, not the root path.
- **macOS string match brittleness**: `f_fstypename` differs between
  `osxfuse`, `macfuse`, and `fskit_fuse`. Match a prefix list, not an
  exact string.
- **Windows UNC vs drive letter**: a mapped drive (`Z:\`) reports
  `NTFS` even when the share is SMB. Detect via
  `GetDriveTypeW == DRIVE_REMOTE` before trusting the fs name.
- **Misdetection cost is asymmetric**: classifying NFS as local hurts
  (we serialise 100 ms-per-call), classifying local as NFS is cheap
  (rayon overhead at small N). Bias the unknown branch toward the
  parallel side.
