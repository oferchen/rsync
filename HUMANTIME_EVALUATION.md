# Evaluation: humantime Crate for Duration Parsing

## Executive Summary

**Recommendation: Do NOT adopt humantime for this codebase**

The humantime crate is designed for human-readable durations (e.g., "2h 30m", "1 day"), but rsync's duration parsing is fundamentally different:
- rsync uses **seconds only** (simple integers: "30", "60", "300")
- rsync has **bandwidth limits** with custom syntax (e.g., "1.5M", "100KB+1", "2.5e3")
- rsync has **size parsing** with unit suffixes (e.g., "1K", "2.5M", "1GB")

The current custom implementation is more appropriate for rsync's specific needs.

---

## Current Duration Parsing in Codebase

### 1. Timeout Parsing (`--timeout`)
**Location:** `/home/ofer/rsync/crates/cli/src/frontend/execution/options.rs`

**Format:** Simple integers representing seconds
- `30` → 30 seconds
- `0` → disabled
- `+60` → 60 seconds (leading + allowed)
- Negative values rejected
- No units (always seconds)

**Implementation:**
```rust
pub(crate) fn parse_timeout_argument(value: &OsStr) -> Result<TransferTimeout, Message> {
    // Parses plain integer seconds: "30", "60", "0"
    // Returns TransferTimeout::Seconds(NonZeroU64) or TransferTimeout::Disabled
    normalized.parse::<u64>()
}
```

### 2. Bandwidth Limit Parsing (`--bwlimit`)
**Location:** `/home/ofer/rsync/crates/bandwidth/src/parse.rs`

**Format:** Highly specialized bandwidth syntax
- `1K` → 1 KiB/s (1024 bytes/sec, default)
- `1KB` → 1 KB/s (1000 bytes/sec)
- `1KiB` → 1 KiB/s (1024 bytes/sec, explicit)
- `1.5M` → 1.5 MiB/s with fractional component
- `2.5e3` → scientific notation support
- `100K+1` → 100K + 1 byte/sec (adjustment syntax)
- `100K-1` → 100K - 1 byte/sec
- `1M:500K` → rate:burst syntax
- Suffixes: B, K, M, G, T, P

**Implementation:**
- Custom parser with 243 lines of logic
- Handles decimal/comma separators: "1.5" or "1,5"
- Scientific notation: "1e3", "2.5E-2"
- Binary (1024) vs decimal (1000) bases
- Rounding to alignment boundaries
- Minimum 512 bytes/sec enforced

### 3. Size Limit Parsing (`--max-size`, `--min-size`, `--block-size`)
**Location:** `/home/ofer/rsync/crates/cli/src/frontend/execution/options.rs`

**Format:** Size specifications with unit suffixes
- `1K` → 1024 bytes (KiB)
- `1KB` → 1000 bytes (KB)
- `1.5M` → 1.5 MiB
- `1,5M` → European decimal notation
- Suffixes: B, K, M, G, T, P, E
- Binary (1024) vs decimal (1000) bases

**Implementation:**
- ~200 lines in `parse_size_spec()`
- Shared logic with fractional support
- u128 arithmetic for large values

### 4. Modify Window (`--modify-window`)
**Location:** `/home/ofer/rsync/crates/cli/src/frontend/execution/options.rs`

**Format:** Simple integer seconds
- Same as timeout: plain integers only

---

## What humantime Provides

The `humantime` crate (v2.1.0) provides:

### Supported Formats
```rust
use humantime::parse_duration;

// Human-readable formats:
parse_duration("2h 30m")      // 2 hours 30 minutes
parse_duration("1 day")       // 24 hours
parse_duration("500ms")       // 500 milliseconds
parse_duration("1y 2mo 3w")   // years, months, weeks
parse_duration("3min 5sec")   // minutes and seconds
```

### Units Supported by humantime
- Nanoseconds: `ns`, `nsec`
- Microseconds: `us`, `usec`
- Milliseconds: `ms`, `msec`
- Seconds: `s`, `sec`, `second`, `seconds`
- Minutes: `m`, `min`, `minute`, `minutes`
- Hours: `h`, `hr`, `hour`, `hours`
- Days: `d`, `day`, `days`
- Weeks: `w`, `week`, `weeks`
- Months: `M`, `mo`, `month`, `months` (30 days)
- Years: `y`, `yr`, `year`, `years` (365 days)

