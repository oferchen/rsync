## Completed tasks

- Compression now uses the system zlib backend so the encoder and decoder share
  the same implementation as upstream rsync, and checksum validation continues
  to rely on the upstream algorithms provided by `checksums`.
- Removed the `legacy-binaries` feature and the optional `oc-rsyncd`/`rsyncd`
  wrappers so the workspace now ships only the unified `oc-rsync` binary while
  leaving compatibility symlinks to downstream packaging.
- Added regression coverage ensuring the fallback binary availability cache
  expires negative entries once the TTL elapses.
- Extended the bandwidth parser test suite with exponent and byte-suffix
  adjustment cases so `--bwlimit` parity with upstream `parse_size_arg()` stays
  locked down.
