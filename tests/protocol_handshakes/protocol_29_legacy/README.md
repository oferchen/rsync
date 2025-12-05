# Protocol 29 Legacy ASCII Handshake Golden Files

This directory contains golden ASCII sequences for the protocol 29 legacy `@RSYNCD:` negotiation.

## Files

- `client_greeting.txt` - Client's ASCII greeting: `@RSYNCD: 29\n` or `@RSYNCD: 29.0\n`
- `server_response.txt` - Server's ASCII response
- `module_exchange.txt` - Module listing or selection exchange

## Protocol 29 Legacy Negotiation

Protocol 29 is the **newest** protocol version to use the legacy ASCII `@RSYNCD:` negotiation format:

1. **Client Greeting**: `@RSYNCD: 29\n` or `@RSYNCD: 29.0\n`
   - May include digest list: `@RSYNCD: 29.0 md4 md5\n`

2. **Server Response**: `@RSYNCD: 29.0 md4 md5\n`
   - Lists supported authentication digests

3. **Module Exchange**:
   - Client requests module list or specific module
   - Server responds with module info or authentication challenge

## Capture Process

```bash
# Start upstream rsync 3.0.9 daemon (protocol 29 compatible)
target/interop/upstream-install/3.0.9/bin/rsync --daemon --config=/tmp/test-rsyncd.conf

# Capture with netcat to see raw ASCII
nc localhost 873 | tee server_response.txt
# Type: @RSYNCD: 29
# (server responds with @RSYNCD: 29.0 md4 md5)

# Or capture with tcpdump and extract ASCII
sudo tcpdump -i lo -A -w protocol_29_handshake.pcap 'port 873'
```

## ASCII Format

Legacy greetings follow this format:

```
@RSYNCD: <version>[.<subprotocol>][ <digest1> <digest2> ...]\n
```

Examples:
- `@RSYNCD: 29\n` - Basic protocol 29
- `@RSYNCD: 29.0\n` - Protocol 29 with explicit subprotocol 0
- `@RSYNCD: 29.0 md4 md5\n` - With digest list

## Validation

Tests verify:
- Correct ASCII prefix (`@RSYNCD:`)
- Version number parsing (29)
- Optional subprotocol handling (`.0`)
- Digest list parsing (`md4 md5`)
- Line termination (`\n`)

## References

- `clientserver.c`: `send_daemon_greeting()`, `recv_daemon_greeting()`
- Protocol specification: Legacy ASCII negotiation format
