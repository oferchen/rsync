# FCV-20.c - Fuzz target coverage matrix

Framework for measuring, tracking, and improving code coverage achieved by
each fuzz target in the `fuzz/` workspace. This document defines the matrix
schema, threshold criteria, regeneration procedure, and prioritised gap list.

## 1. Coverage matrix

The matrix below tracks per-target coverage metrics. Columns:

- **Target** - cargo-fuzz binary name from `fuzz/Cargo.toml`
- **Category** - classification for threshold selection
- **Lines** - source lines exercised (llvm-cov line count)
- **Branches** - conditional branches taken (llvm-cov branch count)
- **Edge %** - edge coverage percentage reported by libFuzzer
- **Corpus** - number of seed inputs in `fuzz/corpus/<target>/`
- **Last run** - date of most recent coverage collection

### 1.1 Protocol targets (threshold: >90% edge coverage)

| Target                    | Category | Lines | Branches | Edge % | Corpus | Last run |
| ------------------------- | -------- | ----- | -------- | ------ | ------ | -------- |
| `protocol_wire`           | protocol | -     | -        | -      | 0      | -        |
| `multiplex_frame_parse`   | protocol | -     | -        | -      | 0      | -        |
| `flist_entry_decode`      | protocol | -     | -        | -      | 0      | -        |
| `incremental_flist`       | protocol | -     | -        | -      | 0      | -        |
| `ndx_codec`              | protocol | -     | -        | -      | 0      | -        |
| `varint_decode`           | protocol | -     | -        | -      | 14     | -        |
| `legacy_greeting`         | protocol | -     | -        | -      | 0      | -        |
| `daemon_greeting`         | protocol | -     | -        | -      | 0      | -        |

### 1.2 Parser targets (threshold: >80% edge coverage)

| Target                    | Category | Lines | Branches | Edge % | Corpus | Last run |
| ------------------------- | -------- | ----- | -------- | ------ | ------ | -------- |
| `filter_list_wire`        | parser   | -     | -        | -      | 0      | -        |
| `rsyncd_conf`             | parser   | -     | -        | -      | 0      | -        |
| `capability_flags`        | parser   | -     | -        | -      | 0      | -        |
| `auth_response`           | parser   | -     | -        | -      | 0      | -        |
| `vstring`                 | parser   | -     | -        | -      | 0      | -        |
| `batch_reader`            | parser   | -     | -        | -      | 0      | -        |
| `acl_xattr_wire`          | parser   | -     | -        | -      | 0      | -        |
| `bwlimit`                 | parser   | -     | -        | -      | 0      | -        |

### 1.3 Codec targets (threshold: >70% edge coverage)

| Target                    | Category | Lines | Branches | Edge % | Corpus | Last run |
| ------------------------- | -------- | ----- | -------- | ------ | ------ | -------- |
| `decompressor_zlib`       | codec    | -     | -        | -      | 0      | -        |
| `decompressor_zstd`       | codec    | -     | -        | -      | 0      | -        |
| `simd_checksum_parity`    | codec    | -     | -        | -      | 0      | -        |
| `zlib_token_decode`       | codec    | -     | -        | -      | 0      | -        |

### 1.4 Differential targets (threshold: >80% edge coverage)

| Target                      | Category     | Lines | Branches | Edge % | Corpus | Last run |
| --------------------------- | ------------ | ----- | -------- | ------ | ------ | -------- |
| `filter_differential`       | differential | -     | -        | -      | 0      | -        |
| `filter_rules_vs_upstream`  | differential | -     | -        | -      | 0      | -        |
| `differential_outcome`      | differential | -     | -        | -      | 0      | -        |
| `differential_multiplex`    | differential | -     | -        | -      | 0      | -        |
| `differential_flist`        | differential | -     | -        | -      | 0      | -        |

## 2. Threshold criteria

Coverage thresholds are set per target category based on the attack surface
each category represents:

| Category     | Edge % threshold | Rationale                                                 |
| ------------ | ---------------- | --------------------------------------------------------- |
| protocol     | > 90%            | Parses untrusted network bytes from remote peers          |
| parser       | > 80%            | Parses config files or structured data from local sources |
| codec        | > 70%            | Compression/checksum - correctness via parity assertions  |
| differential | > 80%            | Behavioural equivalence with upstream - oracle-based      |

A target is considered **adequate** when its edge coverage meets or exceeds
the threshold for its category. Targets below threshold appear in the
prioritised gap list (section 3).

## 3. Prioritised gap list

Ordered by severity (protocol > parser > differential > codec) and then by
distance below threshold. Update this list after each coverage run.

| Priority | Target                  | Category | Current edge % | Gap   | Recommended action                |
| -------- | ----------------------- | -------- | -------------- | ----- | --------------------------------- |
| P1       | `protocol_wire`         | protocol | -              | -     | Seed corpus from golden captures  |
| P2       | `multiplex_frame_parse` | protocol | -              | -     | Structured input via `Arbitrary`  |
| P3       | `flist_entry_decode`    | protocol | -              | -     | Seed from interop captures        |
| P4       | `incremental_flist`     | protocol | -              | -     | Generate multi-segment INC corpus |
| P5       | `filter_list_wire`      | parser   | -              | -     | Expand rule-grammar seeds         |
| P6       | `rsyncd_conf`           | parser   | -              | -     | Real-world rsyncd.conf samples    |

Targets with existing corpora (e.g., `varint_decode` with 14 seeds) may
already be adequate - verify by running coverage collection.

## 4. Regeneration procedure

Coverage data is collected via `cargo fuzz coverage` which instruments the
target with source-based coverage and replays the entire corpus.

### 4.1 Prerequisites

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
# llvm-tools for coverage report generation
rustup +nightly component add llvm-tools-preview
```

### 4.2 Per-target coverage collection

```bash
# Run from repository root
TARGET=protocol_wire

