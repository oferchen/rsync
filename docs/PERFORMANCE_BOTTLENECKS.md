# Performance Bottlenecks Analysis

This document tracks identified performance bottlenecks with concrete evidence from profiling.

## Measurement Methodology

All measurements use consistent methodology:

```bash
# Create test data: 1000 x 1KB files
WORKDIR=$(mktemp -d)
mkdir -p "$WORKDIR/src" "$WORKDIR/dest"
for i in $(seq 1 1000); do
    dd if=/dev/urandom of="$WORKDIR/src/file_$i.dat" bs=1024 count=1 2>/dev/null
done

# Timing comparison (3 runs each)
time rsync -a "$WORKDIR/src/" "$WORKDIR/dest/"
time oc-rsync -a "$WORKDIR/src/" "$WORKDIR/dest/"

# Syscall analysis
strace -c <binary> -a "$WORKDIR/src/" "$WORKDIR/dest/"
```

## Executive Summary

| Metric | Upstream rsync | oc-rsync | Ratio |
|--------|---------------|----------|-------|
| **Time (1000 files)** | 72 ms | 1069 ms | **15x slower** |
| **Total syscalls** | 5,406 | 222,553 | **41x more** |
| `close` | 1,032 | 28,037 | 27x |
| `openat` | 1,024 | 12,021 | 12x |
| `getdents64` | 6 | 8,010 | **1,335x** |
| `timerfd_create` | 2 | 4,004 | **2,002x** |
| `timerfd_settime` | 2 | 4,004 | **2,002x** |

---

## CRITICAL: UID/GID NSS Lookup Bottleneck

**Severity:** CRITICAL (causes 15x slowdown)

### Evidence

Syscall trace shows per-file pattern:

```
openat(AT_FDCWD, "/etc/passwd", O_RDONLY|O_CLOEXEC) = 5
openat(AT_FDCWD, "/etc/passwd", O_RDONLY|O_CLOEXEC) = 5
openat(AT_FDCWD, "/etc/group", O_RDONLY|O_CLOEXEC) = 5
openat(AT_FDCWD, "/run/systemd/userdb/", O_RDONLY|O_DIRECTORY) = 5
getdents64(5, /* 6 entries */, 32768) = 232
timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC|TFD_NONBLOCK) = 8
timerfd_settime(8, TFD_TIMER_ABSTIME, ...) = 0
```

This pattern repeats **for every file** during metadata application.

### Root Cause

**Location:** `crates/metadata/src/id_lookup.rs`

The `map_uid()` and `map_gid()` functions perform uncached NSS lookups:

```rust
// crates/metadata/src/id_lookup.rs:29-44
pub fn map_uid(uid: RawUid, numeric_ids: bool) -> Option<Uid> {
    if numeric_ids {
        return Some(ownership::uid_from_raw(uid));
    }
    // PROBLEM: No caching - does NSS lookup for EVERY file
    let mapped = match lookup_user_name(uid) {      // getpwuid_r + systemd userdb
        Ok(Some(bytes)) => match lookup_user_by_name(&bytes) {  // getpwnam_r + systemd userdb
            // ...
        }
    };
}
```

For each file, this triggers:
1. `getpwuid_r()` → reads `/etc/passwd` + systemd userdb connection
2. `lookup_user_by_name()` → reads `/etc/passwd` again
3. `getgrgid_r()` → reads `/etc/group` + systemd userdb connection
4. `lookup_group_by_name()` → reads `/etc/group` again

The systemd userdb connection creates timerfd for socket timeouts, explaining the 4,004 timerfd_create calls for 1,000 files.

### Fix

Add a **UID/GID cache** to avoid repeated NSS lookups:

```rust
use std::collections::HashMap;
use std::sync::Mutex;

static UID_CACHE: Mutex<HashMap<RawUid, Option<Uid>>> = Mutex::new(HashMap::new());
static GID_CACHE: Mutex<HashMap<RawGid, Option<Gid>>> = Mutex::new(HashMap::new());

pub fn map_uid(uid: RawUid, numeric_ids: bool) -> Option<Uid> {
    if numeric_ids {
        return Some(ownership::uid_from_raw(uid));
    }

    // Check cache first
    if let Some(cached) = UID_CACHE.lock().unwrap().get(&uid) {
        return *cached;
    }

    // Expensive lookup only on cache miss
    let result = lookup_and_map_uid(uid);
    UID_CACHE.lock().unwrap().insert(uid, result);
    result
}
```

**Expected impact:** Reduce syscalls from 222,553 to ~6,000 (matching upstream), achieving 10-15x speedup.

### Upstream Reference

Upstream rsync caches UID/GID mappings in `uidlist.c`:
- `static struct idlist *uidlist` - cached UID mappings
- `static struct idlist *gidlist` - cached GID mappings
- Lookups only happen once per unique UID/GID

---

## HIGH: Atomic Rename Per File

**Severity:** HIGH