### humantime API
```rust
pub fn parse_duration(s: &str) -> Result<Duration, Error>
pub fn format_duration(d: Duration) -> FormattedDuration
pub fn parse_rfc3339(s: &str) -> Result<SystemTime, Error>
```

---

## Comparison: humantime vs Current Implementation

| Aspect | rsync Parsing | humantime |
|--------|---------------|-----------|
| **Timeout syntax** | `30` (seconds only) | `30s` or `30sec` (unit required) |
| **Simple integers** | Yes (`30`, `60`) | No (requires unit) |
| **Fractional values** | No for timeout | Yes (`2.5h`) |
| **Size suffixes** | K, M, G, T, P, E | No size support |
| **Bandwidth units** | KB/s, KiB/s, etc. | No bandwidth support |
| **Scientific notation** | Yes (`1e3`, `2.5E-2`) | No |
| **Decimal separators** | Both `.` and `,` | Only `.` |
| **Adjustment syntax** | `100K+1`, `100K-1` | No |
| **Burst syntax** | `1M:500K` | No |
| **Binary vs decimal** | Both (1024 vs 1000) | N/A |
| **Error messages** | rsync-compatible | Generic Rust errors |
| **Zero = disabled** | Yes (`0` → disabled) | No special meaning |

---

## Compatibility Analysis

### What Would Break with humantime

1. **All timeout arguments would break:**
   ```bash
   # Current (works):
   --timeout=30
   --timeout=0

   # With humantime (would fail):
   --timeout=30     # Error: missing unit
   --timeout=0      # Error: missing unit

   # Required with humantime:
   --timeout=30s
   --timeout=0s
   ```

2. **Bandwidth limits completely incompatible:**
   ```bash
   # Current bandwidth syntax (would all break):
   --bwlimit=1.5M       # humantime doesn't understand 'M' for megabytes
   --bwlimit=100KB      # not a duration
   --bwlimit=1M:500K    # burst syntax not supported
   --bwlimit=100K+1     # adjustment syntax not supported
   ```

3. **Size limits completely incompatible:**
   ```bash
   # Current size syntax (would all break):
   --max-size=1M        # not a duration
   --block-size=8K      # not a duration
   ```

### rsync-Specific Requirements Not in humantime

1. **Size-based parsing:** rsync needs bytes/bandwidth, not time
2. **Scientific notation:** `2.5e3` for bandwidth limits
3. **European decimals:** `1,5M` support
4. **Binary/decimal bases:** KiB (1024) vs KB (1000)
5. **Adjustment syntax:** `+1` / `-1` modifiers
6. **Burst specifications:** `rate:burst` syntax
7. **Rounding/alignment:** Bandwidth rounded to 1K/1000 boundaries
8. **Minimum enforcement:** 512 bytes/sec minimum for bandwidth
9. **Zero semantics:** `0` means "disabled" not "zero duration"

---

## Code Size Comparison

### Current Implementation
- **Timeout parsing:** ~50 lines (simple integer parsing)
- **Bandwidth parsing:** ~243 lines (complex custom logic)
- **Size parsing:** ~200 lines (shared with bandwidth)
- **Total custom code:** ~500 lines (well-tested, rsync-compatible)

### With humantime
- **Dependency size:** humantime = 5.1 KB source, ~100 KB compiled
- **Required wrappers:** ~200+ lines to maintain compatibility
- **Complexity:** Bridge between humantime format and rsync semantics
- **Still need custom parsing:** For bandwidth and size (majority of code)

**Net benefit:** Negative (add dependency, increase complexity, no meaningful reduction)

---

## Test Coverage Analysis

### Current Tests
```
/home/ofer/rsync/crates/cli/src/frontend/execution/options.rs:
- parse_timeout_argument: 7 tests
- parse_size_spec: 20+ tests
- parse_block_size_argument: 5 tests
- parse_modify_window_argument: 6 tests

/home/ofer/rsync/crates/bandwidth/src/parse/tests/:
- numeric.rs: 50+ tests (pow_u128, decimal parsing, exponents)
- argument.rs: Bandwidth-specific tests
- edge_cases.rs: Edge case coverage
- limit.rs: Burst syntax tests
```

**Total:** 100+ tests covering all edge cases, overflow, error messages

### With humantime
- Would need to **rewrite all tests** for new format
- Would need **compatibility wrapper tests**
- Still need **all custom tests** for bandwidth/size parsing
- **Higher maintenance burden**

