# Per-File Wire Segment Count - Upstream vs OC-rsync Daemon (MIF-2)

Tracking: MIF-2
Status: Complete
Date: 2026-05-29

## Summary

tcpdump captures of daemon-mode transfers on loopback quantify the
per-file TCP segment overhead in oc-rsync v0.6.2 (with MIF-5 deferred
flush) compared to upstream rsync 3.4.1. The delta-sync scenario - where
MSG_INFO framing dominates over bulk file data - shows oc-rsync emitting
+733% more server-to-client data segments (100 vs 12 for 50 files).

The initial-sync scenario, dominated by large file payloads, shows only
+14% more segments (302 vs 265). The noop scenario (all files up to date)
shows +44% (13 vs 9). Client-to-server segment counts are identical
across all scenarios because the same upstream rsync 3.4.1 client is
used.

---

## 1. Test Environment

| Component | Version / Detail |
|-----------|-----------------|
| Container | `rsync-profile` (Debian, `rust:latest`) |
| Upstream rsync | 3.4.1, `/usr/bin/rsync` |
| OC-rsync | v0.6.2 rev `5d7328ed0`, `/usr/local/bin/oc-rsync` |
| MIF-5 status | Active - `requires_immediate_flush()` skips flush for `MSG_INFO`/`MSG_WARNING` |
| Interface | `lo` (loopback), IPv4 `127.0.0.1` |
| Capture tool | `tcpdump -i lo -w <file> port <port>` |
| Upstream daemon port | 18896 |
| OC-rsync daemon port | 18897 |
| Client | Upstream rsync 3.4.1 in all cases (isolates server-side framing) |

## 2. Test Workload

50 files across four size classes:

| Size class | Count | Per-file size | Subtotal |
|------------|------:|---------------|----------|
| 1 KB | 12 | 1,024 bytes | 12 KB |
| 10 KB | 13 | 10,240 bytes | 130 KB |
| 100 KB | 13 | 102,400 bytes | 1.3 MB |
| 1 MB | 12 | 1,048,576 bytes | 12.3 MB |
| **Total** | **50** | | **13.7 MB** |

Files contain random data (`/dev/urandom`). The mix of sizes stresses
both small-file framing overhead and large-file batching.

## 3. Scenarios

Three transfer scenarios, each run against both daemons:

1. **Initial sync** - destination empty, all 50 files transferred in full.
   Tests bulk-data throughput where MSG_INFO overhead is diluted by file
   payload.

2. **Delta sync** - destination populated from initial sync, then all files
   backdated (`touch -t 202501010000`). Quick-check detects timestamp
   mismatch, triggering signature exchange and delta transfer for each
   file. File content is identical so the delta is small, but each file
   generates protocol overhead (signatures, ack, itemize). This scenario
   maximizes the MSG_INFO-to-data ratio.

3. **Noop sync** - destination fully up to date. Quick-check matches on
   all files. No data or delta transfer occurs. Only handshake, file-list
   exchange, and goodbye frames appear on the wire.

## 4. Results

### 4.1 Aggregate Segment Counts

| Scenario | Direction | Upstream | OC-rsync | Delta |
|----------|-----------|----------|----------|-------|
| **Initial sync** | Total packets | 437 | 440 | +1% |
| | Data-bearing | 276 | 313 | +13% |
| | Server-to-client data | 265 | 302 | +14% |
| | Client-to-server data | 11 | 11 | 0% |
| | Server payload bytes | 14,066,239 | 13,999,880 | -0.5% |
| **Delta sync** | Total packets | 41 | 145 | +254% |
| | Data-bearing | 25 | 113 | +352% |
| | Server-to-client data | 12 | 100 | **+733%** |
| | Client-to-server data | 13 | 13 | 0% |
| | Server payload bytes | 60,855 | 74,893 | +23% |
| **Noop sync** | Total packets | 31 | 37 | +19% |
| | Data-bearing | 19 | 23 | +21% |
| | Server-to-client data | 9 | 13 | +44% |
| | Client-to-server data | 10 | 10 | 0% |
| | Server payload bytes | 1,377 | 1,379 | +0.1% |