### Evidence

```
strace output:
rename: 1,000 vs 0
```

### Root Cause

**Location:** `crates/engine/src/local_copy/executor/file/guard.rs`

oc-rsync uses atomic write-to-temp-then-rename pattern for crash safety:

```rust
// Creates: /dest/.rsync-tmp-file_1.txt-338608-0
// Then: rename() to final path
```

This is correct behavior but adds syscall overhead. Upstream rsync uses `--inplace` by default for local copies.

### Fix

Consider `--inplace` optimization for local copies where atomicity is less critical, or batch the renames.

---

## MEDIUM: Excessive Directory Reads

**Severity:** MEDIUM

### Evidence

```
getdents64: 6 vs 8,010 (1,335x more)
```

### Root Cause

The 8,010 getdents64 calls are split:
- ~2 calls for source/dest initial read
- ~8,000 calls to `/run/systemd/userdb/` (NSS-related, covered above)

Once UID/GID caching is implemented, this will drop to ~6 calls.

---

## Running Benchmarks

```bash
# Quick profiling (compares oc-rsync vs upstream)
./scripts/profile_transfer.sh

# With perf profiling
./scripts/profile_transfer.sh --perf

# With flamegraph generation
./scripts/profile_transfer.sh --flamegraph

# Syscall analysis
WORKDIR=$(mktemp -d)
mkdir -p "$WORKDIR/src" "$WORKDIR/dest"
for i in $(seq 1 1000); do echo "x" > "$WORKDIR/src/file_$i.txt"; done
strace -c oc-rsync -a "$WORKDIR/src/" "$WORKDIR/dest/"
```

---

## Previously Identified (Lower Priority)

### Delta Token Allocation

**Location:** `crates/transfer/src/delta_apply.rs:326, 353, 375`

Per-token vector allocation in delta application loop. Impact is significant for large files with many delta tokens, but not the primary bottleneck for many-small-files workloads.

### PathBuf Cloning in File Walker

**Location:** `crates/flist/src/file_list_walker.rs`

Addressed with `std::mem::take()` optimization - minimal impact measured.

---

## Summary

| Priority | Issue | Impact | Status |
|----------|-------|--------|--------|
| ~~**CRITICAL**~~ | ~~UID/GID NSS lookup per file~~ | ~~15x slowdown~~ | **✅ FIXED** |
| **LOW** | Atomic rename per file | ~1000 extra syscalls | Acceptable |
| ~~**MEDIUM**~~ | ~~Directory reads (NSS-related)~~ | ~~Resolved by UID/GID fix~~ | **✅ FIXED** |

---

## Results After UID/GID Cache Implementation

**Commit:** Implemented in `crates/metadata/src/id_lookup.rs`

| Metric | Before Cache | After Cache | Improvement |
|--------|-------------|-------------|-------------|
| **Time (1000 files)** | 1069 ms | 37 ms | **29x faster** |
| **Syscalls** | 222,553 | 17,346 | **13x fewer** |
| **vs Upstream rsync** | 15x slower | **2x faster** | ✅ |

### Syscall Comparison (After Cache)

| Syscall | Upstream rsync | oc-rsync | Notes |
|---------|---------------|----------|-------|
| total | 5,406 | 17,346 | 3x more (acceptable) |
| openat | 1,024 | 3,021 | Extra for atomic write pattern |
| statx | 0 | 3,004 | oc-rsync uses modern statx |
| rename | 0 | 1,000 | Atomic write pattern |
| timerfd_create | 2 | 4 | **✅ Fixed from 4,004** |
| getdents64 | 6 | 10 | **✅ Fixed from 8,010** |

### Implementation Details

```rust
// crates/metadata/src/id_lookup.rs
static UID_CACHE: LazyLock<Mutex<HashMap<RawUid, RawUid>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static GID_CACHE: LazyLock<Mutex<HashMap<RawGid, RawGid>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn map_uid(uid: RawUid, numeric_ids: bool) -> Option<Uid> {
    // Fast path: numeric mode
    if numeric_ids {
        return Some(ownership::uid_from_raw(uid));
    }

    // Check cache first (O(1) lookup)
    if let Ok(cache) = UID_CACHE.lock() {
        if let Some(&cached) = cache.get(&uid) {
            return Some(ownership::uid_from_raw(cached));
        }
    }

    // Cache miss: expensive NSS lookup (only once per unique UID)
    let mapped = map_uid_uncached(uid);

    // Store for future lookups
    if let Ok(mut cache) = UID_CACHE.lock() {
        cache.insert(uid, mapped);
    }

    Some(ownership::uid_from_raw(mapped))
}
```

The remaining syscall overhead (3x vs upstream) comes from legitimate operations:
- **Atomic write pattern**: write to temp file + rename (safer than inplace)
- **statx**: Modern metadata API with richer information
- **Extra openat**: One for reading source, one for writing dest

These are acceptable trade-offs for correctness and modern API usage.