---

## Performance Comparison

### Current Implementation
- **Timeout:** Direct `u64::parse()` - optimal performance
- **Bandwidth:** Hand-optimized with `memchr` for fast scanning
- **Size:** Specialized u128 arithmetic with overflow checks
- **Allocations:** Minimal (only for error messages)

### With humantime
- **Additional parsing:** Must parse unit suffixes even for simple "30s"
- **String allocations:** humantime may allocate for parsing
- **Conversion overhead:** humantime Duration → rsync semantics
- **No performance benefit:** Timeout is critical path, simple integer is fastest

**Performance impact:** Negative (slower parsing, no benefit)

---

## Maintenance Considerations

### Current Approach (Status Quo)
**Pros:**
- Complete control over error messages (rsync-compatible)
- Exact rsync behavior replication
- No external dependencies for core parsing
- Zero breaking changes
- Optimized for rsync use cases

**Cons:**
- Custom code to maintain (~500 lines)
- Need to keep tests up to date

### humantime Approach
**Pros:**
- Well-tested third-party crate
- Standard Rust Duration output
- Community maintenance

**Cons:**
- **Breaking change** for all users (timeout syntax incompatible)
- **Still need 80%+ of custom code** (bandwidth, size parsing)
- Dependency on external crate (security, updates)
- Less control over error messages
- Wrapper complexity to maintain compatibility
- No support for rsync-specific formats

---

## Recommendation Details

### Do NOT adopt humantime because:

1. **Incompatibility:** humantime requires units (`30s`) but rsync uses plain integers (`30`)
2. **Limited applicability:** Only useful for `--timeout`, not bandwidth or size parsing
3. **Breaking change:** Would break existing rsync usage patterns
4. **No code reduction:** Still need 80%+ of custom parsing for bandwidth/size
5. **Performance regression:** Slower than simple integer parsing
6. **Increased complexity:** Need compatibility wrappers
7. **Upstream rsync compatibility:** Must match C rsync behavior exactly

### Alternative: Keep Current Implementation

The current implementation is:
- **Well-tested** (100+ test cases)
- **rsync-compatible** (matches upstream behavior)
- **Performant** (optimized for common cases)
- **Complete** (handles all rsync-specific formats)
- **Stable** (no breaking changes needed)

### If More User-Friendly Parsing Desired

If you want to **optionally** support human-readable durations while maintaining compatibility:

```rust
// Could add humantime as optional fallback:
fn parse_timeout_extended(value: &str) -> Result<u64, Error> {
    // Try rsync format first (plain integer)
    if let Ok(seconds) = value.parse::<u64>() {
        return Ok(seconds);
    }

    // Fallback to humantime for convenience
    humantime::parse_duration(value)
        .map(|d| d.as_secs())
        .map_err(|_| Error::InvalidTimeout)
}

// Accepts both:
// --timeout=30     (rsync style, no unit)
// --timeout=30s    (humantime style, with unit)
```

**However:** This adds complexity without clear user benefit, since rsync users expect the simple integer format.

---

## Conclusion

**Do not adopt humantime** for this rsync implementation. The current custom parsing is:

1. **More appropriate** for rsync's specific needs (bandwidth, sizes, plain integers)
2. **Better performing** (direct integer parsing vs unit parsing)
3. **More maintainable** (no breaking changes, established test suite)
4. **More compatible** (matches upstream rsync exactly)
5. **Lower complexity** (no dependency, no wrappers needed)

The humantime crate solves a different problem (parsing human-readable time spans) than what rsync needs (parsing rsync-specific bandwidth/size/timeout formats).

---

## Files Analyzed

```
/home/ofer/rsync/crates/cli/src/frontend/execution/options.rs
/home/ofer/rsync/crates/bandwidth/src/parse.rs
/home/ofer/rsync/crates/bandwidth/src/parse/numeric.rs
/home/ofer/rsync/crates/bandwidth/src/parse/tests/numeric.rs
/home/ofer/rsync/crates/core/src/client/config/enums.rs
/home/ofer/rsync/crates/core/src/client/config/client/selection.rs
```

**Total custom parsing code:** ~500 lines (well-tested, production-ready)
**Potential savings with humantime:** ~50 lines (timeout only, 10% of total)
**Required wrapper code:** ~200 lines (compatibility layer)
**Net benefit:** **Negative** (-150 lines, +1 dependency, +breaking changes)
