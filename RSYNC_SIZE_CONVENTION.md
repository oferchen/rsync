# rsync Size Parsing Convention

## Overview

This document explains rsync's unique size/bandwidth parsing convention, which differs significantly from standard conventions used in most libraries.

## The Core Convention: K vs KB

### rsync's Rule
```
K alone    = 1024 (binary, base-2)
KB/Kb/kB   = 1000 (decimal, base-10, SI units)
KiB/kiB    = 1024 (explicit IEC binary units)
```

### Why This Is Unusual

Most systems use one of these conventions:

**Convention 1: SI (Decimal)**
- K = 1000
- KB = 1000
- KiB = 1024

**Convention 2: Traditional Computing (Binary)**
- K = 1024
- KB = 1024
- KiB = 1024

**Convention 3: Strict IEC**
- K = invalid/ambiguous
- KB = 1000 (SI decimal)
- KiB = 1024 (binary)

**rsync's Convention (Unique):**
- K = 1024 (binary default)
- KB = 1000 (adding 'B' switches to decimal)
- KiB = 1024 (explicit binary)

This means **adding a 'B' changes the multiplier** from 1024 to 1000, which is the opposite of typical expectations.

## Complete Unit Reference

### Single-Letter Units (Binary, 1024-based)

| Input | Value | Calculation |
|-------|-------|-------------|
| `1K` | 1,024 | 1 × 1024¹ |
| `1M` | 1,048,576 | 1 × 1024² |
| `1G` | 1,073,741,824 | 1 × 1024³ |
| `1T` | 1,099,511,627,776 | 1 × 1024⁴ |
| `1P` | 1,125,899,906,842,624 | 1 × 1024⁵ |

### Double-Letter Units with 'B' (Decimal, 1000-based)

| Input | Value | Calculation |
|-------|-------|-------------|
| `1KB` | 1,000 | 1 × 1000¹ |
| `1MB` | 1,000,000 | 1 × 1000² |
| `1GB` | 1,000,000,000 | 1 × 1000³ |
| `1TB` | 1,000,000,000,000 | 1 × 1000⁴ |
| `1PB` | 1,000,000,000,000,000 | 1 × 1000⁵ |

### IEC Units (Binary, 1024-based, Explicit)

| Input | Value | Calculation |
|-------|-------|-------------|
| `1KiB` | 1,024 | 1 × 1024¹ |
| `1MiB` | 1,048,576 | 1 × 1024² |
| `1GiB` | 1,073,741,824 | 1 × 1024³ |
| `1TiB` | 1,099,511,627,776 | 1 × 1024⁴ |
| `1PiB` | 1,125,899,906,842,624 | 1 × 1024⁵ |

### Byte Units (Identity)

| Input | Value | Calculation |
|-------|-------|-------------|
| `100B` | 100 | 100 × 1 |
| `100b` | 100 | 100 × 1 (case insensitive) |

## Examples

### Basic Examples
```rust
"1K"    → 1,024 bytes         // Binary by default
"1KB"   → 1,000 bytes         // Decimal with B suffix
"1KiB"  → 1,024 bytes         // Explicit binary (IEC)
"1M"    → 1,048,576 bytes     // Binary
"1MB"   → 1,000,000 bytes     // Decimal
"1G"    → 1,073,741,824 bytes // Binary
"1GB"   → 1,000,000,000 bytes // Decimal
```

### Case Insensitivity
```rust
// Single letter: case doesn't matter
"1K"  → 1,024
"1k"  → 1,024

// With B: any case combination works for decimal
"1KB" → 1,000
"1Kb" → 1,000
"1kB" → 1,000
"1kb" → 1,000

// IEC: case doesn't matter
"1KiB" → 1,024
"1kib" → 1,024
"1KIB" → 1,024
```

### Fractional Values
```rust
"1.5K"   → 1,536 bytes        // 1.5 × 1024
"0.5M"   → 524,288 bytes      // 0.5 × 1024²
"2.25G"  → 2,415,919,104      // 2.25 × 1024³
"1.5KB"  → 1,500 bytes        // 1.5 × 1000 (decimal)
".5M"    → 524,288 bytes      // Leading decimal point OK
"1."     → 1,024 bytes        // Trailing decimal point OK
```

### Comma Decimal Separator
```rust
"1,5K"   → 1,536 bytes        // Same as 1.5K
"2,25M"  → 2,359,296 bytes    // Same as 2.25M
```

## Advanced Features

### Scientific Notation
```rust
"1e3"      → 1,024,000 bytes      // 1000 × 1024 (default K)
"1e3b"     → 1,000 bytes          // 1000 × 1 (byte unit)
"1e3K"     → 1,024,000 bytes      // 1000 × 1024
"1e3KB"    → 1,000,000 bytes      // 1000 × 1000 (decimal)
"2.5e2M"   → 262,144,000 bytes    // 250 × 1024²
"1e-1M"    → 104,448 bytes        // 0.1 × 1024²
"1e+5K"    → 102,400,000 bytes    // 100000 × 1024
"1.5e2G"   → 161,061,273,600      // 150 × 1024³
```

**Note:** Exponent is applied to the numeric part first, then multiplied by the unit.

Formula: `value = (number × 10^exponent) × unit_multiplier`

