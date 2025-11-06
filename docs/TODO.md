## Completed tasks

- Compression now uses the system zlib backend so the encoder and decoder share
  the same implementation as upstream rsync, and checksum validation continues
  to rely on the upstream algorithms provided by `rsync_checksums`.
- `oc-rsyncd` and `rsyncd` are thin wrappers that invoke the client entrypoint
  with an implicit `--daemon`, mirroring the upstream symlink behaviour while
  remaining shell scripts in packaging contexts.
- Added regression coverage ensuring the fallback binary availability cache
  expires negative entries once the TTL elapses.
- Extended the bandwidth parser test suite with exponent and byte-suffix
  adjustment cases so `--bwlimit` parity with upstream `parse_size_arg()` stays
  locked down.
