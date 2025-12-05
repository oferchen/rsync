# Protocol Handshake Golden Files

This directory contains golden byte sequences captured from upstream rsync implementations, used to validate wire-level protocol compatibility.

## Directory Structure

```
protocol_handshakes/
├── protocol_32_binary/    # Protocol 32 (newest) - binary negotiation
├── protocol_31_binary/    # Protocol 31 - binary negotiation
├── protocol_30_binary/    # Protocol 30 - first binary negotiation
├── protocol_29_legacy/    # Protocol 29 - last ASCII negotiation
└── protocol_28_legacy/    # Protocol 28 (oldest) - ASCII negotiation
```

## Protocol Negotiation Evolution

### Binary Negotiation (Protocols 30-32)

Introduced in protocol 30:
- Varint-encoded version lists
- Binary compatibility flags
- Structured negotiation messages

### Legacy ASCII Negotiation (Protocols 28-29)

Legacy `@RSYNCD:` format:
- ASCII greeting: `@RSYNCD: <version>\n`
- Line-based text protocol
- Optional digest list

## Capturing Golden Files

### Prerequisites

- Upstream rsync binaries in `target/interop/upstream-install/`
- Network capture tools (`tcpdump`, `tshark`, or `wireshark`)
- Root access for network capture (or `CAP_NET_RAW` capability)

### Capture Workflow

1. **Start upstream rsync daemon**:
   ```bash
   target/interop/upstream-install/3.4.1/bin/rsync --daemon \
       --config=/tmp/test-rsyncd.conf \
       --port=8873
   ```

2. **Capture network traffic**:
   ```bash
   sudo tcpdump -i lo -w handshake.pcap 'port 8873'
   ```

3. **Connect with rsync client**:
   ```bash
   target/interop/upstream-install/3.4.1/bin/rsync \
       --protocol=32 rsync://localhost:8873/test/
   ```

4. **Extract binary sequences** from pcap file

5. **Save as golden files** in appropriate protocol directory

### Automated Capture

For automated golden file generation:

```bash
cargo xtask capture-handshakes --protocol=all --output=tests/protocol_handshakes/
```

(Note: This command requires implementation in `xtask`)

## Validation

Golden files are validated in `crates/protocol/tests/golden_handshakes.rs`:

- Binary files compared byte-for-byte
- ASCII files validated line-by-line
- Protocol-specific format checks

### Test Execution

```bash
# Run all golden handshake tests
cargo test -p protocol --test golden_handshakes

# Run specific protocol tests
cargo test -p protocol --test golden_handshakes -- protocol_32
```

## Updating Golden Files

Golden files should be updated when:

1. Upstream rsync releases new protocol versions
2. Protocol negotiation logic changes in oc-rsync
3. Wire format compatibility issues are discovered

### Update Process

1. Capture new handshakes from upstream rsync
2. Compare with existing golden files
3. Document any differences
4. Update golden files if changes are intentional
5. Fix oc-rsync implementation if differences are bugs

## CI Integration

Golden handshake validation is part of the CI protocol test suite:

```yaml
- name: Protocol Handshake Validation
  run: |
    cargo test -p protocol --test golden_handshakes
    cargo test -p protocol --test version_negotiation
```

## References

- Upstream rsync source: `clientserver.c`, `compat.c`, `io.c`
- Protocol specification: Protocol negotiation and handshake formats
- oc-rsync implementation: `crates/protocol/src/negotiation/`
