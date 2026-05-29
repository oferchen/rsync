#!/usr/bin/env bash
set -euo pipefail
UP="/usr/bin/rsync"
OC="/workspace/target/release/oc-rsync"
PORT=18895
FIX="/tmp/mif7-fixture"
DST="/tmp/mif7-dest"
CONF="/tmp/mif7-rsyncd.conf"

pkill -f "mif7-rsyncd" 2>/dev/null || true
sleep 0.3
rm -rf "$DST" && mkdir -p "$DST" && chmod 777 "$DST"
$UP --daemon --config="$CONF" --no-detach &
DPID=$!
sleep 0.5

echo "=== TCP Segment Counts (1000 files, no latency) ==="

for CLIENT_LABEL in upstream oc-rsync; do
    if [ "$CLIENT_LABEL" = "upstream" ]; then
        CLIENT="$UP"
    else
        CLIENT="$OC"
    fi
    rm -rf "$DST"/*
    PCAP="/tmp/mif7-${CLIENT_LABEL}.pcap"
    rm -f "$PCAP"

    tcpdump -i lo -w "$PCAP" port "$PORT" 2>/dev/null &
    TCPID=$!
    sleep 0.3

    $CLIENT -a --itemize-changes "$FIX/" "rsync://127.0.0.1:${PORT}/bench/" >/dev/null 2>&1 || true
    sleep 0.3
    kill $TCPID 2>/dev/null || true
    wait $TCPID 2>/dev/null || true

    total=$(tcpdump -r "$PCAP" 2>/dev/null | wc -l | tr -d ' ')
    data=$(tcpdump -r "$PCAP" 2>/dev/null | grep -c "length [1-9]" || echo 0)
    c2s=$(tcpdump -r "$PCAP" 'dst port '"$PORT" 2>/dev/null | grep -c "length [1-9]" || echo 0)
    s2c=$(tcpdump -r "$PCAP" 'src port '"$PORT" 2>/dev/null | grep -c "length [1-9]" || echo 0)

    printf "%s: total=%s data=%s c2s=%s s2c=%s\n" "$CLIENT_LABEL" "$total" "$data" "$c2s" "$s2c"
done

kill $DPID 2>/dev/null || true
wait $DPID 2>/dev/null || true
echo "done"
