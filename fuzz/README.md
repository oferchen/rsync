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

| Target          | Entry point                                          |
| --------------- | ---------------------------------------------------- |
| `protocol_wire` | `protocol::BorrowedMessageFrames` (multiplex frames) |

## Running

From the repository root:

```bash
cargo +nightly fuzz run protocol_wire
```

Useful flags:

```bash
# Cap a single run at 60 seconds (handy for smoke checks).
cargo +nightly fuzz run protocol_wire -- -max_total_time=60

# Run with a corpus directory of seed inputs.
mkdir -p fuzz/corpus/protocol_wire
cargo +nightly fuzz run protocol_wire fuzz/corpus/protocol_wire
```

Crashes are written to `fuzz/artifacts/protocol_wire/`. Reproduce a crash by
re-running the target with the artifact path appended.

## Adding more targets

Drop a new file into `fuzz/fuzz_targets/` and add a matching `[[bin]]` entry
in `fuzz/Cargo.toml`. Keep targets minimal: a `fuzz_target!` block that hands
the input slice to a single public parser function is enough. libFuzzer's
coverage-guided search takes care of reaching deeper code paths.

Good candidates for future targets:

- File list decoding (`protocol::flist`)
- Filter rule parsing (`protocol::filters`)
- Varint and varlong decoders
- ACL and xattr wire decoders

## Workspace integration

The `fuzz/` crate is intentionally not part of the root Cargo workspace.
`libfuzzer-sys` requires nightly-only linker flags and would otherwise break
`cargo build` for ordinary contributors. It is listed under `workspace.exclude`
in the root `Cargo.toml`.
