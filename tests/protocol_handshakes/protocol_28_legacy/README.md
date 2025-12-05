# Protocol 28 Legacy ASCII Handshake Golden Files

This directory contains golden ASCII sequences for the protocol 28 legacy `@RSYNCD:` negotiation - the **oldest** protocol version supported by oc-rsync.

## Files

- `client_greeting.txt` - Client's ASCII greeting: `@RSYNCD: 28\n`
- `server_response.txt` - Server's ASCII response
- `module_exchange.txt` - Module listing or selection exchange

## Protocol 28 Legacy Negotiation

Protocol 28 uses the legacy ASCII `@RSYNCD:` format. It is the oldest protocol version in the supported range (28-32).

### Handshake Flow

1. **Client Greeting**: `@RSYNCD: 28\n`
   - No subprotocol suffix for protocol 28
   - May include digest list in some implementations

2. **Server Response**: `@RSYNCD: 28\n` or `@RSYNCD: 28.0\n`
   - May include digest capabilities

3. **Module Exchange**: ASCII module listing or authentication

## Capture Process

```bash
# Start upstream rsync 3.0.9 in protocol 28 mode
target/interop/upstream-install/3.0.9/bin/rsync --daemon --config=/tmp/test-rsyncd.conf

# Capture with netcat
nc localhost 873 | tee server_response.txt
# Type: @RSYNCD: 28
```

## ASCII Format

Protocol 28 greetings:

```
@RSYNCD: 28\n
```

Simpler than protocol 29:
- No mandatory subprotocol
- Fewer digest options
- Basic ASCII line-based negotiation

## Validation

Tests verify:
- Correct ASCII prefix
- Version number 28
- Line termination
- Compatibility with oldest supported protocol

## Historical Context

Protocol 28 represents the earliest rsync protocol version that oc-rsync maintains compatibility with. Earlier protocols (< 28) are explicitly unsupported.

## References

- Upstream rsync 3.0.x source: `clientserver.c`
- `OLDEST_SUPPORTED_PROTOCOL = 28` in `crates/protocol/src/version/constants.rs`
