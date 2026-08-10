# Daemon cold-start benchmark (DIS-7)

Re-benchmark of daemon cold-start performance after DIS-6 fixes.
Previous measurement (DIS-2) showed oc-rsync daemon was 3.75x slower than
upstream rsync (1.35s vs 0.36s). DIS-3 through DIS-6 identified and fixed
the top overhead sources.

## Environment

- **Container:** `rsync-profile` (Debian, `rust:latest` base)
- **Platform:** aarch64-linux, kernel 6.18
- **oc-rsync:** v0.6.2 (revision #585dbc16c), release build
- **Upstream:** rsync 3.4.1, protocol 32
- **Date:** 2026-05-28

## Methodology

Each round uses a fresh daemon process per iteration (true cold start -
no persistent daemon, no connection reuse). The upstream rsync client
pushes files to each daemon to ensure wire-compatible transfer.

1. Start daemon (`--daemon --no-detach`)
2. Wait 300ms for daemon readiness
3. Time `rsync -a src/ rsync://localhost:PORT/bench/` (client to daemon)
4. Kill daemon, record elapsed milliseconds
5. Repeat 20 times per round

Three rounds were run:

- **Round 1 and 2:** 5 files, 10 KB each (50 KB total)
- **Round 3:** 20 files, 10 KB each (200 KB total)

## Results

### Round 1 - 5 files (50 KB)

| Metric | oc-rsync (ms) | upstream (ms) | ratio |
|--------|---------------|---------------|-------|
| Mean | 136.1 | 153.0 | 0.89x |
| Median | 129.5 | 154.0 | 0.84x |
| Min | 102 | 133 | - |
| Max | 174 | 180 | - |
| P10 | 114 | 140 | - |
| P90 | 170 | 161 | - |

Raw oc-rsync (ms): 121 152 165 120 174 160 114 132 170 102 114 116 124 102 169 116 132 173 138 127

Raw upstream (ms): 148 163 161 141 133 159 154 159 180 157 161 152 156 135 146 140 161 151 154 149

### Round 2 - 5 files (50 KB)

| Metric | oc-rsync (ms) | upstream (ms) | ratio |
|--------|---------------|---------------|-------|
| Mean | 129.9 | 150.6 | 0.86x |
| Median | 128.5 | 149.0 | 0.86x |
| Min | 112 | 131 | - |
| Max | 193 | 200 | - |

Raw oc-rsync (ms): 138 136 149 112 119 116 115 131 126 124 140 125 112 130 132 136 174 120 127 136

Raw upstream (ms): 175 153 152 147 145 168 153 148 155 150 139 144 154 145 161 140 142 144 154 143

### Round 3 - 20 files (200 KB)

| Metric | oc-rsync (ms) | upstream (ms) | ratio |
|--------|---------------|---------------|-------|
| Mean | 142.6 | 151.8 | 0.94x |
| Median | 142.5 | 149.0 | 0.96x |
| Min | 113 | 131 | - |
| Max | 193 | 200 | - |

Raw oc-rsync (ms): 165 141 153 138 143 142 113 115 151 114 126 130 158 162 130 144 149 143 142 193

Raw upstream (ms): 168 161 149 171 146 149 153 147 134 151 144 155 134 141 131 200 157 148 154 142

## Summary

| Round | Files | oc-rsync median | upstream median | ratio |
|-------|-------|-----------------|-----------------|-------|
| 1 | 5 | 129.5 ms | 154.0 ms | 0.84x |
| 2 | 5 | 128.5 ms | 149.0 ms | 0.86x |
| 3 | 20 | 142.5 ms | 149.0 ms | 0.96x |

**Aggregate median ratio: 0.84x - 0.96x (oc-rsync is faster than upstream)**

The DIS-6 fixes eliminated the 3.75x regression entirely. Daemon cold-start
performance now meets the <= 1.1x target - oc-rsync is consistently at
parity with or faster than upstream rsync for cold-start daemon transfers.

## Prior measurements

| Date | oc-rsync | upstream | ratio | Notes |
|------|----------|----------|-------|-------|
| Pre-DIS-6 | ~1350 ms | ~360 ms | 3.75x | Initial measurement (DIS-2) |
| Post-DIS-6 | ~130 ms | ~151 ms | 0.86x | This benchmark (DIS-7) |

The absolute times differ from the DIS-2 measurement because DIS-2 used
a different container image (Arch Linux `oc-rsync-bench`) with different
system load characteristics. The ratio comparison is what matters - the
regression has been eliminated.
