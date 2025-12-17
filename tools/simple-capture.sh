#!/bin/bash
# Simple handshake capture without requiring sudo kill
set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$WORKSPACE_ROOT"

UPSTREAM_BIN="target/interop/upstream-install/3.4.1/bin/rsync"
TEST_PORT=18873
OUTPUT_DIR="tests/protocol_handshakes"

if [ ! -x "$UPSTREAM_BIN" ]; then
    echo "Error: Upstream rsync binary not found"
    exit 1
fi

PROTOCOL="${1:-28}"

if [ "$PROTOCOL" -le 29 ]; then
    output_subdir="${OUTPUT_DIR}/protocol_${PROTOCOL}_legacy"
else
    output_subdir="${OUTPUT_DIR}/protocol_${PROTOCOL}_binary"
fi

mkdir -p "$output_subdir"

echo "Capturing protocol $PROTOCOL handshake..."

# Create daemon config
config="/tmp/capture-rsyncd-$$.conf"
cat > "$config" << 'EOF'
[testmodule]
    path = /tmp/rsync-test-source
    comment = Test module
    use chroot = no
    read only = yes
    list = yes
EOF

# Create test directory
mkdir -p /tmp/rsync-test-source
echo "test" > /tmp/rsync-test-source/testfile.txt

# Start daemon
"$UPSTREAM_BIN" --daemon --no-detach --config="$config" --port=$TEST_PORT &
daemon_pid=$!
sleep 1

# Start tcpdump in background with timeout
pcap_file="/tmp/handshake-proto${PROTOCOL}-$$.pcap"
timeout 10 sudo tcpdump -i lo -w "$pcap_file" "port $TEST_PORT" &
tcpdump_pid=$!
sleep 0.5

# Connect with client
"$UPSTREAM_BIN" --protocol="$PROTOCOL" -v rsync://localhost:$TEST_PORT/testmodule/ &> /dev/null || true

sleep 1

# Wait for tcpdump timeout (or kill daemon to trigger tcpdump exit)
kill $daemon_pid 2>/dev/null || true
wait $tcpdump_pid 2>/dev/null || true
wait $daemon_pid 2>/dev/null || true

# Move pcap file
if [ -f "$pcap_file" ]; then
    mv "$pcap_file" "$output_subdir/handshake.pcap"
    echo "  ✓ Captured to $output_subdir/handshake.pcap"
    ls -lh "$output_subdir/handshake.pcap"
else
    echo "  ✗ Failed to capture pcap"
    rm -f "$config"
    exit 1
fi

rm -f "$config"
