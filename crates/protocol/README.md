# `protocol`

`protocol` implements the negotiation and multiplexing primitives required by the
Rust `rsync` implementation. The crate mirrors upstream rsync 3.4.1 behaviour so higher
layers can negotiate protocol versions, interpret legacy daemon banners, and exchange
multiplexed frames without depending on the original C sources.

## Design

The crate is decomposed into small modules that map onto the upstream architecture:

- `version` exposes [`ProtocolVersion`] and
  helpers for selecting the highest mutually supported protocol between peers.
- `legacy` provides parsers for ASCII daemon handshakes such as
  `@RSYNCD: 31.0` and the follow-up control messages emitted by rsync daemons prior to
  protocol 30.
- `negotiation` contains incremental sniffers that classify the
  handshake style (binary vs. legacy ASCII) without losing buffered bytes.
- `multiplex` and `envelope` re-create the
  control/data framing used once a session has been negotiated.
- `compatibility` models the post-negotiation compatibility flags
  shared by peers and exposes typed helpers for working with individual bits.
- `varint` reproduces rsync's variable-length integer codec so other
  modules can serialise the compatibility flags and future protocol values.

Each module satisfies the workspace style guide, while the crate root re-exports the
stable APIs consumed by the higher-level transport, core, and daemon layers.

## Invariants

- [`SUPPORTED_PROTOCOLS`] always lists protocol numbers in
  descending order (`32` through `28`).
- Legacy negotiation helpers never drop or duplicate bytes: sniffed prefixes can be
  replayed verbatim into the parsing routines.
- Multiplexed message headers clamp payload lengths to the 24-bit limit used by upstream
  rsync.

## Errors

Parsing helpers surface rich error types that carry enough context to reproduce upstream
diagnostics. For example,
[`NegotiationError`] distinguishes between malformed greetings,
unsupported protocol ranges, and truncated payloads. All error types implement
[`std::error::Error`] and convert into [`std::io::Error`] where appropriate so they
integrate naturally with transport code.

## Examples

Determine whether a buffered prologue belongs to the legacy ASCII greeting or the binary
negotiation. The helper behaves exactly like upstream rsync's `io.c:check_protok` logic by
classifying the session based on the first byte.

```rust
use protocol::{detect_negotiation_prologue, NegotiationPrologue};

assert_eq!(
    detect_negotiation_prologue(b"@RSYNCD: 30.0\n"),
    NegotiationPrologue::LegacyAscii
);
assert_eq!(
    detect_negotiation_prologue(&[0x00, 0x20, 0x00, 0x00]),
    NegotiationPrologue::Binary
);
```

Once the negotiation style is known, the highest mutually supported protocol can be
derived from the peer advertisement.

```rust
use protocol::{select_highest_mutual, ProtocolVersion};

let negotiated = select_highest_mutual([32, 31]).expect("mutual version exists");
assert_eq!(negotiated, ProtocolVersion::NEWEST);
```

When a peer selects the legacy ASCII negotiation, the bytes that triggered the decision
must be replayed into the greeting parser so the full `@RSYNCD:` line can be
reconstructed. [`NegotiationPrologueSniffer`] owns the
buffered prefix, allowing callers to reuse it without copying more data than upstream
rsync would have consumed.

```rust
use protocol::{
    NegotiationPrologue, NegotiationPrologueSniffer, parse_legacy_daemon_greeting,
};
use std::io::{Cursor, Read};

let mut reader = Cursor::new(&b"@RSYNCD: 31.0\n"[..]);
let mut sniffer = NegotiationPrologueSniffer::new();

let decision = sniffer
    .read_from(&mut reader)
    .expect("sniffing never fails for in-memory data");
assert_eq!(decision, NegotiationPrologue::LegacyAscii);

let mut prefix = Vec::new();
sniffer
    .take_buffered_into(&mut prefix)
    .expect("the vector has enough capacity for @RSYNCD:");
assert_eq!(prefix, b"@RSYNCD:");

let mut full_line = prefix;
reader.read_to_end(&mut full_line).expect("cursor read cannot fail");
assert_eq!(full_line, b"@RSYNCD: 31.0\n");
assert_eq!(
    parse_legacy_daemon_greeting(std::str::from_utf8(&full_line).unwrap())
        .expect("banner is well-formed"),
    protocol::ProtocolVersion::from_supported(31).unwrap()
);
```

## See also

- [`transport`](https://docs.rs/transport) for transport wrappers that reuse
  the sniffers and parsers exposed here.
- [`core`](https://docs.rs/core) for message formatting utilities that rely on
  negotiated protocol numbers.