### 4.2 Per-File Segment Counts

| Scenario | Upstream segs/file | OC-rsync segs/file | Ratio |
|----------|-------------------:|-------------------:|------:|
| Initial sync (S2C) | 5.30 | 6.04 | 1.14x |
| Delta sync (S2C) | 0.24 | 2.00 | 8.33x |
| Noop sync (S2C) | 0.18 | 0.26 | 1.44x |

In the delta-sync scenario, upstream batches all 50 files' delta responses
and itemize output into approximately 3 large segments (22,411 + 20,679 +
16,544 bytes), while oc-rsync sends each file as a pair of segments: one
for the delta data and one for the MSG_INFO itemize line.

### 4.3 Payload Size Distributions (Delta Sync, Server-to-Client)

#### Upstream rsync 3.4.1

```
7 x 0       (ACK-only)
1 x 2       (module negotiation)
1 x 4       (phase marker)
1 x 5       (phase marker)
1 x 6       (goodbye)
1 x 12      (checksum seed)
1 x 21      (stats)
1 x 36      (file list segment)
1 x 41      (greeting)
1 x 1094    (file list)
1 x 16544   (batched file data + itemize)
1 x 20679   (batched file data + itemize)
1 x 22411   (batched file data + itemize)
```

Three large segments carry the entire transfer payload for 50 files.
MSG_INFO frames for all 50 itemize lines are coalesced into these
segments alongside the MSG_DATA file payloads.

#### OC-rsync v0.6.2

```
10 x 0      (ACK-only)
 1 x 1      (extra protocol byte)
 1 x 2      (module negotiation)
 4 x 5      (phase markers - 2 extra vs upstream)
 1 x 6      (goodbye)
 1 x 12     (checksum seed)
18 x 32     (MSG_INFO itemize: 1KB + 1MB files)
11 x 33     (MSG_INFO itemize: 10KB files)
13 x 34     (MSG_INFO itemize: 100KB files)
 1 x 21     (stats)
 1 x 35     (file list segment)
 1 x 41     (greeting)
 6 x 375    (MSG_DATA delta: 1KB files)
12 x 543    (MSG_DATA delta: 10KB files)
13 x 831    (MSG_DATA delta: 100KB files)
12 x 4139   (MSG_DATA delta: 1MB files)
 1 x 609    (MSG_DATA partial batch)
 1 x 1082   (file list)
 1 x 2442   (MSG_DATA batched - 1KB files)
```

Each file generates two segments: one MSG_DATA segment with the delta
response, then one MSG_INFO segment with the itemize line. The 32-34
byte MSG_INFO segments contain a 4-byte multiplex header plus the
itemize text (e.g., `<f..t...... file_001_1k.dat\n` = 28 bytes + 4
byte header = 32 bytes).

## 5. Analysis

### 5.1 MIF-5 Impact Assessment

The MIF-5 deferred flush (`requires_immediate_flush()`) eliminated the
per-MSG_INFO `inner.flush()` call. Before MIF-5, each MSG_INFO frame
would have forced its own TCP segment via explicit flush. With MIF-5,
MSG_INFO frames are written to the underlying `BufWriter` without
flushing and should coalesce with adjacent writes.

However, the measurement shows that MSG_INFO frames still appear as
individual TCP segments. The reason is structural: `send_message()` in
`MultiplexWriter` still calls `flush_buffer()` before writing the
MSG_INFO frame. This flushes any buffered MSG_DATA, then writes the
MSG_INFO header+payload to the underlying writer. Since the MSG_DATA
was just flushed and the next write (another MSG_DATA for the next
file) has not yet occurred, the MSG_INFO payload sits alone in the
`BufWriter`. When the next MSG_DATA write arrives, it triggers another
`flush_buffer()` which flushes the previous MSG_INFO, creating a
separate TCP segment.

