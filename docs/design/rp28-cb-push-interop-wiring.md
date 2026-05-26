# RP28.c.b - Wire RP28.c.a Fixtures into the 2.6.9 Push CI Cell

Design-only document. No code changes ship with this task. Specifies how
to replace the existing hard-coded smoke tree in the RP28.c push interop
cell with the twelve-fixture matrix defined in RP28.c.a.

Task: RP28.c.b. Parent: RP28.c (#2728). Grandparent: RP28 (#2725).

Memory note: `[[project_protocol_compat]]`.

## 1. Current State

The RP28.c push cell is an inline shell block in
`.github/workflows/_interop.yml` (lines 95-187). It exercises a single
hard-coded file tree:

- `hello.txt` (12 bytes)
- `multiline.txt` (12 bytes)
- `binary.dat` (8 KiB, `/dev/urandom`)
- `subdir/nested.txt` (7 bytes)

The topology is: **oc-rsync sender -> rsync 2.6.9 receiver daemon**. The
rsync 2.6.9 binary is started as `--daemon --no-detach` on an ephemeral
port, oc-rsync pushes via `rsync://` URL, and the step asserts
byte-identical contents via `diff -r`.

This exercises a trivial happy-path push but does not cover most of the
protocol-28-specific code paths catalogued in RP28.a. Specifically, it
misses:

- Flist ordering under the `t_PATH` comparator (RP28.h).
- Zlib compression at the 0xFFFF chunk boundary (RP28.i).
- Non-INC_RECURSE deep directory walk.
- Hardlink wire model (pre-30 `(dev+1, ino)` longints).
- Delta/rolling-checksum at protocol 28 (MD4 default, no negotiation).
- `--delete` without `NDX_DEL_STATS` trailer.
- Filter/exclude rule encoding at protocol 28.
- Empty-file framing edge case.
- Incremental update (quick-check + delta).

## 2. Target State

Replace the inline smoke tree with a structured fixture matrix that
mirrors the RP28.f pattern: a setup script materialises deterministic
fixtures, and a run script iterates them with per-fixture pass/fail
tracking. The existing inline step in `_interop.yml` is replaced with a
single `bash scripts/rp28_c_b_run.sh` invocation.

After RP28.c.b, the push cell exercises all twelve RP28.c.a fixtures:

| # | Description | Flags | What it validates |
|---|-------------|-------|-------------------|
| F1 | Single small file (32 B) | `-av` | Basic wire handshake, single-block flist, delta framing. |
| F2 | 100 mixed-size files (1 B - 64 KiB) | `-av` | Flist ordering under `t_PATH` comparator (RP28.h). |
| F3 | 64 KiB repeated-pattern file | `-av` | Content that stresses the zlib 0xFFFF chunk boundary. |
| F4 | Same as F3 with compression | `-avz` | Zlib wire format at protocol 28 without cursor-advance (RP28.i). |
| F5 | Deep directory tree (5 levels, 1365 nodes) | `-av` | Non-INC_RECURSE sender flist path. |
| F6 | Mixed types: symlinks, hardlink pair, FIFO | `-avH` | Legacy flag-byte encoding for non-regular files (RP28.g). |
| F7 | Extended UTF-8 filenames | `-av` | Name encoding pre-iconv at protocol 28. |
| F8 | 0-byte file + normal file | `-av` | Empty-content framing in the delta loop. |
| F9 | Incremental: push once, modify, push again | `-av` | Quick-check + delta at protocol 28. |
| F10 | 10 MiB file with 4 KiB mid-file mutation, pushed twice | `-av` | Rolling + strong checksum wire format (MD4, no negotiation). |
| F11 | F5 tree, delete two leaves, push with `--delete` | `-av --delete` | Receiver delete handling without `NDX_DEL_STATS` trailer. |
| F12 | F2 tree with `--exclude '*.tmp'` and `--filter '+ keep.dat'` | `-av --exclude --filter` | Filter chain encoding at protocol 28. |

## 3. Protocol Negotiation Behaviour

When oc-rsync pushes to a rsync 2.6.9 daemon:

1. **Greeting**: rsync 2.6.9 sends `@RSYNCD: 29\n` (protocol 29 is the
   highest version rsync 2.6.9 advertises). oc-rsync responds with
   `@RSYNCD: 29\n` - negotiated version = min(ours, theirs) = 29.
   Actually rsync 2.6.9 advertises protocol 29; the negotiated version
   is 29, not 28. However, capability flags available at protocol 29
   are a strict subset of protocol 30+ (no INC_RECURSE, no checksum
   negotiation, no varint flist flags). See RP28.a section C1-C4.

2. **Argument terminator**: protocol < 30 uses `\n` between arguments
   instead of `\0` (RP28.a W13).

3. **Capability string**: oc-rsync omits the `-e.LsfxCIvu` capability
   string entirely for protocol < 30 (RP28.a O1). No checksum
   negotiation occurs - both sides default to MD4 for strong checksums.

4. **Compression**: no vstring negotiation; zlib is the only option
   (RP28.a Z2). The zlib codec does NOT advance the deflate dictionary
   cursor between chunks > 0xFFFF bytes because protocol < 31 (RP28.a
   Z1, RP28.c.a section 2.3).

5. **Flist encoding**: single-byte flag prefix per entry (not varint,
   RP28.a W12). Fixed 4-byte LE for NDX, sizes, mtimes, uid/gid
   (RP28.a W1-W8). Hardlinks use `(dev+1, ino)` longints (RP28.a W10).

6. **INC_RECURSE**: disabled. The sender transmits the complete file
   list in one shot, not in incremental segments (RP28.a C4).

7. **Delete**: `delete_before` is the default mode at protocol < 30
   (RP28.a C2). No `NDX_DEL_STATS` trailer in the goodbye phase -
   that is protocol >= 31 only.

8. **Multi-phase**: protocol 29 supports the redo phase
   (`max_phase = 2`); the second phase uses `SUM_LENGTH = 16` for
   stronger verification (RP28.a F4).

## 4. Deliverables

### 4.1 Setup Script: `scripts/rp28_c_b_setup.sh`

Follows the pattern established by `scripts/rp28_f_1_setup.sh`. Creates
three directory trees under `${RP28_C_ROOT:-/tmp/rp28-c}`:

- `daemon-dst/` - the destination tree exposed by the rsync 2.6.9
  receiver daemon's `[push]` module. Each fixture gets a sub-directory
  (`f01/` through `f12/`) that the run script resets before each push.
- `sender-src/` - the source trees that oc-rsync pushes from. Each
  fixture's sub-directory (`f01/` through `f12/`) is populated once at
  setup time.
- `expected/` - post-transfer expected trees for fixtures that need
  non-trivial assertions (F11 delete, F12 filter). For most fixtures
  the expected tree is identical to `sender-src/fNN/`.

The script also writes the rsync 2.6.9 daemon config to
`${RP28_C_ROOT}/rsyncd-push.conf`.

CLI contract (per RP28.c.a section 4):

```
scripts/rp28_c_b_setup.sh --scale {small|medium|full} --out <tmpdir>
```

- `--scale small`: F1-F4 only.
- `--scale medium`: F1-F8. Default for PR-triggered runs.
- `--scale full`: F1-F12. Default for nightly/release runs.
- `--out <tmpdir>`: materialisation root (defaults to
  `${RP28_C_ROOT:-/tmp/rp28-c}`).

The script writes `<tmpdir>/manifest.txt` listing each materialised
fixture, its description, and the recommended oc-rsync invocation flags.

All content is deterministic per RP28.c.a section 4:

- Pseudo-random content uses `awk` with arithmetic seeds or Python
  `random.seed(<fixture-id>)` - never unseeded `/dev/urandom`.
- The `binary.dat` in the current inline step uses `/dev/urandom`;
  RP28.c.b replaces this with seeded deterministic content.
- Re-running the script on a fresh tmpdir produces byte-identical output.

Environment requirements: coreutils, `dd`, `python3`, `ln`, `mkfifo`.
No network fetch. No cargo invocations.

#### 4.1.1 Fixture Generation Details

**F1** - Single small file:
```
sender-src/f01/hello.txt  # printf 'hello protocol 28\n' (19 bytes)
```

**F2** - 100 mixed-size files:
```
sender-src/f02/file_001.txt through file_100.txt
```
Sizes 1 B through 64 KiB, distributed deterministically. Filenames are
lowercase-only to avoid `LC_COLLATE` flakiness (RP28.c.a section 7).
Pin `LC_ALL=C` in the setup script.

**F3** - 64 KiB repeated-pattern file:
```
sender-src/f03/zlib_boundary.bin  # 65536 bytes of repeating pattern
```
Content is a single 64-byte pattern repeated 1024 times. This sits
exactly at the 0xFFFF chunk boundary that the zlib codec's `see_token`
gate checks.

**F4** - Compression variant:
```
sender-src/f04/  # identical to f03, transferred with -z
```
The run script passes `-avz` instead of `-av`. Same source content as
F3.

**F5** - Deep directory tree:
```
sender-src/f05/d0/d0/d0/d0/d0/leaf.txt  (5 levels deep)
```
4 entries per level = 4^1 + 4^2 + 4^3 + 4^4 + 4^5 = 1364 directories
plus 1 root = 1365 nodes. Each leaf directory contains a 16-byte file.
This exercises the non-INC_RECURSE sender-side flist build with a
non-trivial tree.

**F6** - Mixed file types:
```
sender-src/f06/regular.txt
sender-src/f06/link_target.txt
sender-src/f06/symlink1 -> link_target.txt
sender-src/f06/symlink2 -> /nonexistent/absolute
sender-src/f06/hard_a.txt  (hardlinked pair)
sender-src/f06/hard_b.txt  (hardlinked pair)
sender-src/f06/fifo1  (FIFO, via mkfifo)
```
No block-special placeholder - CI runners lack `mknod` privileges.
Pushed with `-avH` (hardlinks enabled). The FIFO test verifies the
legacy `needs_rdev` predicate for special files at protocol < 31
(RP28.a O2).

**F7** - Extended UTF-8 filenames:
```
sender-src/f07/café.txt
sender-src/f07/naïve.txt
sender-src/f07/日本語.txt
```
Tests `-8` (`--8-bit-output`) interaction at protocol 28, where iconv
negotiation does not exist.

**F8** - Empty file:
```
sender-src/f08/empty.dat     # 0 bytes
sender-src/f08/companion.txt # 32 bytes
```
Tests the receiver's handling of `len == 0` in the delta loop.

**F9** - Incremental update:
First push materialises `sender-src/f09/target.bin` (1 KiB). After the
first push succeeds, the run script appends 1 KiB to `target.bin` and
pushes again. The second push must produce a COPY+LITERAL delta pair,
not a full re-send.

**F10** - Large file delta:
```
sender-src/f10/payload.bin  # 10 MiB deterministic content
```
First push transfers the full file. The run script then mutates 4 KiB
at offset 5 MiB and pushes again. The second push exercises the
rolling-checksum match phase at protocol 28 with MD4 strong checksums.

**F11** - Delete:
Pre-seed `daemon-dst/f11/` with two extra leaf files before the push.
```
sender-src/f11/ = subset of F5 tree with two leaves removed
daemon-dst/f11/ = full F5 tree (pre-seeded at setup time)
```
After the push with `--delete`, the two extra files must be absent from
`daemon-dst/f11/`. The expected tree is the sender's tree.

**F12** - Filter/exclude:
```
sender-src/f12/keep.dat
sender-src/f12/keep.txt
sender-src/f12/drop.tmp
sender-src/f12/notes.tmp
```
The push uses `--exclude '*.tmp' --filter '+ keep.dat'`. Expected
destination: `keep.dat` and `keep.txt` are present; `drop.tmp` and
`notes.tmp` are absent.

### 4.2 Run Script: `scripts/rp28_c_b_run.sh`

Follows the pattern established by `scripts/rp28_f_1_run.sh`. CLI:

```
scripts/rp28_c_b_run.sh \
  [--oc-rsync target/release/oc-rsync] \
  [--rsync-2-6-9 /usr/local/bin/rsync-2.6.9] \
  [--scale {small|medium|full}]
```

Exit codes:

- `0` - all requested fixtures passed.
- `1` - one or more fixtures failed.
- `77` - required binary missing (treat as skip in CI).

The script:

1. Validates both binaries exist and are executable.
2. Invokes `scripts/rp28_c_b_setup.sh --scale <scale> --out <tmpdir>`.
3. Writes a minimal `rsyncd.conf` for the 2.6.9 daemon (reusing the
   config created by the setup script, or generating one if absent):
   ```
   use chroot = no
   address = 127.0.0.1
   pid file = <tmpdir>/rsyncd-push.pid
   log file = <tmpdir>/rsyncd-push.log
   [push]
       path = <tmpdir>/daemon-dst
       read only = false
       list = yes
   ```
4. Allocates an ephemeral port via the `python3` socket trick.
5. Starts the rsync 2.6.9 daemon on that port with `--daemon
   --no-detach`.
6. Polls for the daemon to bind (up to 10 seconds, 0.5s interval).
7. Iterates each fixture in the manifest:
   - Resets the per-fixture destination sub-directory in `daemon-dst/`.
   - Pushes from `sender-src/fNN/` to
     `rsync://127.0.0.1:<port>/push/fNN/`.
   - Runs the per-fixture verification (see section 5).
   - Records pass/fail with the `record()` helper.
8. Prints a summary and exits non-zero if any fixture failed.
9. Trap handler kills the daemon and removes the tmpdir on success (leaves
   it on failure for CI artifact capture).

Helper functions carried over from the `rp28_f_1_run.sh` pattern:

- `run_push()` - wraps `timeout 60 "$OC_RSYNC" <flags> "$SRC/" "rsync://..."`.
  Captures stderr to a per-fixture file.
- `reset_dst()` - `rm -rf` + `mkdir -p` for the destination sub-dir.
- `verify_diff()` - `diff -r --no-dereference` between expected and
  actual trees.
- `check_daemon_quiet()` - scan daemon log for `rsync error:` and
  `protocol mismatch|unexpected EOF`.
- `check_client_quiet()` - scan oc-rsync stderr for panics, `error:`
  lines, and unexpected `WARNING` lines (allow-list protocol-downgrade
  warnings).

### 4.3 Workflow YAML Changes

In `.github/workflows/_interop.yml`, replace the existing inline
`Run rsync 2.6.9 push interop (RP28.c)` step (lines 104-187) with:

```yaml
      - name: Run rsync 2.6.9 push interop (RP28.c)
        continue-on-error: true
        env:
          RP28_C_SCALE: ${{ github.event_name == 'schedule' && 'full' || 'medium' }}
        run: |
          set -euo pipefail
          UP_269="target/interop/upstream-install/2.6.9/bin/rsync"
          if [[ ! -x "$UP_269" ]]; then
            echo "rsync 2.6.9 binary missing at $UP_269; skipping RP28.c push cell" >&2
            exit 0
          fi

          OC_RSYNC="target/dist/oc-rsync"
          if [[ ! -x "$OC_RSYNC" ]]; then
            OC_RSYNC="target/release/oc-rsync"
          fi
          if [[ ! -x "$OC_RSYNC" ]]; then
            echo "oc-rsync binary missing" >&2
            exit 1
          fi

          bash scripts/rp28_c_b_run.sh \
            --oc-rsync "$OC_RSYNC" \
            --rsync-2-6-9 "$UP_269" \
            --scale "${RP28_C_SCALE}"

      - name: Capture RP28.c push log on failure
        if: failure()
        run: |
          if [[ -f /tmp/rp28-c/rsyncd-push.log ]]; then
            echo "--- /tmp/rp28-c/rsyncd-push.log ---"
            cat /tmp/rp28-c/rsyncd-push.log
          else
            echo "no /tmp/rp28-c/rsyncd-push.log to capture"
          fi
```

Key changes from the current inline step:

- The fixture materialisation and test iteration move out of the YAML
  into `scripts/rp28_c_b_setup.sh` and `scripts/rp28_c_b_run.sh`.
- Scale selection via `RP28_C_SCALE` env var: `medium` for PR runs,
  `full` for scheduled nightly and `workflow_dispatch`.
- A new log-capture step runs on failure to surface the 2.6.9 daemon
  log in the GitHub Actions UI.
- `continue-on-error: true` remains until RP28.k promotes the pre-30
  parity baseline to a required check.

### 4.4 Scale Selection Logic

The `_interop.yml` workflow is called by other workflows via
`workflow_call`. The caller determines `github.event_name`:

- `pull_request` / `push`: `RP28_C_SCALE` = `medium` (F1-F8, ~30s).
- `schedule` (nightly cron): `RP28_C_SCALE` = `full` (F1-F12, ~90s).
- `workflow_dispatch` (manual): `RP28_C_SCALE` defaults to `medium` but
  can be overridden by adding an input parameter (deferred to RP28.c.c
  if desired).

## 5. Pass/Fail Criteria

Per RP28.c.a section 6, a fixture passes when all of the following hold:

### 5.1 Exit Code

`oc-rsync` push exits 0. The rsync 2.6.9 daemon does not crash (the
daemon PID is still alive after the push completes).

### 5.2 Byte-Identical Destination

`diff -r --no-dereference "$EXPECTED" "$DST"` produces no output. The
expected tree for most fixtures is the sender source tree. Exceptions:

- **F9**: after the second push, the expected tree is the mutated source
  (source with 1 KiB appended).
- **F10**: after the second push, the expected tree is the mutated source
  (4 KiB mutation at offset 5 MiB).
- **F11**: the expected tree is the sender source (a subset of the
  pre-seeded destination); the two extra files must be absent.
- **F12**: the expected tree excludes `*.tmp` files. The `expected/f12/`
  directory in the setup script contains only `keep.dat` and `keep.txt`.

### 5.3 Clean Daemon Log

The rsync 2.6.9 daemon log must not contain:

- `rsync error:` lines.
- `protocol mismatch` or `unexpected EOF` lines.

Routine `rsync: connection from ...` notices are allowed.

### 5.4 Clean Client Stderr

oc-rsync stderr must not contain:

- `panicked at` or `thread ... panicked`.
- `error:` log lines.
- `WARNING` lines other than expected protocol-downgrade messages.
  Allow-listed prefixes: `WARNING: protocol downgrade`,
  `WARNING: protocol version 29`, `WARNING: protocol 29`.

### 5.5 Hardlink Preservation (F6)

For the hardlink pair in F6, `stat -c '%i'` on both destination files
must report the same inode number.

### 5.6 Symlink Target Preservation (F6)

For symlinks in F6, `readlink` on the destination must match the source
target string. The absolute symlink (`/nonexistent/absolute`) must
survive as a dangling symlink, not be resolved.

### 5.7 No Orphan Processes

The trap handler must kill the daemon PID. After cleanup, the ephemeral
port must be released (no `LISTEN` state in `ss -tlnp` output).

### 5.8 Single Fixture Failure Fails the Cell

Any single fixture failure exits the run script with code 1, which fails
the CI step.

## 6. Known Limitations of Protocol 28/29 Push

These are inherent to the protocol version and are NOT bugs. The test
fixtures must not assert against these limitations:

- **No INC_RECURSE**: the sender transmits the full file list before any
  data. For F5 (1365 nodes) this means the entire flist is in memory on
  both sides before the first byte of file content flows.

- **No checksum negotiation**: both sides use MD4 for strong checksums.
  MD5/XXH3/XXH128 are unavailable. F10 exercises this path explicitly.

- **Zlib-only compression**: no zstd, no lz4, no zlibx. F4 exercises
  this by passing `-z` and asserting the compressed transfer succeeds.

- **No `NDX_DEL_STATS`**: protocol < 31 does not send delete-count
  statistics in the goodbye phase. F11 must NOT assert on delete stats;
  it only asserts the files are actually deleted.

- **No varint flist flags**: the flag prelude per flist entry is 1-2
  bytes (fixed), not varint. This is transparent to the test harness
  because it operates at the file-tree level, not the wire-byte level.

- **Legacy hardlink model**: pre-30 transmits raw `(dev+1, ino)`
  longints instead of varint indices. The receiver normalises these
  internally. F6 tests the end result (same inode on disk).

- **`delete_before` default**: protocol < 30 defaults to deleting before
  transfer, not during. F11 is unaffected because `--delete` works in
  both modes.

- **No `-A` (ACLs) or `-X` (xattrs)**: rejected at protocol < 30 per
  upstream `compat.c:652-668`. No fixture tests these flags.

- **Append mode clamped**: `append_mode == 1` is adjusted to `2` on
  protocol < 30. No fixture tests `--append`.

## 7. Fixture-to-Code-Path Mapping

Each fixture exercises specific protocol-28 code paths from the RP28.a
inventory. This mapping ensures every HIGH-severity gate from RP28.a
section "Summary Table" is covered by at least one fixture:

| RP28.a Gate | Severity | Covered by Fixture(s) |
|-------------|----------|-----------------------|
| W1 (NDX codec) | HIGH | F1, F2, F5, F9, F10, F11 |
| W2 (NDX_DONE goodbye) | HIGH | F1, F2 (every transfer) |
| W3 (varint fallback) | HIGH | F2, F5, F10 |
| W4 (protocol codec) | HIGH | F1 (every transfer) |
| W5 (name length encoding) | HIGH | F2, F7 |
| W6 (size encoding) | HIGH | F2, F3, F10 |
| W7 (mtime encoding) | HIGH | F2, F5 |
| W8 (uid/gid encoding) | HIGH | F2, F6 |
| W9 (rdev minor) | HIGH | F6 (FIFO) |
| W10 (hardlink encoding) | HIGH | F6 |
| W11 (xflags bit layout) | HIGH | F2, F6 |
| W12 (flag prelude) | HIGH | F2, F5, F6 |
| C1 (binary negotiation skip) | HIGH | F1 (every transfer) |
| C6 (daemon auth digest) | HIGH | F1 (greeting/auth) |
| F1 (sort comparator) | HIGH | F2 |
| F2 (io_error flag) | HIGH | F1 (every transfer) |
| S1 (MD4 default) | HIGH | F9, F10 |
| S4 (block-length max) | HIGH | F10 |

All 18 HIGH-severity gates are covered.

## 8. Dependencies

- **RP28.b.1** (merged, PR #4903): `scripts/build_rsync_2_6_9.sh`.
- **RP28.b.3** (cache step): publishes the 2.6.9 binary at
  `target/interop/upstream-install/2.6.9/bin/rsync`. The run script
  falls back to `scripts/build_rsync_2_6_9.sh` if the cache is cold.
- **RP28.c.a** (merged): fixture spec at
  `docs/design/rp28-c-a-rsync-2-6-9-push-fixtures.md`.
- **RP28.k**: decision on whether pre-30 parity gates CI.
  `continue-on-error: true` stays until RP28.k promotes to required.

## 9. Risks and Mitigations

### 9.1 F5 Node Count

The F5 deep tree (1365 nodes) may be slow on CI runners with cold
filesystem caches. Mitigation: the `--scale medium` tier excludes F5
(it is F5, index 5, included only in `medium` and above). Actually per
RP28.c.a, `--scale medium` includes F1-F8, so F5 is always exercised in
PR runs. If F5 proves too slow (> 30s per fixture), reduce the branching
factor from 4 to 3 (= 364 nodes) in a follow-up.

### 9.2 FIFO Transfer via Daemon

rsync 2.6.9 may or may not handle FIFO receipt correctly in daemon mode
without `use chroot = true`. The F6 fixture includes a FIFO as a
stretch test. If FIFO transfer fails consistently, downgrade the FIFO
assertion to an expected-failure annotation (`XFAIL F6.fifo`) and track
in a follow-up issue. The rest of F6 (symlinks, hardlinks, regular
files) must still pass.

### 9.3 F10 Duration

The 10 MiB file in F10 is transferred twice (full + delta). On slow CI
runners this may take 10-15 seconds per transfer. The 60-second
`timeout` wrapper per fixture provides adequate headroom. F10 is in the
`--scale full` tier only (F1-F12), so PR runs (`--scale medium`, F1-F8)
skip it.

### 9.4 Ephemeral Port Races

The `python3` socket-bind trick releases the port before the daemon
binds. A competing process could grab the port in the gap. Mitigation:
the bind-poll loop retries for 10 seconds, and the port is allocated
from the kernel's ephemeral range (typically 32768-60999) where
collision probability is low. This is the same trade-off accepted by
RP28.e.2, RP28.f.2, and the existing inline RP28.c step.

### 9.5 LC_COLLATE Sort-Order Drift

F2 uses lowercase-only filenames and pins `LC_ALL=C` in the setup
script (per RP28.c.a section 7). The run script inherits the same
locale. If CI runners override `LC_ALL` at the job level, the fixture
ordering could drift. Mitigation: the run script explicitly exports
`LC_ALL=C` before the transfer loop.

## 10. Implementation Checklist

The follow-up implementation PR must land the following changes:

1. [ ] `scripts/rp28_c_b_setup.sh` - fixture generator with
   `--scale`/`--out` CLI.
2. [ ] `scripts/rp28_c_b_run.sh` - test runner with `--oc-rsync`,
   `--rsync-2-6-9`, `--scale` CLI.
3. [ ] `.github/workflows/_interop.yml` - replace inline RP28.c push
   step with `bash scripts/rp28_c_b_run.sh` invocation. Add
   log-capture step.
4. [ ] Verify `continue-on-error: true` is preserved.
5. [ ] Verify the cell degrades gracefully when the 2.6.9 binary is
   absent (exit 0, not exit 1).
6. [ ] Verify all twelve fixtures pass locally against a hand-built
   rsync 2.6.9 binary (or in a CI trial run).

## 11. Cross-References

- RP28.a inventory: `docs/design/rp28-a-pre30-code-paths-inventory.md`.
- RP28.c.a fixture spec:
  `docs/design/rp28-c-a-rsync-2-6-9-push-fixtures.md`.
- RP28.f.1 runner pattern: `scripts/rp28_f_1_run.sh`,
  `scripts/rp28_f_1_setup.sh`.
- RP28.e.1 runner pattern: `scripts/rp28_e_1_run.sh`,
  `scripts/rp28_e_1_setup.sh`.
- RP28.b.1 build script: `scripts/build_rsync_2_6_9.sh`.
- RP28.k decision: `docs/design/rp28-k-1-protocol-drop-vs-keep-decision.md`.
- Existing push cell:
  `.github/workflows/_interop.yml` lines 95-187.
- Protocol-floor constant:
  `crates/protocol/src/version/constants.rs:7`
  (`OLDEST_SUPPORTED_PROTOCOL = 28`).
- Capability string: `crates/core/src/setup.rs`
  (`build_capability_string`).
