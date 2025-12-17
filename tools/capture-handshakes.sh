#!/bin/bash
# Capture protocol handshake golden files from upstream rsync
#
# Usage: bash tools/capture-handshakes.sh [protocol_version]
#
# This script:
# 1. Starts an upstream rsync daemon
# 2. Captures network traffic
# 3. Connects with an upstream client
# 4. Extracts handshake bytes from pcap
# 5. Saves as golden files

set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$WORKSPACE_ROOT"

# Configuration
UPSTREAM_VERSION="${UPSTREAM_VERSION:-3.4.1}"
UPSTREAM_BIN="target/interop/upstream-install/${UPSTREAM_VERSION}/bin/rsync"
TEST_PORT=18873
CAPTURE_IFACE="${CAPTURE_IFACE:-lo}"
OUTPUT_DIR="tests/protocol_handshakes"

# Check prerequisites
if [ ! -x "$UPSTREAM_BIN" ]; then
    echo "Error: Upstream rsync binary not found at $UPSTREAM_BIN"
    echo "Run interop test setup first to build upstream binaries."
    exit 1
fi

if ! command -v tcpdump &> /dev/null; then
    echo "Error: tcpdump not found. Install with: sudo apt install tcpdump"
    exit 1
fi

if ! command -v tshark &> /dev/null; then
    echo "Warning: tshark not found. Install for better pcap extraction: sudo apt install tshark"
fi

# Protocol to capture (28-32, or "all")
PROTOCOL="${1:-all}"

echo "=== rsync Protocol Handshake Capture ==="
echo "Upstream version: $UPSTREAM_VERSION"
echo "Protocol(s): $PROTOCOL"
echo "Output directory: $OUTPUT_DIR"
echo ""

# Function to capture handshake for a specific protocol
capture_protocol() {
    local proto=$1
    local legacy_mode=false

    # Protocols 28-29 use legacy ASCII negotiation
    if [ "$proto" -le 29 ]; then
        legacy_mode=true
        output_subdir="${OUTPUT_DIR}/protocol_${proto}_legacy"
    else
        output_subdir="${OUTPUT_DIR}/protocol_${proto}_binary"
    fi

    mkdir -p "$output_subdir"

    echo "Capturing protocol $proto handshake..."

    # Create temporary daemon config
    local config="/tmp/capture-rsyncd-$$.conf"
    cat > "$config" << EOF
[testmodule]
    path = /tmp/rsync-test-source
    comment = Test module for handshake capture
    use chroot = no
    read only = yes
    list = yes
EOF

    # Ensure test directory exists
    mkdir -p /tmp/rsync-test-source
    echo "test content" > /tmp/rsync-test-source/testfile.txt

    # Start daemon in background
    local daemon_pid
    "$UPSTREAM_BIN" --daemon --no-detach --config="$config" --port=$TEST_PORT &> /dev/null &
    daemon_pid=$!
    sleep 1

    # Check if daemon started
    if ! kill -0 $daemon_pid 2>/dev/null; then
        echo "Error: Daemon failed to start"
        rm -f "$config"
        return 1
    fi

    # Start packet capture in background
    local pcap_file="/tmp/handshake-proto${proto}-$$.pcap"
    sudo tcpdump -i "$CAPTURE_IFACE" -w "$pcap_file" "port $TEST_PORT" &> /dev/null &
    local tcpdump_pid=$!
    sleep 0.5

    # Connect with client to trigger handshake
    "$UPSTREAM_BIN" --protocol="$proto" -v rsync://localhost:$TEST_PORT/testmodule/ &> /dev/null || true

    sleep 0.5

    # Stop capture and daemon
    sudo kill $tcpdump_pid 2>/dev/null || true
    kill $daemon_pid 2>/dev/null || true
    wait $daemon_pid 2>/dev/null || true
    wait $tcpdump_pid 2>/dev/null || true

    # Extract handshake data from pcap
    # For now, just save the pcap - manual extraction will be needed
    mv "$pcap_file" "$output_subdir/handshake.pcap"

    echo "  ✓ Captured to $output_subdir/handshake.pcap"
    echo "  → Manual extraction required - see README.md"

    rm -f "$config"
}

# Capture specified protocol(s)
if [ "$PROTOCOL" = "all" ]; then
    for proto in 28 29 30 31 32; do
        capture_protocol $proto
        echo ""
    done
else
    capture_protocol "$PROTOCOL"
fi

echo ""
echo "=== Capture Complete ==="
echo ""
echo "Next steps:"
echo "1. Extract handshake bytes from pcap files using tshark/wireshark"
echo "2. Save as .txt (protocols 28-29) or .bin (protocols 30-32)"
echo "3. Update golden test files"
echo ""
echo "For automated extraction, run:"
echo "  cargo xtask extract-handshakes"
echo " (Not yet implemented - manual extraction required for now)"
