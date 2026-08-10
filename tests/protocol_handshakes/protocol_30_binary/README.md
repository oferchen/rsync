# Protocol 30 Binary Handshake Golden Files

This directory contains golden byte sequences for the protocol 30 binary negotiation handshake, the first protocol to introduce binary negotiation.

## Files

- `client_hello.bin` - Initial client handshake message
- `server_response.bin` - Server's response to client hello

## Protocol 30 Significance

Protocol 30 marks the transition from legacy ASCII `@RSYNCD:` negotiation to binary handshakes. This is the earliest protocol version that uses:

- Binary varint encoding for version numbers
- Compatibility flags
- Structured binary negotiation messages

## Capture Process

```bash
# Start upstream rsync 3.1.3 daemon (protocol 30 default)
target/interop/upstream-install/3.1.3/bin/rsync --daemon --config=/tmp/test-rsyncd.conf

# Capture with tcpdump
sudo tcpdump -i lo -w protocol_30_handshake.pcap 'port 873'

# Connect with explicit protocol 30
target/interop/upstream-install/3.1.3/bin/rsync --protocol=30 rsync://localhost/test/
```

## Wire Format

Protocol 30 introduced:
- Varint-encoded protocol version lists
- Compatibility flags bitfield
- Binary capability negotiation

Unlike protocols 28-29, protocol 30 does NOT use ASCII `@RSYNCD:` prefix for daemon connections.

## Validation

Tests in `crates/protocol/tests/golden_handshakes.rs` verify that our protocol 30 implementation produces byte-identical handshakes to upstream rsync.