# Generate raw coverage profile
cargo +nightly fuzz coverage "$TARGET" fuzz/corpus/"$TARGET"/

# Coverage profile lands at:
#   fuzz/coverage/$TARGET/coverage.profdata

# Generate human-readable report
FUZZ_BIN=$(find target -path "*/release/$TARGET" -type f | head -1)
llvm-cov report "$FUZZ_BIN" \
    -instr-profile=fuzz/coverage/"$TARGET"/coverage.profdata \
    --ignore-filename-regex='\.cargo|rustc'

# For detailed line-by-line HTML:
llvm-cov show "$FUZZ_BIN" \
    -instr-profile=fuzz/coverage/"$TARGET"/coverage.profdata \
    --format=html \
    --output-dir=fuzz/coverage/"$TARGET"/html \
    --ignore-filename-regex='\.cargo|rustc'
```

### 4.3 All-targets sweep

```bash
#!/usr/bin/env bash
set -euo pipefail

TARGETS=(
    protocol_wire multiplex_frame_parse flist_entry_decode incremental_flist
    ndx_codec varint_decode legacy_greeting daemon_greeting
    filter_list_wire rsyncd_conf capability_flags auth_response
    vstring batch_reader acl_xattr_wire bwlimit
    decompressor_zlib decompressor_zstd simd_checksum_parity
    filter_differential filter_rules_vs_upstream
    differential_outcome differential_multiplex differential_flist
)

for target in "${TARGETS[@]}"; do
    echo "=== Coverage: $target ==="
    corpus_dir="fuzz/corpus/$target"
    if [ ! -d "$corpus_dir" ] || [ -z "$(ls -A "$corpus_dir" 2>/dev/null)" ]; then
        echo "  SKIP - no corpus"
        continue
    fi
    cargo +nightly fuzz coverage "$target" "$corpus_dir" 2>&1 | tail -5
    echo ""
done
```

### 4.4 Extracting metrics for the matrix

After running coverage for a target, extract line/branch/edge counts:

```bash
# Lines and branches from llvm-cov report
llvm-cov report "$FUZZ_BIN" \
    -instr-profile=fuzz/coverage/"$TARGET"/coverage.profdata \
    --ignore-filename-regex='\.cargo|rustc' \
    | tail -1
# Output format: TOTAL  <regions> <missed> <cover%> <functions> <missed> <cover%> <lines> <missed> <cover%> <branches> <missed> <cover%>

# Corpus size
find "fuzz/corpus/$TARGET" -type f | wc -l

# Edge coverage from libFuzzer stats (available during fuzz run, not coverage)
# Captured from the last line of `cargo fuzz run` output
```

## 5. Coverage improvement recommendations

### 5.1 Protocol targets

Protocol targets parse bytes from untrusted network peers. Improvements:

1. **Seed from golden captures** - extract wire bytes from
   `crates/protocol/tests/golden/` test fixtures into corpus files.
2. **Seed from interop** - run `tools/ci/run_interop.sh` with tcpdump capture,
   split pcaps into per-frame corpus entries.
3. **Structured fuzzing** - use `arbitrary::Arbitrary` to generate valid
   protocol structures, then encode them as seed inputs.
4. **Dictionary** - provide a libFuzzer dictionary with protocol magic bytes,
   common varints, and frame type tags.

### 5.2 Parser targets

Parser targets handle locally-sourced structured data. Improvements:

1. **Real-world samples** - collect rsyncd.conf files, filter rule files, and
   ACL wire dumps from production deployments.
2. **Grammar-based generation** - write an `Arbitrary` impl that produces
   syntactically plausible inputs (valid modifiers, balanced brackets).
3. **Minimise existing crashes** - run `cargo fuzz tmin` on any findings to
   produce minimal reproduction cases that become corpus entries.

### 5.3 Codec targets

Codec targets exercise compression and checksum paths. Improvements:

1. **Cross-check corpus** - for `simd_checksum_parity`, generate inputs
   that exercise SIMD lane boundary conditions (lengths 1-64 in steps of 1).
2. **Compressed stream seeds** - for decompressor targets, compress known
   payloads with upstream zlib/zstd at various levels and truncate at
   different offsets to generate partial-stream corpus entries.
3. **Round-trip invariant** - compress then decompress; any divergence from
   identity is a finding.

### 5.4 Differential targets

Differential targets compare behaviour against upstream rsync. Improvements:

1. **Expand flag combinations** - the `Arbitrary` input should cover more
   of the rsync flag space (currently limited to deterministic flags).
2. **Larger file trees** - increase the file-count and nesting-depth caps
   in structured input generation.
3. **Edge-case file content** - empty files, sparse regions, binary data
   with NUL bytes, maximum-length paths.

## 6. Cross-references

- **FCV-17**: Initial fuzz target inventory and corpus gap analysis.
- **FCV-18**: Seed corpus generation for protocol parser targets.
- **FCV-19**: Structured input generation for differential targets.
- **WDF series**: Wire differential fuzzing design
  (`docs/design/wdf-differential-fuzzing.md`).
- **Wire format differential fuzzer**:
  `docs/design/wire-format-differential-fuzzer.md`.
- **Protocol fuzzing harness**: `docs/design/protocol-fuzzing-harness.md`.
- **Differential filter fuzzer**: `docs/design/differential-filter-fuzzer.md`.

## 7. Maintenance cadence

- Run the all-targets sweep weekly (or after significant parser/protocol
  changes).
- Update this matrix after each sweep.
- Targets that regress below threshold trigger a P1 corpus-expansion task.
- New fuzz targets added to `fuzz/Cargo.toml` must be added to this matrix
  within the same PR.
