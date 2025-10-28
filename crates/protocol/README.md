# rsync-protocol

`rsync-protocol` implements the negotiation and multiplexing primitives required
by the Rust `rsync` implementation. The crate mirrors upstream rsync 3.4.1
behaviour so higher layers can negotiate protocol versions, interpret legacy
daemon banners, and exchange multiplexed frames without depending on the
original C sources.

## Design

The crate is decomposed into small modules that track the upstream layout:

- The `version` module exposes the protocol enumeration and helpers for
  selecting the highest mutually supported protocol between peers.
- The `legacy` module provides parsers for ASCII daemon handshakes and the
  follow-up control messages emitted by classic rsync daemons.
- The `negotiation` module contains incremental sniffers that classify the
  handshake style without losing buffered bytes.
- The `multiplex` and `envelope` modules recreate the control/data framing used
  once a session has been negotiated.
- The `compatibility` module models post-negotiation compatibility flags and
  exposes typed helpers for working with individual bits.
- The `varint` module reproduces rsync's variable-length integer codec so other
  modules can serialise compatibility flags and protocol values.

The crate root re-exports the stable APIs consumed by the higher-level
transport, core, and daemon layers.

## Invariants

- Supported protocol numbers are listed in descending order (`32` through `28`).
- Legacy negotiation helpers never drop or duplicate bytes, allowing sniffed
  prefixes to be replayed verbatim into the parsing routines.
- Multiplexed message headers clamp payload lengths to the 24-bit limit used by
  upstream rsync.

## Errors

Parsing helpers surface error types that carry enough context to reproduce
upstream diagnostics. Errors implement `std::error::Error` and convert into
`std::io::Error` where appropriate so they integrate naturally with transport
code.

## Examples

Determine whether a buffered prologue belongs to the legacy ASCII greeting or
binary negotiation:

```rust
use rsync_protocol::{detect_negotiation_prologue, NegotiationPrologue};

assert_eq!(
    detect_negotiation_prologue(b"@RSYNCD: 30.0\n"),
    NegotiationPrologue::LegacyAscii
);
assert_eq!(
    detect_negotiation_prologue(&[0x00, 0x20, 0x00, 0x00]),
    NegotiationPrologue::Binary
);
```

## See also

- `rsync-transport` for wrappers that reuse the sniffers and parsers exposed
  here.
- `rsync-core` for message formatting utilities that rely on negotiated
  protocol numbers.
