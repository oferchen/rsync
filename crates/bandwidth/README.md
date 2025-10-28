# rsync-bandwidth

`rsync-bandwidth` centralises the parsing and pacing logic used by the Rust
`rsync` implementation when honouring `--bwlimit`. Higher level crates reuse
these helpers so both client and daemon roles share identical validation and
throttling behaviour.

## Features

- Textual limit parsing with upstream-compatible syntax including
  binary/decimal suffixes, fractional values, leading signs, and optional
  `+1`/`-1` adjustments.
- Token-bucket pacing that mirrors upstream rsync's behaviour, including
  burst-handling and minimum sleep intervals.
- Deterministic testing support that records requested sleep durations instead
  of touching the system clock, keeping unit tests fast and reproducible.

## Design overview

Parsing helpers accept textual bandwidth specifications and return either an
optional byte limit or a parse error. Daemon-style `RATE[:BURST]` combinations
are handled as well, producing structures that can be forwarded to pacing
components.

The pacing helpers implement the token-bucket scheduler used by the transfer
engine. They track accumulated debt, cap burst sizes, and sleep long enough to
honour the configured rate. Optional burst parameters clamp the debt just like
upstream rsync.

With the `test-support` feature enabled, helper functions record sleep requests
rather than calling `std::thread::sleep`. Tests can snapshot, inspect, and reset
captured durations while running in parallel without races.

## Example

```rust
use rsync_bandwidth::{parse_bandwidth_argument, BandwidthLimiter};
use std::num::NonZeroU64;

let limit = parse_bandwidth_argument("8M").expect("valid limit")
    .expect("non-zero limit");
let mut limiter = BandwidthLimiter::new(limit);
let chunk = limiter.recommended_read_size(1 << 20);
assert!(chunk <= 1 << 20);
limiter.register(chunk);
```

The example mirrors how higher layers throttle outgoing writes. The limiter
keeps the observed throughput at or below 8 MiB/s while coalescing smaller
chunks to reduce context switches.

## See also

- `rsync-core` and `rsync-daemon` for integration points that orchestrate
  parsing, transport, and pacing.
- `rsync-protocol` for message framing and version negotiation.
