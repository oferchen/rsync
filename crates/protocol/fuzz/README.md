# Protocol Fuzzing

This directory contains fuzz targets for the protocol crate's wire parsing functions.
These targets use [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) with libFuzzer.

## Prerequisites

Install cargo-fuzz:

```bash
cargo install cargo-fuzz
```

Note: cargo-fuzz requires a nightly Rust toolchain.

## Available Fuzz Targets

| Target | Description |
|--------|-------------|
| `fuzz_varint` | Variable-length integer encoding (read_varint, read_varlong, read_varlong30) |
| `fuzz_delta` | Delta wire protocol (read_delta, read_token, read_signature) |
| `fuzz_multiplex_frame` | Multiplexed I/O frames (MessageHeader, BorrowedMessageFrame) |
| `fuzz_legacy_greeting` | Legacy daemon greeting parsing |

## Running Fuzz Tests

From the `crates/protocol/fuzz` directory:

```bash
# Run a specific target
cargo +nightly fuzz run fuzz_varint

# Run with a time limit (seconds)
cargo +nightly fuzz run fuzz_varint -- -max_total_time=60

# Run all targets in sequence
for target in fuzz_varint fuzz_delta fuzz_multiplex_frame fuzz_legacy_greeting; do
    cargo +nightly fuzz run "$target" -- -max_total_time=60
done
```

## Corpus Management

Fuzzing corpora are stored in `corpus/<target>/`. To seed with interesting inputs:

```bash
mkdir -p corpus/fuzz_varint
# Add seed files...
```

## Coverage-Guided Fuzzing

For coverage reports, use cargo-fuzz's coverage feature:

```bash
cargo +nightly fuzz coverage fuzz_varint
```

## Security Considerations

These fuzz targets exercise critical attack surfaces:

1. **Varint parsing**: Malformed variable-length integers could cause
   integer overflows or excessive memory allocation
2. **Delta operations**: Untrusted deltas could specify out-of-bounds
   copies or trigger excessive memory usage
3. **Multiplex frames**: Malicious frame headers could specify invalid
   message codes or oversized payloads
4. **Legacy greetings**: Protocol negotiation parsing handles untrusted
   server responses

All parsing functions should:
- Return errors (not panic) on malformed input
- Bound memory allocations
- Validate all indices and sizes before use