The sequence per file is:

1. `send_message(Info, itemize_line)` calls `flush_buffer()` -
   drains any buffered MSG_DATA as a TCP segment
2. `send_msg(Info, payload)` writes the MSG_INFO frame to BufWriter
3. No flush (MIF-5 deferred) - but MSG_INFO sits alone in BufWriter
4. Next file: `write()` calls `flush_buffer()` - drains the
   accumulated MSG_DATA, which triggers the BufWriter to flush,
   emitting the MSG_INFO from step 2 as its own segment
5. Repeat

MIF-5 eliminated the explicit flush after step 2, but the structural
`flush_buffer()` in step 1/4 still forces segment boundaries. The net
effect: MIF-5 reduced overhead from the MIF-1 predicted +140% to the
measured +733% in the delta scenario, which appears worse but is a
different measurement. MIF-1 counted MSG_INFO frames, not TCP segments.
The +733% measures TCP segments in a scenario that maximizes the
MSG_INFO-to-MSG_DATA ratio.

### 5.2 Root Cause: flush_buffer() Before Every send_message()

The `MultiplexWriter::send_message()` method calls `flush_buffer()`
before writing the control message to ensure message ordering (DATA
before INFO). This is correct for ordering but creates a forced
segment boundary. Upstream rsync avoids this by using a separate
`iobuf.msg` buffer for control messages - MSG_INFO frames accumulate
independently and drain alongside MSG_DATA during the next
`perform_io()` cycle.

The fix requires changing `send_message()` to buffer the MSG_INFO
header+payload into the same buffer as MSG_DATA, preserving ordering
by writing them in sequence rather than flushing between them. This
is MIF-3 (buffer MSG_INFO alongside MSG_DATA).

### 5.3 Where the Overhead Matters

| Scenario | Overhead | Impact |
|----------|----------|--------|
| Initial sync (large files) | +14% S2C segments | Negligible - file payload dominates |
| Delta sync (small deltas) | +733% S2C segments | Significant on WAN - each extra segment adds RTT |
| Noop sync (no transfer) | +44% S2C segments | Moderate - only handshake/protocol framing |
| Itemize-heavy (delete 1000 files) | Estimated +100x | Severe - each deletion produces one MSG_INFO segment |

The overhead is most visible in delta syncs of many small files over
high-latency links. For 1000 files over a 10ms WAN link, the extra
~2000 segments (2 per file instead of ~20 total) add approximately
20 seconds of cumulative RTT delay.

### 5.4 Payload Byte Overhead

Payload byte counts are comparable across both daemons:

- Initial sync: -0.5% (oc-rsync slightly smaller due to different
  file-list encoding)
- Delta sync: +23% (extra per-file MSG_INFO headers contribute 4
  bytes per frame of overhead that upstream avoids by batching)
- Noop sync: +0.1% (identical)

The per-file 4-byte MSG_INFO header overhead is small in absolute
terms but adds up: 50 files x 4 bytes = 200 bytes of extra framing
in the delta sync. The larger contributor to the +23% is the per-file
delta response framing.

## 6. Comparison with MIF-1 Predictions

| MIF-1 Prediction | Measured Result | Status |
|------------------|-----------------|--------|
| +140% wire segments (general) | +733% S2C in delta, +14% in initial | Delta is worse than predicted; initial is better |
| 2N MSG_INFO frames for N files | ~50 MSG_INFO segments for 50 files (1 per file, not 2) | Generator itemize not producing separate segments |
| Per-message flush is highest-impact site | Confirmed - structural flush_buffer() still forces segments | Partially addressed by MIF-5 |
| Upstream batches ~N/100 segments | Confirmed - upstream uses 3 segments for 50 files | Matches prediction |

