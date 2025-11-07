## Completed tasks

- Compression now uses the system zlib backend so the encoder and decoder share
  the same implementation as upstream rsync, and checksum validation continues
  to rely on the upstream algorithms provided by `oc_rsync_checksums`.
- Added a `legacy-binaries` feature so optional `oc-rsyncd`/`rsyncd` wrappers
  invoke the single binary with an implicit `--daemon`, mirroring upstream
  symlink behaviour without altering default packages.
- Added regression coverage ensuring the fallback binary availability cache
  expires negative entries once the TTL elapses.
- Extended the bandwidth parser test suite with exponent and byte-suffix
  adjustment cases so `--bwlimit` parity with upstream `parse_size_arg()` stays
  locked down.