### Adjustment Modifiers (±1)
```rust
"1K+1"     → 1,025 bytes          // 1024 + 1
"1K-1"     → 1,023 bytes          // 1024 - 1
"600b+1"   → 601 bytes            // 600 + 1
"600b-1"   → 599 bytes            // 600 - 1
"1.5M+1"   → 1,572,865 bytes      // (1.5 × 1024²) + 1
```

**Use case:** Fine-tuning bandwidth limits for edge cases.

**Restrictions:**
- Only `+1` or `-1` allowed (not `+2`, `+10`, etc.)
- Must be at the very end of the input
- No spaces allowed (`"1K +1"` is invalid)

### Default Unit for Bandwidth

When parsing bandwidth limits (not size limits), a bare number defaults to kilobytes:

```rust
// In bandwidth parsing context:
"1000"     → 1,024,000 bytes      // Same as "1000K"
"500"      → 512,000 bytes        // Same as "500K"

// In size limit parsing context:
"1000"     → 1,000 bytes          // Just the number
```

This is context-dependent and affects `parse_bandwidth_argument()` but not `parse_size_limit_argument()`.

### Rounding and Alignment

Different units have different alignment requirements:

**Binary units (K, M, G, T, P):**
- Round to nearest 1024-byte boundary
- `0.5K` → 1,024 (rounds up from 512)
- `0.1M` → 104,448 (rounds to nearest 1024)

**Decimal units (KB, MB, GB, TB, PB):**
- Round to nearest 1000-byte boundary
- `0.5KB` → 1,000 (rounds up from 500)

**Byte units (B):**
- No rounding (1-byte alignment)
- `512b` → 512 (exact)

### Special Values

**Zero (Unlimited):**
```rust
"0"        → None (unlimited)
"0K"       → None (unlimited)
"0M"       → None (unlimited)
"0.0"      → None (unlimited)
```

In bandwidth contexts, zero means "no limit" rather than "zero bandwidth".

**Minimum Value (Bandwidth Only):**
```rust
"512b"     → 512 bytes/sec (minimum allowed)
"511b"     → Error: TooSmall
"100"      → Error: TooSmall (< 512 after default K unit)
```

Bandwidth limits enforce a 512 bytes/second minimum. Size limits have no minimum.

## Error Cases

### Invalid Syntax
```rust
"1 K"      → Error (space not allowed)
"1_000K"   → Error (underscores not allowed)
"K"        → Error (no number)
"1Q"       → Error (invalid unit)
"1KX"      → Error (invalid suffix)
"abc"      → Error (not a number)
"1.2.3K"   → Error (multiple decimal points)
"1Ki"      → Error (incomplete IEC suffix)
```

### Negative Values
```rust
"-1K"      → Error (bandwidth/size cannot be negative)
"-100"     → Error (negative not allowed)
```

### Empty/Whitespace
```rust
""         → Error (empty input)
"   "      → Error (whitespace only)
"\t"       → Error (whitespace only)
```

Note: Surrounding whitespace is also rejected:
```rust
"  1K  "   → Error (must be exact, no trimming)
```

### Overflow
```rust
"999999999999999P"  → Error (exceeds u64::MAX)
"1e2000M"           → Error (exponent too large)
```

## Comparison with Standard Conventions

| Input | rsync | Linux `dd` | macOS | IEC Standard | SI Standard |
|-------|-------|------------|-------|--------------|-------------|
| `1K` | 1024 | 1000 | 1024 | Ambiguous | 1000 |
| `1KB` | 1000 | 1000 | 1000 | 1000 | 1000 |
| `1KiB` | 1024 | N/A | 1024 | 1024 | N/A |
| `1M` | 1048576 | 1000000 | 1048576 | Ambiguous | 1000000 |
| `1MB` | 1000000 | 1000000 | 1000000 | 1000000 | 1000000 |
| `1MiB` | 1048576 | N/A | 1048576 | 1048576 | N/A |

**Key Difference:** rsync's `K` = 1024 is unusual. Most modern tools treat `K` as 1000 (SI) unless explicitly using binary suffixes.

## Why This Convention?

Historical reasons:
1. **Backwards compatibility** with original rsync behavior
2. **User expectations** from older computing conventions (1 KB = 1024 bytes)
3. **Convenience** for binary-aligned buffers (network/disk I/O)
4. **Flexibility** to specify both binary and decimal units explicitly

The convention prioritizes compatibility with existing rsync scripts and user expectations over following modern standards.

## Code Locations

**Implementation:**
- `/home/ofer/rsync/crates/bandwidth/src/parse.rs` - Bandwidth parsing with this convention
- `/home/ofer/rsync/crates/cli/src/frontend/execution/options.rs` - Size limit parsing

**Tests:**
- `/home/ofer/rsync/crates/bandwidth/src/parse/tests/argument.rs` - Comprehensive test coverage
- `/home/ofer/rsync/crates/cli/src/frontend/tests/parse_size.rs` - Size parsing tests

**Test count:** 150+ test cases covering all edge cases and conventions.

## Summary

rsync uses a unique size parsing convention where:
- Single-letter units (K, M, G) default to binary (1024-based)
- Adding 'B' switches to decimal (1000-based)
- Explicit IEC units (KiB, MiB) are binary
- Supports scientific notation, fractional values, and adjustment modifiers
- Context-dependent default units (bandwidth defaults to KB)
- Special handling for zero (unlimited) and minimum values

This convention is **not compatible** with standard parsing libraries and requires custom implementation.