MIF-1 predicted +200x overhead per batch (N segments vs N/100). The
measured ratio of 100/12 = 8.3x is lower because:
1. Only 1 MSG_INFO segment per file (not 2) - generator itemize
   appears to merge with MSG_DATA in some cases
2. Protocol overhead segments (greeting, negotiation, phase markers)
   are counted in both totals, diluting the ratio

## 7. Recommended Next Steps

| Priority | Task | Expected Impact |
|----------|------|-----------------|
| **P0** | MIF-3: Buffer MSG_INFO in MultiplexWriter alongside MSG_DATA | Eliminates per-file segment boundary; reduces delta S2C from 100 to ~15 |
| P1 | MIF-4: Batch deletion itemize lines | Reduces deletion scenario from N to 1 segment |
| P2 | MIF-7: Wire segment count regression test | Prevents regression in future changes |

MIF-5 (deferred flush) is necessary but not sufficient. The structural
`flush_buffer()` in `send_message()` must be replaced with in-buffer
MSG_INFO accumulation (MIF-3) to match upstream's batching behavior.

## 8. Reproduction

```bash
# Container: rsync-profile (podman)
# Create 50-file test workload
podman exec rsync-profile bash -c '
  mkdir -p /tmp/mif2_src
  idx=0
  for i in $(seq 1 12); do
    idx=$((idx + 1))
    dd if=/dev/urandom of=/tmp/mif2_src/file_$(printf "%03d" $idx)_1k.dat \
       bs=1024 count=1 2>/dev/null
  done
  for i in $(seq 1 13); do
    idx=$((idx + 1))
    dd if=/dev/urandom of=/tmp/mif2_src/file_$(printf "%03d" $idx)_10k.dat \
       bs=10240 count=1 2>/dev/null
  done
  for i in $(seq 1 13); do
    idx=$((idx + 1))
    dd if=/dev/urandom of=/tmp/mif2_src/file_$(printf "%03d" $idx)_100k.dat \
       bs=102400 count=1 2>/dev/null
  done
  for i in $(seq 1 12); do
    idx=$((idx + 1))
    dd if=/dev/urandom of=/tmp/mif2_src/file_$(printf "%03d" $idx)_1m.dat \
       bs=1048576 count=1 2>/dev/null
  done
'

# Start upstream daemon
podman exec rsync-profile bash -c '
  printf "port = 18896\nuse chroot = false\n\n[mif2src]\n    path = /tmp/mif2_src\n    read only = true\n    use chroot = false\n" > /tmp/rsyncd.conf
  rsync --daemon --no-detach --port=18896 --config=/tmp/rsyncd.conf &
  DPID=$!; sleep 2

  # Initial sync
  mkdir -p /tmp/dest
  tcpdump -i lo -w /tmp/upstream_initial.pcap port 18896 &
  TPID=$!; sleep 1
  rsync -av rsync://127.0.0.1:18896/mif2src/ /tmp/dest/
  sleep 2; kill $TPID; wait $TPID 2>/dev/null

  # Backdate for delta sync
  for f in /tmp/dest/*.dat; do touch -t 202501010000 "$f"; done

  # Delta sync
  tcpdump -i lo -w /tmp/upstream_delta.pcap port 18896 &
  TPID=$!; sleep 1
  rsync -av rsync://127.0.0.1:18896/mif2src/ /tmp/dest/
  sleep 2; kill $TPID; wait $TPID 2>/dev/null
  kill $DPID
'

# Analyze: count server-to-client data segments
podman exec rsync-profile bash -c '
  echo "Upstream initial S2C data segments:"
  tcpdump -r /tmp/upstream_initial.pcap -nn 2>/dev/null \
    | grep "\.18896 >" | grep "length [1-9]" | wc -l
  echo "Upstream delta S2C data segments:"
  tcpdump -r /tmp/upstream_delta.pcap -nn 2>/dev/null \
    | grep "\.18896 >" | grep "length [1-9]" | wc -l
'

# Repeat with oc-rsync daemon on port 18897 using same workload.
```
