# rsync_checksums

`rsync_checksums` provides the rolling and strong checksum primitives used by the
Rust `rsync` implementation. The algorithms are byte-for-byte compatible with
upstream rsync 3.4.1 so delta-transfer heuristics and compatibility checks remain
interchangeable with the C reference implementation.

## Design

The crate currently offers two modules:

- The `rolling` module implements the Adler-32–style weak checksum (`rsum`) used
  for block matching during delta transfers.
- [`crate::strong`] exposes MD4, MD5, XXH64, and XXH3 (64- and 128-bit) digests
  together with the [`crate::strong::StrongDigest`] trait that higher layers use
  to abstract over the negotiated algorithm.
- When the crate is built with the default `openssl-vendored` feature (or the
  narrower `openssl` flag), the MD4 and MD5 wrappers transparently dispatch to
  OpenSSL's EVP implementations while retaining the pure-Rust fallback so the
  workspace can advertise `openssl-crypto` capability in the version banner.

The modules are intentionally small, allowing the workspace to enforce strict
layering while keeping checksum-specific optimisations in one place.

## Invariants

- [`RollingChecksum`] truncates both state components to 16 bits
  after every update, matching upstream rsync's behaviour.
- Rolling updates reject mismatched slice lengths and empty windows so the
  caller never observes silent state corruption.
- Strong digests stream data incrementally and never panic; they surface
  failures through the standard digest traits.

## Errors

- [`crate::RollingError`] reports invalid rolling operations (empty windows,
  window lengths that overflow `u32`, or mismatched slice lengths) and
  implements [`std::error::Error`] so the failure can be forwarded to
  user-facing diagnostics.
- [`crate::RollingSliceError`] signals that a digest could not be reconstructed
  from a byte slice because the input length differed from the expected four
  bytes.

## Examples

Compute a rolling checksum for a block and then advance the window.

```rust
use rsync_checksums::RollingChecksum;

let mut rolling = RollingChecksum::new();
rolling.update(b"abcd");
assert_eq!(rolling.len(), 4);

// Replace the first byte with `e` and observe that the helper succeeds.
rolling.roll(b'a', b'e').unwrap();
assert_eq!(rolling.len(), 4);
```

Calculate a strong checksum using the MD5 wrapper.

```rust
use rsync_checksums::strong::Md5;

let mut md5 = Md5::new();
md5.update(b"hello");
let digest = md5.finalize();
assert_eq!(
    digest,
    [
        0x5d, 0x41, 0x40, 0x2a, 0xbc, 0x4b, 0x2a, 0x76,
        0xb9, 0x71, 0x9d, 0x91, 0x10, 0x17, 0xc5, 0x92,
    ]
);
```

## See also

- [`rsync_protocol`](https://docs.rs/rsync-protocol) for the protocol version
  logic that selects the strong checksum variant used during negotiation.
- [`rsync_core`](https://docs.rs/rsync-core) for message formatting utilities
  that surface checksum mismatches to the user.
