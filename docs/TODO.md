## Completed tasks

- Compression now uses the system zlib backend so the encoder and decoder share
  the same implementation as upstream rsync, and checksum validation continues
  to rely on the upstream algorithms provided by `oc_rsync_checksums`.
- Retired the `legacy-binaries` feature so packaging always ships a single
  `oc-rsync` binary while still allowing administrators to provide their own
  compatibility symlinks when necessary.
- Added regression coverage ensuring the fallback binary availability cache
  expires negative entries once the TTL elapses.
- Extended the bandwidth parser test suite with exponent and byte-suffix
  adjustment cases so `--bwlimit` parity with upstream `parse_size_arg()` stays
  locked down.
