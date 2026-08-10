# rsync_io

Transport adapters for the rsync implementation - handles SSH, TCP daemon, and
binary protocol negotiation while preserving buffered bytes across handshakes.

## Key Public Types

- `SessionHandshake` / `SessionHandshakeParts` - high-level session negotiation entry point
- `NegotiatedStream` - stream wrapper that replays sniffed prologue bytes
- `BinaryHandshake` - binary protocol (direct rsync-over-TCP) handshake
- `LegacyDaemonHandshake` - `@RSYNCD:` ASCII daemon protocol handshake
- `SshCommand` - SSH subprocess builder with compression detection
- `SshConnection` - established SSH connection with read/write split

## Key Functions

- `sniff_negotiation_stream` - classifies prologue as binary or legacy ASCII
- `negotiate_session` - top-level session negotiation (auto-detects transport)
- `parse_remote_shell` - parses remote shell specification into command parts

## Modules

- `binary` - binary protocol handshake
- `daemon` - legacy ASCII daemon handshake
- `negotiation` - stream sniffing and classification
- `session` - unified session negotiation facade
- `ssh` - SSH transport (subprocess and embedded russh)
- `channel_adapter` - tokio mpsc to AsyncRead/AsyncWrite bridge (async-ssh feature)

## Dependencies

- **Upstream:** `protocol` (wire format), `logging`, `memchr`, `shell-words`
- **Downstream:** `core` (orchestration)

## Features

- `embedded-ssh` - in-process SSH via russh (no subprocess)
- `async-ssh` - tokio-backed async SSH transport
- `ssh-config-parse` - reads `~/.ssh/config` for compression detection (default)
- `ssh-socketpair-stderr` - async stderr drain via Unix socketpair
