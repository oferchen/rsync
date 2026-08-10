# oc-rsync Fuzz Harnesses

Top-level [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) workspace for
the parts of oc-rsync that consume bytes from untrusted peers. The protocol
parser sits behind every network connection, so any panic uncovered here is a
potential remote denial-of-service vector at minimum.

A per-crate fuzz workspace also exists at `crates/protocol/fuzz/` with more
specialised targets (varint, delta, legacy greeting). The harness in this
directory is the recommended starting point for new contributors and for CI
integration.

## Prerequisites

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
```

`cargo-fuzz` requires nightly Rust because it links against libFuzzer using
unstable compiler flags.

## Available targets

| Target                 | Entry point                                                              |
| ---------------------- | ------------------------------------------------------------------------ |
| `protocol_wire`        | `protocol::BorrowedMessageFrames` (multiplex frames)                     |
| `simd_checksum_parity` | `checksums` rolling + MD4/MD5 batch SIMD dispatchers vs scalar reference |
| `filter_differential`  | `filters::FilterSet` decisions vs upstream rsync 3.4.2 (spawns rsync)    |
| `differential_outcome` | End-to-end local-copy outcome comparison vs upstream rsync (spawns both) |

## Running

From the repository root:

```bash
cargo +nightly fuzz run protocol_wire
cargo +nightly fuzz run simd_checksum_parity
cargo +nightly fuzz run filter_differential
```

`filter_differential` shells out to a real `rsync` binary per input, so
throughput is roughly 200-500 exec/sec versus 100k+ for the in-process
targets. Use `tools/ci/run_filter_fuzz.sh` for a wrapped invocation with
binary discovery and a configurable duration.

Useful flags:

```bash
# Cap a single run at 60 seconds (handy for smoke checks).
cargo +nightly fuzz run protocol_wire -- -max_total_time=60

# Run with a corpus directory of seed inputs.
mkdir -p fuzz/corpus/protocol_wire
cargo +nightly fuzz run protocol_wire fuzz/corpus/protocol_wire
```

Crashes are written to `fuzz/artifacts/<target>/`. Reproduce a crash by
re-running the target with the artifact path appended.

### `simd_checksum_parity`

Cross-checks every runtime-selected SIMD checksum dispatcher
(`RollingChecksum::update`, `strong::md5_digest_batch`,
`strong::md4_digest_batch`) against an independent scalar reference for each
arbitrary libFuzzer input. The on-tree `checksums::run_simd_self_test` uses a
fixed input sweep; this target relies on coverage-guided fuzzing to reach
shapes outside that sweep (odd remainders past SIMD lane boundaries, repeated
byte patterns, adversarial transitions across MD4/MD5 block boundaries, ...).
Any byte-level disagreement panics, which libFuzzer records as a finding
under `fuzz/artifacts/simd_checksum_parity/`.

## Adding more targets

Drop a new file into `fuzz/fuzz_targets/` and add a matching `[[bin]]` entry
in `fuzz/Cargo.toml`. Keep targets minimal: a `fuzz_target!` block that hands
the input slice to a single public parser function is enough. libFuzzer's
coverage-guided search takes care of reaching deeper code paths.

Good candidates for future targets:

- File list decoding (`protocol::flist`)
- Varint and varlong decoders
- ACL and xattr wire decoders

## Workspace integration

The `fuzz/` crate is intentionally not part of the root Cargo workspace.
`libfuzzer-sys` requires nightly-only linker flags and would otherwise break
`cargo build` for ordinary contributors. It is listed under `workspace.exclude`
in the root `Cargo.toml`.
