#!/bin/bash
# Capture handshake bytes using strace (no root required)
set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$WORKSPACE_ROOT"

UPSTREAM_BIN="target/interop/upstream-install/3.4.1/bin/rsync"
TEST_PORT=18873
OUTPUT_DIR="tests/protocol_handshakes"

PROTOCOL="${1:-28}"

if [ "$PROTOCOL" -le 29 ]; then
    output_subdir="${OUTPUT_DIR}/protocol_${PROTOCOL}_legacy"
else
    output_subdir="${OUTPUT_DIR}/protocol_${PROTOCOL}_binary"
fi

mkdir -p "$output_subdir"

echo "Capturing protocol $PROTOCOL handshake with strace..."

# Create daemon config
config="/tmp/strace-rsyncd-$$.conf"
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

# Run client with strace to capture I/O
strace_out="/tmp/strace-client-$$.log"
strace -e trace=write,read -s 200 -o "$strace_out" \
    "$UPSTREAM_BIN" --protocol="$PROTOCOL" -v rsync://localhost:$TEST_PORT/testmodule/ &> /dev/null || true

# Kill daemon
kill $daemon_pid 2>/dev/null || true
wait $daemon_pid 2>/dev/null || true

# Extract handshake from strace log
if [ -f "$strace_out" ]; then
    echo "  ✓ Captured strace to $strace_out"
    echo "  → Extracting handshake bytes..."

    # Save full strace log for manual analysis
    cp "$strace_out" "$output_subdir/strace_full.log"

    # Extract writes to fd 3 or 4 (usually the socket)
    echo "First 20 write() calls:"
    grep 'write(' "$strace_out" | head -20

    rm -f "$strace_out"
else
    echo "  ✗ Failed to capture strace"
    rm -f "$config"
    exit 1
fi

rm -f "$config"
echo ""
echo "Manual extraction needed from: $output_subdir/strace_full.log"
