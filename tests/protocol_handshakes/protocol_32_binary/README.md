# Protocol 32 Binary Handshake Golden Files

This directory contains golden byte sequences for the protocol 32 binary negotiation handshake, captured from upstream rsync 3.4.1.

## Files

- `client_hello.bin` - Initial client handshake message
- `server_response.bin` - Server's response to client hello
- `compatibility_exchange.bin` - Compatibility flags exchange (if applicable)

## Protocol 32 Handshake Flow

Protocol 32 uses the binary negotiation introduced in protocol 30:

1. **Client Hello**: Client sends a binary message containing:
   - Supported protocol versions (varint encoded list)
   - Optional capability flags

2. **Server Response**: Server responds with:
   - Selected protocol version
   - Server capabilities

3. **Compatibility Exchange**: Both sides exchange compatibility flags via varint encoding

## Capture Process

### Using tcpdump

```bash
# Start rsync daemon on port 873
target/interop/upstream-install/3.4.1/bin/rsync --daemon --config=/tmp/test-rsyncd.conf

# Capture handshake traffic
sudo tcpdump -i lo -w protocol_32_handshake.pcap 'port 873'

# In another terminal, connect with rsync client
target/interop/upstream-install/3.4.1/bin/rsync --protocol=32 rsync://localhost/test/

# Stop tcpdump (Ctrl+C)

# Extract binary sequences from pcap using tshark or wireshark
tshark -r protocol_32_handshake.pcap -T fields -e data -Y 'tcp.port==873' | \
    xxd -r -p > handshake_raw.bin
```

### Using strace

```bash
# Capture client-side handshake
strace -f -e write -s 1000 -o client_trace.txt \
    target/interop/upstream-install/3.4.1/bin/rsync --protocol=32 rsync://localhost/test/

# Extract binary data from strace output
grep 'write.*"' client_trace.txt
```

## Wire Format

Protocol 32 binary handshake uses:
- **Varint encoding** for protocol versions and lengths
- **Little-endian** for multi-byte integers where applicable
- **Compatibility flags** as varint-encoded bitfield

## Validation

Golden files are validated in `crates/protocol/tests/golden_handshakes.rs`:

```rust
#[test]
fn test_protocol_32_client_hello_matches_golden() {
    let golden = include_bytes!("../../../tests/protocol_handshakes/protocol_32_binary/client_hello.bin");
    let generated = generate_client_hello(ProtocolVersion::V32);
    assert_eq!(generated, golden, "Protocol 32 handshake drift detected");
}
```

## References

- `rsync.h`: `PROTOCOL_VERSION`, `SUBPROTOCOL_VERSION`
- `compat.c`: Protocol negotiation and compatibility flags
- `io.c`: Wire format encoding/decoding
