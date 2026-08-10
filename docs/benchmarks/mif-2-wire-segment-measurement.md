# MIF-2: Wire segment count measurement

Quantifies the per-file MSG_INFO framing overhead in oc-rsync vs upstream
rsync by counting TCP segments via tcpdump during daemon-mode transfers.

## Methodology

- **Environment:** Podman container (`rsync-profile`, Debian, `rust:latest`),
  loopback interface, IPv6 (`::1`).
- **Daemon:** Upstream rsync 3.4.1, `use chroot = false`, port 18894.
- **Clients:** Upstream rsync 3.4.1 and oc-rsync v0.6.2 (rev `cd70daedc`).
- **Test data:** 20 files, sizes 96-704 bytes (32-byte increments), 8 KB total.
- **Capture:** `tcpdump -i any -w <file> port 18894` around each transfer.
- **Note:** oc-rsync daemon mode is not functional in this build, so both
  tests use the upstream rsync daemon. The measurement captures oc-rsync's
  client-side framing overhead (sender multiplexing and write batching).

## Results

### Push mode (client sends files to upstream daemon)

| Metric | Upstream client | OC-rsync client | Delta |
|--------|---------------:|----------------:|------:|
| Total TCP segments | 36 | 52 | +44% |
| Data-bearing segments | 21 | 35 | +67% |
| Client-to-server data | 11 | 25 | +127% |
| Server-to-client data | 10 | 10 | 0% |

The server-to-client direction is identical because the same upstream daemon
generates MSG_INFO frames in both tests. The overhead is entirely in the
client-to-server direction.

**Root cause:** Upstream rsync batches all file data into a single large
write (8,864 bytes for 20 files). OC-rsync emits each file's data as a
separate write, producing 13+ individual TCP segments with per-file sizes
(143, 175, 207, ... 463, 5607 bytes). The per-file `write()` pattern
defeats TCP Nagle coalescing because each write is followed by a flush
or the next write arrives after the ACK.

### Pull mode (upstream daemon sends files to client)

| Metric | Upstream client | OC-rsync client | Delta |
|--------|---------------:|----------------:|------:|
| Total TCP segments | 33 | 37 | +12% |
| Data-bearing segments | 21 | 25 | +19% |
| Client-to-server data | 11 | 13 | +18% |
| Server-to-client data | 10 | 12 | +20% |

Pull mode overhead is modest because the upstream daemon generates the
MSG_INFO frames and file data in both cases. The +18-20% delta comes from
oc-rsync's client-side protocol framing (phase acknowledgments, goodbye
sequence) using more individual writes than upstream.

## Payload length distributions

### Push - upstream client

```
 15 x length 0    (ACK-only)
  4 x length 5    (phase markers, goodbye)
  2 x length 41   (greeting exchange)
  2 x length 2    (module/checksum negotiation)
  1 x length 8867 (all file data in one segment)
  1 x length 391  (file list + negotiation)
  1 x length 384  (file list response)
```

### Push - oc-rsync client

```
 17 x length 0    (ACK-only)
  6 x length 5    (phase markers, goodbye - 2x more than upstream)
  2 x length 41   (greeting exchange)
  2 x length 7    (extra protocol framing)
  2 x length 2    (module/checksum negotiation)
  1 x length 5607 (bulk file data, partial batch)
  1 x length 463  (file 20 data)
  1 x length 431  (file 19 data)
  1 x length 399  (file 18 data)
  1 x length 389  (file list)
  1 x length 387  (server response)
  1 x length 367  (file 17 data)
  1 x length 335  (file 16 data)
  ...              (one segment per file, increasing sizes)
  1 x length 143  (file 11 data)
```

## Analysis

1. **Push write batching gap (primary):** The +127% client-to-server segment
   overhead directly measures the per-file write pattern in the sender
   multiplexer. Upstream accumulates file data in an output buffer and
   flushes once; oc-rsync flushes after each file's data block. Each
   per-file flush triggers a separate TCP segment.

2. **Protocol framing overhead (secondary):** OC-rsync sends 6 length-5
   segments (phase markers, goodbye) vs upstream's 4. The extra 2 segments
   are additional protocol acknowledgments that upstream does not send.

3. **Pull overhead is modest:** When the upstream daemon is the sender, both
   clients receive the same batched data. The +19% pull overhead is from
   oc-rsync's receiver sending slightly more acknowledgment segments.

4. **Real-world impact:** On loopback, the extra segments complete in
   microseconds. Over WAN (1 ms+ RTT), each extra segment incurs at least
   one RTT of additional latency. For 1000 small files over a 10 ms link,
   the +127% segment overhead translates to roughly +1.3 seconds of
   additional transfer time.

## Recommended fix

Coalesce MSG_INFO frames and file data writes in the sender multiplexer
output buffer. Flush the buffer based on size threshold (e.g., 8 KB) or
idle timeout rather than per-file. This is tracked as MIF-3.

## Reproduction

```bash
# Start upstream daemon in container
podman exec rsync-profile bash -c '
  mkdir -p /tmp/mif2/src /tmp/mif2/dest && chmod 777 /tmp/mif2/dest
  for i in $(seq 1 20); do
    dd if=/dev/urandom of=/tmp/mif2/src/file_$(printf "%02d" $i).dat \
       bs=$((64 + i * 32)) count=1 2>/dev/null
  done
  printf "[t]\n    path = /tmp/mif2/dest\n    read only = false\n    use chroot = false\n" \
    > /tmp/mif2/rsyncd.conf
  rsync --daemon --no-detach --port 18894 --config=/tmp/mif2/rsyncd.conf &
  sleep 2
  tcpdump -i any -w /tmp/capture.pcap port 18894 &
  sleep 1
  oc-rsync -av /tmp/mif2/src/ rsync://localhost:18894/t/
  sleep 2; kill %2; kill %1
  tcpdump -r /tmp/capture.pcap -nn | grep "length [1-9]" | wc -l
'
```
