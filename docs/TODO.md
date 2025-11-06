## Completed tasks

- Compression now uses the system zlib backend so the encoder and decoder share
  the same implementation as upstream rsync, and checksum validation continues
  to rely on the upstream algorithms provided by `rsync_checksums`.
- `oc-rsyncd` and `rsyncd` are thin wrappers that invoke the client binary with
  an implicit `--daemon`, mirroring the upstream symlink behaviour.
