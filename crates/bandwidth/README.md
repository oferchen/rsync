# rsync-bandwidth

`rsync-bandwidth` centralises the parsing and pacing logic that backs
[`rsync`'s `--bwlimit` option](https://download.samba.org/pub/rsync/rsync.html).
Higher level crates reuse the utilities exposed here so both the client and
daemon share identical validation and throttling behaviour.

## Features

- Textual limit parsing with upstream-compatible syntax including
  binary/decimal suffixes, fractional values, leading signs, and optional
  `+1` / `-1` adjustments.
- Token-bucket pacing that mirrors upstream rsync's shape, including
  burst-handling and minimum sleep intervals.
- Deterministic testing support that records requested sleep durations in lieu
  of touching the system clock, keeping unit tests fast and reproducible.

## Design Overview

### Parsing helpers

[`parse::parse_bandwidth_argument`](crate::parse::parse_bandwidth_argument)
accepts textual bandwidth specifications and returns either an optional byte
limit or a [`BandwidthParseError`](crate::parse::BandwidthParseError). The
helper trims ASCII whitespace, validates suffixes, applies the `+1` / `-1`
adjustments accepted by upstream rsync, and rounds to the nearest 1024 bytes per
second as the C implementation does.

[`parse::parse_bandwidth_limit`](crate::parse::parse_bandwidth_limit) extends the
behaviour to parse daemon-style `RATE[:BURST]` combinations. The returned
[`BandwidthLimitComponents`](crate::parse::BandwidthLimitComponents) struct can
be converted into a [`BandwidthLimiter`](crate::BandwidthLimiter) or stored for
later negotiation.

### Pacing helpers

[`BandwidthLimiter`](crate::BandwidthLimiter) implements the token-bucket
scheduler used by the transfer engine and daemon. It tracks accumulated debt,
limits write sizes, and sleeps long enough to honour the configured rate. When
the optional burst parameter is supplied the limiter caps the debt to the burst
size, mirroring upstream behaviour.

`apply_effective_limit` merges daemon-imposed caps with pre-existing limiter
configuration. The helper ensures precedence rules match upstream rsync by
keeping the strictest rate while allowing burst overrides when explicitly
requested, and returns a [`LimiterChange`](crate::LimiterChange) describing
whether throttling was enabled, updated, disabled, or left untouched.

### Test support

Enabling the `test-support` feature exposes helpers that record sleep requests
instead of calling [`std::thread::sleep`]. Tests obtain a
[`recorded_sleep_session`](crate::recorded_sleep_session) guard to serialise
access to the captured durations, ensuring race-free assertions when scenarios
run in parallel.

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

- [`rsync-core`](https://docs.rs/rsync-core/) and
  [`rsync-daemon`](https://docs.rs/rsync-daemon/) for integration points that
  orchestrate parsing, transport, and pacing.
- [`rsync-protocol`](https://docs.rs/rsync-protocol/) for message framing and
  version negotiation.
