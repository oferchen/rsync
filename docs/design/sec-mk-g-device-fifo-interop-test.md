# SEC-MK.g - Interop test: device/FIFO transfer with upstream rsync through sandbox

- **Status**: OPEN
- **Date**: 2026-05-26
- **Issue**: #3040
- **Predecessors**:
  - SEC-MK.a - mknod/mkfifo code-path inventory (`docs/audit/sec-mk-a-mknod-mkfifo-code-paths.md`)
  - SEC-MK.b - mknodat sandbox implementation (`docs/design/sec-mk-b-mknodat-sandbox-impl.md`)
  - SEC-MK.c - mkfifoat sandbox implementation
  - SEC-MK.d - receiver mknodat/mkfifoat wiring (`docs/design/sec-mk-d-receiver-mknodat-wiring.md`)
  - SEC-MK.e - mknod regression test spec (PR #5018, merged)
  - SEC-MK.f - mkfifo regression test spec (PR #5016, merged)
- **CVE context**: CVE-2026-29518, CVE-2026-43619 - symlink-swap TOCTOU
  on path-based syscalls under `use_chroot=false`
- **Scope**: End-to-end interop tests that verify device files and FIFOs
  transferred between upstream rsync and oc-rsync (with the sandboxed
  `mknodat`/`mkfifoat` receiver path) produce identical filesystem state
  in both transfer directions.

---

## 1. Objective

SEC-MK.e and SEC-MK.f validate the sandbox primitives in isolation
(unit + regression). This spec covers the wire-level interop dimension:
proving that the sandboxed creation path does not break protocol
compatibility when upstream rsync is on the other end of the connection.

Specifically:

1. Upstream rsync sender pushes device files and FIFOs to oc-rsync daemon
   receiver. The receiver creates them through `mknodat(sandbox_dirfd, ...)`.
   Verify the created entries match what upstream sent.

2. oc-rsync client pushes device files and FIFOs to upstream rsync daemon
   receiver. Verify that upstream creates identical entries - confirming
   oc-rsync's wire encoding for special files is correct.

3. Repeat both directions for pull transfers (daemon sender, client
   receiver) to cover the full matrix.

---

## 2. Test matrix

### 2.1 Dimensions

| Dimension | Values |
|-----------|--------|
| Upstream version | 3.0.9, 3.1.3, 3.4.1, 3.4.2 |
| Direction | push (client -> daemon), pull (daemon -> client) |
| File type | FIFO (`S_IFIFO`), char device (`S_IFCHR`), block device (`S_IFBLK`) |
| Privilege | root (real devices), unprivileged (FIFOs only) |

### 2.2 Full matrix (24 cells per version, 96 total)

```
Version x Direction x FileType
  3.0.9  x push     x fifo        (unprivileged)
  3.0.9  x push     x char_dev    (root only)
  3.0.9  x push     x block_dev   (root only)
  3.0.9  x pull     x fifo        (unprivileged)
  3.0.9  x pull     x char_dev    (root only)
  3.0.9  x pull     x block_dev   (root only)
  ... (repeat for 3.1.3, 3.4.1, 3.4.2)
```

### 2.3 Minimum viable matrix (unprivileged CI)

GitHub Actions `ubuntu-latest` runners are **not** root. They have
passwordless `sudo`, but the interop harness runs as the `runner` user.
Device node creation requires `CAP_MKNOD`, which unprivileged users lack.

The minimum viable matrix that runs on every PR:

| Version | Direction | FileType | Privilege |
|---------|-----------|----------|-----------|
| 3.0.9 | push | FIFO | unprivileged |
| 3.0.9 | pull | FIFO | unprivileged |
| 3.1.3 | push | FIFO | unprivileged |
| 3.1.3 | pull | FIFO | unprivileged |
| 3.4.1 | push | FIFO | unprivileged |
| 3.4.1 | pull | FIFO | unprivileged |
| 3.4.2 | push | FIFO | unprivileged |
| 3.4.2 | pull | FIFO | unprivileged |

**8 cells** - all unprivileged, all exercising the identical `mknodat`
syscall surface as device nodes (only the mode type bits differ).

### 2.4 Extended matrix (privileged CI, optional)

When the test detects root (or `sudo` availability), it extends with:

| Version | Direction | FileType | Device |
|---------|-----------|----------|--------|
| 3.4.1 | push | char_dev | `/dev/null` equivalent `(1,3)` |
| 3.4.1 | push | block_dev | `/dev/loop0` equivalent `(7,0)` |
| 3.4.1 | pull | char_dev | `(1,3)` |
| 3.4.1 | pull | block_dev | `(7,0)` |
| 3.4.2 | push | char_dev | `(1,3)` |
| 3.4.2 | push | block_dev | `(7,0)` |
| 3.4.2 | pull | char_dev | `(1,3)` |
| 3.4.2 | pull | block_dev | `(7,0)` |

Only tested against 3.4.1 and 3.4.2 because older versions have
different device-encoding semantics that are not relevant to the sandbox
migration validation.

### 2.5 Socket nodes

Socket nodes (`S_IFSOCK`) are excluded from interop testing. Upstream
rsync does not transfer sockets by default (and `--specials` skips them
in many configurations). The sandbox path for sockets is identical to
FIFOs at the syscall level - `mknodat(dirfd, leaf, S_IFSOCK | mode, 0)`.
The SEC-MK.e and SEC-MK.f regression tests cover socket creation through
the sandbox; interop coverage adds no value.

---

## 3. Test harness integration

### 3.1 Location

The tests integrate into the existing interop harness at
`tools/ci/run_interop.sh` as a new standalone test function, following
the pattern of `test_special_chars()`, `test_xattr_interop()`, and
other custom scenario functions.

**New function**: `test_device_fifo_interop()`

**Registration**: added to the standalone test array alongside
`special-chars`, `xattr`, etc., and dispatched by the existing
`run_standalone_tests` case statement.

### 3.2 Why a standalone function, not a comp_run_scenario entry

The existing `"devices|-avD|basic"` scenario in `comp_run_scenario` only
passes `-avD` and verifies basic file transfer. It does not create
device nodes or FIFOs in the source tree because device creation requires
root. The scenario just confirms that `-D` does not break regular file
transfer.

A standalone function is needed because:

1. **Source tree construction** requires `mkfifo` (and optionally `mknod`
   with `sudo`) to create the special files before transfer.
2. **Verification** must check `stat` type bits and device numbers, not
   just content hashes.
3. **Root gating** needs runtime detection to decide which file types
   to include in the source tree.
4. **Separate daemon configs** may be needed (e.g., `fake super = yes`
   for unprivileged device transfer).

### 3.3 Function signature

```bash
test_device_fifo_interop() {
  local version=$1
  local upstream_binary=$2
  local oc_client=$3
  local work=$4
  local log=$5
}
```

---

## 4. Test implementation

### 4.1 Source tree construction

```bash
setup_device_fifo_src() {
  local dir=$1
  local privileged=$2   # "true" or "false"

  rm -rf "$dir"
  mkdir -p "$dir/subdir"

  # FIFOs - always created (no privilege required)
  mkfifo -m 0644 "$dir/pipe_root"
  mkfifo -m 0600 "$dir/subdir/pipe_nested"
  mkfifo -m 0755 "$dir/pipe_exec"

  # Regular files alongside FIFOs (verify mixed content)
  echo "regular file" > "$dir/regular.txt"
  echo "nested regular" > "$dir/subdir/nested.txt"

  if [[ "$privileged" == "true" ]]; then
    # Character device: /dev/null equivalent (1, 3)
    sudo mknod "$dir/null_dev" c 1 3
    sudo chmod 0666 "$dir/null_dev"

    # Block device: /dev/loop0 equivalent (7, 0)
    sudo mknod "$dir/loop_dev" b 7 0
    sudo chmod 0660 "$dir/loop_dev"

    # Ensure files are owned by current user for rsync transfer
    sudo chown "$(id -u):$(id -g)" "$dir/null_dev" "$dir/loop_dev"
  fi
}
```

### 4.2 Verification

```bash
verify_device_fifo_transfer() {
  local src=$1
  local dest=$2
  local privileged=$3
  local direction_label=$4
  local failures=0

  # Verify FIFOs
  for fifo in pipe_root subdir/pipe_nested pipe_exec; do
    if [[ ! -p "$dest/$fifo" ]]; then
      echo "    ${direction_label}: $fifo not a FIFO (or missing)"
      failures=$((failures + 1))
      continue
    fi

    # Compare permissions
    local src_mode dest_mode
    if stat --version >/dev/null 2>&1; then
      src_mode=$(stat -c '%a' "$src/$fifo")
      dest_mode=$(stat -c '%a' "$dest/$fifo")
    else
      src_mode=$(stat -f '%Lp' "$src/$fifo")
      dest_mode=$(stat -f '%Lp' "$dest/$fifo")
    fi

    if [[ "$src_mode" != "$dest_mode" ]]; then
      echo "    ${direction_label}: $fifo perms $src_mode vs $dest_mode"
      failures=$((failures + 1))
    fi
  done

  # Verify regular files survived alongside FIFOs
  for f in regular.txt subdir/nested.txt; do
    if [[ ! -f "$dest/$f" ]]; then
      echo "    ${direction_label}: regular file $f missing"
      failures=$((failures + 1))
    elif ! cmp -s "$src/$f" "$dest/$f"; then
      echo "    ${direction_label}: regular file $f content mismatch"
      failures=$((failures + 1))
    fi
  done

  if [[ "$privileged" == "true" ]]; then
    # Verify character device
    if [[ ! -c "$dest/null_dev" ]]; then
      echo "    ${direction_label}: null_dev not a char device (or missing)"
      failures=$((failures + 1))
    else
      # Verify major/minor
      local src_rdev dest_rdev
      if stat --version >/dev/null 2>&1; then
        src_rdev=$(stat -c '%t:%T' "$src/null_dev")
        dest_rdev=$(stat -c '%t:%T' "$dest/null_dev")
      else
        src_rdev=$(stat -f '%Hr:%Lr' "$src/null_dev")
        dest_rdev=$(stat -f '%Hr:%Lr' "$dest/null_dev")
      fi
      if [[ "$src_rdev" != "$dest_rdev" ]]; then
        echo "    ${direction_label}: null_dev rdev $src_rdev vs $dest_rdev"
        failures=$((failures + 1))
      fi
    fi

    # Verify block device
    if [[ ! -b "$dest/loop_dev" ]]; then
      echo "    ${direction_label}: loop_dev not a block device (or missing)"
      failures=$((failures + 1))
    else
      local src_rdev dest_rdev
      if stat --version >/dev/null 2>&1; then
        src_rdev=$(stat -c '%t:%T' "$src/loop_dev")
        dest_rdev=$(stat -c '%t:%T' "$dest/loop_dev")
      else
        src_rdev=$(stat -f '%Hr:%Lr' "$src/loop_dev")
        dest_rdev=$(stat -f '%Hr:%Lr' "$dest/loop_dev")
      fi
      if [[ "$src_rdev" != "$dest_rdev" ]]; then
        echo "    ${direction_label}: loop_dev rdev $src_rdev vs $dest_rdev"
        failures=$((failures + 1))
      fi
    fi
  fi

  return $failures
}
```

### 4.3 Root detection

```bash
can_create_devices() {
  local tmp
  tmp=$(mktemp -d)
  if sudo mknod "$tmp/test_dev" c 1 3 2>/dev/null; then
    sudo rm -f "$tmp/test_dev"
    rmdir "$tmp"
    return 0
  fi
  rmdir "$tmp" 2>/dev/null || true
  return 1
}
```

### 4.4 Main test function

```bash
test_device_fifo_interop() {
  local version=$1
  local upstream_binary=$2
  local oc_client=$3
  local work=$4
  local log=$5

  local privileged="false"
  if can_create_devices; then
    privileged="true"
    echo "  [device-fifo] privileged mode: testing FIFOs + devices"
  else
    echo "  [device-fifo] unprivileged mode: testing FIFOs only"
  fi

  local df_src="${work}/device-fifo-src"
  local df_dest="${work}/device-fifo-dest"
  local df_oc_conf="${work}/device-fifo-oc.conf"
  local df_oc_pid="${work}/device-fifo-oc.pid"
  local df_oc_log="${work}/device-fifo-oc.log"
  local df_up_conf="${work}/device-fifo-up.conf"
  local df_up_pid="${work}/device-fifo-up.pid"
  local df_up_log="${work}/device-fifo-up.log"

  setup_device_fifo_src "$df_src" "$privileged"

  local oc_port up_port
  oc_port=$(allocate_ephemeral_port)
  up_port=$(allocate_ephemeral_port)

  local passed=0 total=0 failures=0

  # --- Direction 1: upstream sender -> oc-rsync daemon receiver ---
  # This exercises the sandboxed mknodat path on the oc-rsync receiver.

  rm -rf "$df_dest"; mkdir -p "$df_dest"

  write_rust_daemon_conf "$df_oc_conf" "$df_oc_pid" "$oc_port" \
      "$df_dest" "device-fifo interop"

  start_oc_daemon "$df_oc_conf" "$df_oc_log" "$upstream_binary" \
      "$df_oc_pid" "$oc_port"

  total=$((total + 1))
  echo "  [upstream ${version}->oc] device-fifo push"

  local rc=0
  if [[ "$privileged" == "true" ]]; then
    timeout "$hard_timeout" "$upstream_binary" -avD --specials --devices \
        --numeric-ids --timeout=10 \
        "${df_src}/" "rsync://127.0.0.1:${oc_port}/interop" \
        >"${log}.df-push.out" 2>"${log}.df-push.err" || rc=$?
  else
    timeout "$hard_timeout" "$upstream_binary" -av --specials \
        --numeric-ids --timeout=10 \
        "${df_src}/" "rsync://127.0.0.1:${oc_port}/interop" \
        >"${log}.df-push.out" 2>"${log}.df-push.err" || rc=$?
  fi

  if [[ $rc -ne 0 ]]; then
    echo "    FAIL: upstream->oc push exit=$rc"
    echo "    stderr: $(head -5 "${log}.df-push.err")"
    failures=$((failures + 1))
  elif verify_device_fifo_transfer "$df_src" "$df_dest" \
      "$privileged" "upstream->oc push"; then
    echo "    PASS"
    passed=$((passed + 1))
  else
    echo "    FAIL: verification"
    failures=$((failures + 1))
  fi

  stop_oc_daemon

  # --- Direction 2: oc-rsync sender -> upstream daemon receiver ---
  # This confirms oc-rsync's wire encoding for special files is correct.

  rm -rf "$df_dest"; mkdir -p "$df_dest"

  write_upstream_conf "$df_up_conf" "$df_up_pid" "$up_port" \
      "$df_dest" "device-fifo interop" ""

  start_upstream_daemon_with_retry "$upstream_binary" "$df_up_conf" \
      "$df_up_log" "$df_up_pid"

  total=$((total + 1))
  echo "  [oc->upstream ${version}] device-fifo push"

  rc=0
  if [[ "$privileged" == "true" ]]; then
    timeout "$hard_timeout" "$oc_client" -avD --specials --devices \
        --numeric-ids --timeout=10 \
        "${df_src}/" "rsync://127.0.0.1:${up_port}/interop" \
        >"${log}.df-oc-push.out" 2>"${log}.df-oc-push.err" || rc=$?
  else
    timeout "$hard_timeout" "$oc_client" -av --specials \
        --numeric-ids --timeout=10 \
        "${df_src}/" "rsync://127.0.0.1:${up_port}/interop" \
        >"${log}.df-oc-push.out" 2>"${log}.df-oc-push.err" || rc=$?
  fi

  if [[ $rc -ne 0 ]]; then
    echo "    FAIL: oc->upstream push exit=$rc"
    echo "    stderr: $(head -5 "${log}.df-oc-push.err")"
    failures=$((failures + 1))
  elif verify_device_fifo_transfer "$df_src" "$df_dest" \
      "$privileged" "oc->upstream push"; then
    echo "    PASS"
    passed=$((passed + 1))
  else
    echo "    FAIL: verification"
    failures=$((failures + 1))
  fi

  stop_upstream_daemon

  # --- Direction 3: upstream daemon sender -> oc-rsync client (pull) ---
  # Covers the pull path: oc-rsync receiver creates entries locally.

  rm -rf "$df_dest"; mkdir -p "$df_dest"

  # Place source tree in the upstream daemon module path
  local df_up_module_dir="${work}/device-fifo-up-module"
  rm -rf "$df_up_module_dir"
  cp -a "$df_src" "$df_up_module_dir"

  write_upstream_conf "$df_up_conf" "$df_up_pid" "$up_port" \
      "$df_up_module_dir" "device-fifo pull" ""

  start_upstream_daemon_with_retry "$upstream_binary" "$df_up_conf" \
      "$df_up_log" "$df_up_pid"

  total=$((total + 1))
  echo "  [oc<-upstream ${version}] device-fifo pull"

  rc=0
  if [[ "$privileged" == "true" ]]; then
    timeout "$hard_timeout" "$oc_client" -avD --specials --devices \
        --numeric-ids --timeout=10 \
        "rsync://127.0.0.1:${up_port}/interop/" "$df_dest" \
        >"${log}.df-pull.out" 2>"${log}.df-pull.err" || rc=$?
  else
    timeout "$hard_timeout" "$oc_client" -av --specials \
        --numeric-ids --timeout=10 \
        "rsync://127.0.0.1:${up_port}/interop/" "$df_dest" \
        >"${log}.df-pull.out" 2>"${log}.df-pull.err" || rc=$?
  fi

  if [[ $rc -ne 0 ]]; then
    echo "    FAIL: oc<-upstream pull exit=$rc"
    echo "    stderr: $(head -5 "${log}.df-pull.err")"
    failures=$((failures + 1))
  elif verify_device_fifo_transfer "$df_up_module_dir" "$df_dest" \
      "$privileged" "oc<-upstream pull"; then
    echo "    PASS"
    passed=$((passed + 1))
  else
    echo "    FAIL: verification"
    failures=$((failures + 1))
  fi

  stop_upstream_daemon

  # --- Direction 4: oc-rsync daemon sender -> upstream client (pull) ---
  # Covers oc-rsync's daemon serving special files to upstream receiver.

  rm -rf "$df_dest"; mkdir -p "$df_dest"

  local df_oc_module_dir="${work}/device-fifo-oc-module"
  rm -rf "$df_oc_module_dir"
  cp -a "$df_src" "$df_oc_module_dir"

  write_rust_daemon_conf "$df_oc_conf" "$df_oc_pid" "$oc_port" \
      "$df_oc_module_dir" "device-fifo pull"

  start_oc_daemon "$df_oc_conf" "$df_oc_log" "$upstream_binary" \
      "$df_oc_pid" "$oc_port"

  total=$((total + 1))
  echo "  [upstream<-oc ${version}] device-fifo pull"

  rc=0
  if [[ "$privileged" == "true" ]]; then
    timeout "$hard_timeout" "$upstream_binary" -avD --specials --devices \
        --numeric-ids --timeout=10 \
        "rsync://127.0.0.1:${oc_port}/interop/" "$df_dest" \
        >"${log}.df-up-pull.out" 2>"${log}.df-up-pull.err" || rc=$?
  else
    timeout "$hard_timeout" "$upstream_binary" -av --specials \
        --numeric-ids --timeout=10 \
        "rsync://127.0.0.1:${oc_port}/interop/" "$df_dest" \
        >"${log}.df-up-pull.out" 2>"${log}.df-up-pull.err" || rc=$?
  fi

  if [[ $rc -ne 0 ]]; then
    echo "    FAIL: upstream<-oc pull exit=$rc"
    echo "    stderr: $(head -5 "${log}.df-up-pull.err")"
    failures=$((failures + 1))
  elif verify_device_fifo_transfer "$df_oc_module_dir" "$df_dest" \
      "$privileged" "upstream<-oc pull"; then
    echo "    PASS"
    passed=$((passed + 1))
  else
    echo "    FAIL: verification"
    failures=$((failures + 1))
  fi

  stop_oc_daemon

  echo "  [device-fifo] ${version}: ${passed}/${total} passed, ${failures} failed"
  [[ $failures -eq 0 ]]
}
```

---

## 5. Root requirement handling

### 5.1 Privilege escalation strategy

The interop harness runs as the `runner` user on GitHub Actions. Device
node creation requires `CAP_MKNOD`. The strategy is:

1. **Probe**: `can_create_devices()` attempts `sudo mknod` in a temp
   directory. If it succeeds, the test runs in privileged mode.
2. **Graceful degradation**: if the probe fails, the test runs in
   unprivileged mode (FIFOs only). No test failure, no skip - the FIFO
   tests provide the same syscall-level coverage.
3. **CI enhancement (optional)**: a future workflow step can add
   `sudo setcap cap_mknod+ep $(which mknod)` to enable device creation
   without full root, or the test step can run under `sudo`.

### 5.2 Why FIFOs provide sufficient unprivileged coverage

The `mknodat` syscall wrapper is identical for all node types. The kernel
receives `mknodat(dirfd, name, mode, dev)` regardless of whether `mode`
contains `S_IFIFO`, `S_IFCHR`, or `S_IFBLK`. The privilege check happens
inside the kernel after the dirfd resolution. Therefore:

- A FIFO interop test that passes through the sandbox dirfd exercises
  the same dirfd-resolution, leaf-extraction, and wire-encoding code path
  as a device node transfer.
- The only additional coverage from real device nodes is verifying
  `dev` (major/minor) round-trip through the wire format. This is
  validated by the SEC-MK.e unit tests and does not require a full
  daemon interop round-trip.

### 5.3 `--fake-super` interop (future extension)

The `--fake-super` mode stores device metadata in xattrs on regular
placeholder files. This enables device metadata round-tripping without
root. A `--fake-super` interop cell is a natural extension but is
outside the scope of SEC-MK.g because:

- `--fake-super` does not exercise `mknodat` (it creates regular files
  via `open(O_CREAT|O_EXCL)`).
- The xattr encoding round-trip is a separate compatibility surface.

---

## 6. Platform gating

### 6.1 Unix only

The entire test function is gated on `uname -s` returning `Linux` or
`Darwin`. On other platforms (and on Windows CI, which uses
`run_interop_smoke.sh` instead), the test is not registered.

```bash
case "$(uname -s)" in
  Linux|Darwin) ;;
  *) echo "  [device-fifo] skipped: unsupported platform"; return 0 ;;
esac
```

### 6.2 macOS limitations

macOS GitHub Actions runners cannot create FIFOs via `mkfifo` in some
temp directory configurations. The macOS interop workflow uses
`run_interop_smoke.sh`, not `run_interop.sh`. This test is registered
only in `run_interop.sh` (Linux CI). macOS coverage of the sandbox path
is provided by the SEC-MK.e and SEC-MK.f unit tests, which do run on
macOS CI.

### 6.3 Version-specific gating

All four upstream versions (3.0.9, 3.1.3, 3.4.1, 3.4.2) support FIFO
transfer via `--specials`. Device node transfer via `--devices` requires
that both ends agree on the device number encoding. Protocol version
differences:

| Version | Protocol | Device encoding | Notes |
|---------|----------|----------------|-------|
| 3.0.9 | 30 | 32-bit `rdev` | Major/minor extracted via `MAJOR`/`MINOR` macros |
| 3.1.3 | 31 | 32-bit `rdev` | Same wire format |
| 3.4.1 | 32 | 64-bit `rdev` when `XMIT_RDEV_MINOR_8_pre30` cleared | Extended for large device numbers |
| 3.4.2 | 32 | Same as 3.4.1 | |

For the privileged matrix, device interop is restricted to versions
that share the same `rdev` wire encoding as oc-rsync. Since oc-rsync
targets protocol 32, and the 64-bit encoding is negotiated, all four
versions are compatible for the device numbers used (`(1,3)` and `(7,0)`
both fit in 8-bit minor).

---

## 7. Wire-byte verification approach

### 7.1 Filesystem state comparison

The primary verification method compares filesystem state after transfer:

| Attribute | Tool | Comparison |
|-----------|------|-----------|
| File type | `stat -c '%F'` (Linux) / `stat -f '%HT'` (macOS) | Exact match: "fifo", "character special file", "block special file" |
| Permissions | `stat -c '%a'` / `stat -f '%Lp'` | Exact numeric match (e.g., `644`) |
| Device numbers | `stat -c '%t:%T'` / `stat -f '%Hr:%Lr'` | Exact hex match for major:minor |
| File count | `find ... \| wc -l` | Source and dest have same entry count |

### 7.2 Bidirectional parity

Each transfer direction is verified independently. Additionally, a
cross-check confirms that:

- `upstream -> oc-rsync` and `oc-rsync -> upstream` produce the same
  filesystem state (modulo timestamps, which are not preserved for
  special files in all protocol versions).

This catches asymmetric encoding bugs where oc-rsync can receive but
not send (or vice versa) special file metadata correctly.

### 7.3 Wire capture (optional, diagnostic only)

For debugging failures, the harness can capture wire traffic using
`strace -e trace=write -p <daemon_pid>` or `tcpdump -i lo port <port>`.
This is not part of the automated test but is documented as a diagnostic
procedure:

```bash
# Capture wire bytes for the oc-rsync daemon receiver:
strace -f -e trace=read,write -p "$oc_pid" \
    -o "${log}.df-strace.txt" &
strace_pid=$!
# ... run transfer ...
kill "$strace_pid" 2>/dev/null || true
```

---

## 8. Idempotency and re-transfer

### 8.1 Second transfer should be a no-op

After the initial transfer, running the same rsync command again should
transfer zero files (the FIFO/device already exists with matching
metadata). This validates the quick-check logic for special files:

```bash
# Second transfer: expect no files transferred
timeout "$hard_timeout" "$upstream_binary" -avD --specials \
    --numeric-ids --timeout=10 \
    "${df_src}/" "rsync://127.0.0.1:${oc_port}/interop" \
    >"${log}.df-push-2.out" 2>"${log}.df-push-2.err" || rc=$?

# Verify itemize output shows no changes
if grep -qE '^[<>ch.]' "${log}.df-push-2.out"; then
  echo "    FAIL: second transfer modified files"
  failures=$((failures + 1))
fi
```

### 8.2 Permission change triggers re-transfer

After the initial transfer, changing the source FIFO permissions and
re-running should update only the permission attribute:

```bash
chmod 0600 "$df_src/pipe_root"
# ... re-transfer ...
# Verify pipe_root now has 0600 at dest
```

This validates that the generator correctly detects permission changes
on special files and the receiver applies them through the correct
code path.

---

## 9. Error cases

### 9.1 `EPERM` on device creation without privilege

When running unprivileged and the source contains device nodes (which
cannot happen in the test harness because `setup_device_fifo_src` gates
on `privileged`), the receiver should log an error and continue
transferring other files. The test does not exercise this path because
it cannot construct the source tree, but the expected behavior is
documented for manual verification:

- Upstream rsync sender includes a device node in the file list.
- oc-rsync receiver calls `mknodat(dirfd, leaf, S_IFCHR | mode, dev)`.
- Kernel returns `EPERM`.
- oc-rsync logs `rsync: mknod "<path>" failed: Operation not permitted (1)`
  and continues.
- Transfer exit code is 23 (partial transfer).

### 9.2 `EEXIST` on re-creation

If the destination already contains a FIFO at the target path, the
receiver should not fail. Upstream rsync behavior:

- Generator detects the existing FIFO via `stat`.
- If metadata matches, the file is skipped.
- If metadata differs (e.g., permissions), the generator sends an
  update request and the receiver applies `fchmodat` without re-creating.

The idempotency test (section 8.1) validates this implicitly.

---

## 10. CI registration

### 10.1 Standalone test array entry

```bash
standalone_tests=(
  # ... existing entries ...
  "device-fifo"
)
standalone_fns=(
  # ... existing entries ...
  "test_device_fifo_interop"
)
```

### 10.2 Dispatch case

```bash
case "$test_name" in
  # ... existing cases ...
  device-fifo)
    test_device_fifo_interop "$version" "$upstream_binary" \
        "$oc_client" "$workdir" "$interop_log_dir/$version"
    ;;
esac
```

### 10.3 Per-version execution

The test runs once per upstream version, called from the per-version
loop that already iterates over `versions=(3.0.9 3.1.3 3.4.1 3.4.2)`.
Each invocation starts fresh daemons on ephemeral ports.

---

## 11. Upstream rsync reference

- `syscall.c:do_mknod()` - the upstream `mknod` wrapper that this test
  validates compatibility against. Handles `am_root`, `--fake-super`
  placeholder substitution, and platform branching.
- `rsync.h:MAKEDEV()`/`MAJOR()`/`MINOR()` macros - device number
  composition and decomposition. Wire format uses these for `rdev`
  encoding.
- `receiver.c:recv_generator()` - generates file list entries for
  special files and decides whether to create, update, or skip.
- `flist.c:send_file_entry()` / `recv_file_entry()` - wire encoding of
  `rdev` field. Protocol 30+ uses `XMIT_RDEV_MINOR_8_pre30` flag to
  signal 8-bit vs extended minor number encoding.

---

## 12. Files changed

| File | Change |
|------|--------|
| `tools/ci/run_interop.sh` | Add `setup_device_fifo_src`, `verify_device_fifo_transfer`, `can_create_devices`, `test_device_fifo_interop` functions; register in standalone test arrays |

---

## 13. Risks and mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| `mkfifo` unavailable on CI runner | Very low | `ubuntu-latest` includes `mkfifo` in `coreutils`; pre-check with `command -v mkfifo` |
| `sudo mknod` blocked by CI security policy | Medium | Graceful degradation to FIFO-only mode; no test failure |
| Port collision between test phases | Low | `allocate_ephemeral_port` + `wait_for_port_free` pattern from existing harness |
| Upstream 3.0.9 encodes `rdev` differently for large device numbers | Low | Test uses small device numbers `(1,3)` and `(7,0)` that fit in 8-bit minor; encoding is identical across all protocol versions for these values |
| FIFO metadata mismatch due to umask | Medium | Use `--numeric-ids` and `--perms` (implied by `-a`); verify permissions with tolerance for umask stripping |
| Test flakiness from daemon startup race | Low | Existing `start_*_daemon_with_retry` + `wait_for_port` pattern handles this |
| cp -a does not preserve FIFOs | Medium | Verify `cp -a` behavior on Linux for FIFO copies; fall back to `setup_device_fifo_src` for both module dirs if needed |

---

## 14. Success criteria

1. All 8 unprivileged FIFO cells (4 versions x push + pull for each
   direction pair = 4 versions x 4 directions = 16 transfers, grouped
   into 4 per-version test invocations) pass on `ubuntu-latest`.
2. When `sudo` is available, the extended device matrix passes with
   correct major/minor round-trip.
3. Idempotency check confirms zero-change second transfer.
4. No regressions in existing interop scenarios.
5. The test completes within 60 seconds per version (240 seconds total),
   well within the interop workflow timeout.
