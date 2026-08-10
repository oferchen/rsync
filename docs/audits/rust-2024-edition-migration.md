# Rust 2024 edition migration plan

Tracking issue: oc-rsync task #2125. Branch: `docs/rust-2024-migration-2125`.

## Overview & decision question

This audit captures the workspace's current Rust edition posture, enumerates
the 2024-edition idiom shifts that affect this codebase, and proposes a
migration order that limits blast radius. The workspace already declares
`edition = "2024"` in `Cargo.toml`; the document therefore doubles as a
retrospective ratchet (what is locked in) and a forward-looking checklist
(what still has to be reviewed when raising MSRV or revisiting fuzz crates
that opted out).

Two decision questions:

1. Are the remaining `edition = "2021"` crates (the `cargo fuzz` harnesses)
   safe to leave behind, or should they be promoted to 2024 in lockstep with
   the workspace?
2. What is the canonical playbook the next time we cross an edition boundary
   (2027+), so the migration is applied per-crate before any workspace flip?

## Current state

`grep edition Cargo.toml rust-toolchain.toml` produces:

```
Cargo.toml:4:edition.workspace = true
Cargo.toml:171:edition = "2024"
```

`rust-toolchain.toml` pins `channel = "1.88.0"` with `profile = "minimal"` and
the components `rustfmt`, `clippy`, `rust-src`, `llvm-tools-preview`. The
workspace `rust-version = "1.88"` (Cargo.toml:172) is the MSRV gate. Every
production crate inherits via `edition.workspace = true`; the only
opt-outs are the fuzz harnesses:

- `crates/filters/fuzz/Cargo.toml:6` - `edition = "2021"`
- `crates/protocol/fuzz/Cargo.toml:6` - `edition = "2021"`

Both are excluded from the workspace (`Cargo.toml:164-167`) and are driven by
`cargo-fuzz`, which historically pinned 2021 in its template. They build
against an unstable nightly toolchain and do not gate CI.

## 2024 edition idiom changes that affect this codebase

The Rust 2024 release notes and "Edition Guide" call out a specific set of
breaking idiom shifts. Each entry below maps to concrete risk sites in this
tree, identified by reading code rather than running `cargo fix --edition`.

### 1. `unsafe(...)` attribute syntax

In 2024, attributes that can violate soundness from safe code (`no_mangle`,
`export_name`, `link_section`) must be wrapped: `#[unsafe(no_mangle)]`. The
codebase already conforms:

- `crates/windows-gnu-eh/src/lib.rs:180,190` - both shim symbols use
  `#[unsafe(no_mangle)]`. These are the only `no_mangle` sites in the
  workspace.

No `export_name` or `link_section` attributes exist outside dependencies, so
there are no further risk sites. Future contributors adding FFI exports must
use the `unsafe(...)` wrapper.

### 2. Reserved keyword `gen`

`gen` is a reserved keyword in 2024 (reserved for `gen` blocks). Existing call
sites that bind to a method literally named `gen` (notably `rand::Rng::gen`)
must use the raw identifier `r#gen`. The migration was already applied:

- `crates/checksums/src/strong/md4_tests.rs:444`
- `crates/checksums/src/simd_parity_tests.rs:338,361,718,740,984,985,1008,1009`

That is every `rng.gen()` call site in the tree (9 occurrences). The newer
`rand` 0.9 API renames `gen` to `random`, which avoids the raw identifier;
when we next bump `rand`, these can be simplified.

### 3. Temporary lifetimes in `if let` chains

2024 changes drop order so that the temporary scrutinee in
`if let PAT = expr { ... } else { ... }` is dropped before the `else` branch
runs (matching block-`let` rules). For complex `if let ... && ...` chains the
new rule can change observable drop order. A grep for `if let .* = .* &&` in
the workspace sources returns zero matches. Existing `if let` sites are all
single-binding without trailing `&&` boolean conditions:

- `crates/core/src/client/remote/invocation/builder.rs:289`
- `crates/fast_io/src/temp_file_strategy.rs:194`
- `crates/daemon/src/daemon/sections/module_access/client_args.rs:265`
- `crates/daemon/src/daemon/sections/variable_expansion.rs:111`

None bind a temporary that has an observable `Drop` impl with side effects
relevant to the `else` branch, so this change is a non-event for current
code. The lint `if_let_rescope` (auto-applied by `cargo fix --edition`) is
the canonical detector to keep enabled in CI when we next migrate.

### 4. `gen` blocks (unstable)

`gen { ... }` blocks producing iterators are still unstable (`gen_blocks`
feature) and not used anywhere in the workspace. The reservation only matters
because of item #2 above. No migration action required, but new contributors
should be aware that the bare identifier is no longer available.

### 5. Match ergonomics for references

2024 tightens reference patterns: `let Some(x) = &opt` no longer infers
through nested `&mut`/`&` mismatches. The codebase has many `if let
Some(ref x)` patterns (see daemon and invocation builder lines above) but
these explicitly bind via `ref`, so the new rules are a no-op.

### 6. `unsafe extern` blocks

2024 requires `unsafe extern "C" { ... }` for FFI declarations rather than
plain `extern "C" { ... }`. Risk sites:

- `crates/windows-gnu-eh/src/lib.rs:68,69` - function-pointer type aliases
  inside `unsafe extern "C" fn(...)` declarations. These are fine; the
  required `unsafe` already qualifies the `fn` itself.
- `crates/core/src/signal/unix.rs:129,143,157,170` - `extern "C" fn` items
  acting as signal handlers. These are item-position function definitions
  (not block declarations), unaffected by the `unsafe extern` rule.

There are no bare `extern "C" { ... }` declaration blocks in the workspace
(all FFI is mediated through `libc`, `windows`, or `nix`). No action needed.

### 7. Public-API impl Trait capture (`use<>` syntax)

2024 changes RPIT (return-position `impl Trait`) to capture all in-scope
generics by default; the new opt-out is `impl Trait + use<>`. Audit of public
RPIT signatures in production crates surfaces only narrow cases:

- `crates/filters/src/set.rs:418` -
  `pub fn cvs_exclusion_rules(perishable: bool) -> impl Iterator<Item = FilterRule>`
  - returns `'static` data; no lifetime capture concern.

The remaining `-> impl Trait` returns (proptest strategies under
`crates/filters/tests/`) are test-only and live in the same crate as the type
parameters they capture. The new default is the safer choice for those sites.

### 8. Prelude additions

2024 adds `Future` and `IntoFuture` to the prelude. Glob imports that already
brought in `std::future::Future` are now redundant but not breaking. A grep
for `use std::future::Future` shows the codebase imports explicitly where
needed; no shadowing conflicts exist.

### 9. Cargo: `rust-version` enforcement

2024 cargo treats workspace `rust-version` as a hard floor for resolver-2
dependency selection. The workspace already pins `rust-version = "1.88"`,
which is above the 2024 minimum (1.85), so the resolver behaves as expected.

## Risk-site summary

| Idiom shift | Status | Action |
|---|---|---|
| `unsafe(no_mangle)` | Applied | None |
| `r#gen` raw identifier | Applied (9 sites) | Drop when bumping `rand` to 0.9 |
| `if let` chain rescope | No call sites use chained `if let` | Re-audit if patterns change |
| `gen { ... }` blocks | Not used | None |
| Match ergonomics on references | Explicit `ref` binders, no shift | None |
| `unsafe extern` blocks | No bare `extern` blocks | None |
| RPIT `use<>` capture | Public surface trivially safe | Re-audit on each new public RPIT API |
| Prelude additions | No shadowing | None |
| `rust-version` enforcement | MSRV `1.88` >= 1.85 | Track when raising MSRV |

## Migration order (per-crate, then workspace)

The next edition transition (2027 or later) should follow this sequence so a
single broken crate cannot stall the workspace flip:

1. **Leaf crates first.** Crates with no internal dependents:
   `branding`, `bandwidth`, `logging`, `logging-sink`, `apple-fs`,
   `windows-gnu-eh`, `platform`, `test-support`. Edit each crate's
   `Cargo.toml` to set an explicit `edition = "<new>"` (overriding
   `edition.workspace`), run `cargo fix --edition -p <crate>`, build,
   lint, push.
2. **Mid-tier crates.** `checksums`, `compress`, `filters`, `signature`,
   `match`, `metadata`, `flist`, `rsync_io`, `protocol`, `batch`,
   `embedding`, `fast_io`. Same per-crate procedure.
3. **Subsystem crates.** `engine`, `transfer`, `daemon`.
4. **Top-level crates.** `core`, `cli`, `xtask`, then the binary crate at
   the workspace root.
5. **Workspace flip.** Once every crate carries an explicit `edition =
   "<new>"` line and CI is green on all matrices, change
   `[workspace.package].edition` and remove the per-crate overrides in a
   single follow-up commit. The fuzz crates (`crates/{filters,protocol}/fuzz`)
   migrate independently when `cargo-fuzz` upstream supports the new edition;
   they are excluded from the workspace and do not gate the flip.

For each step the verification gate is the workspace-wide command set:
`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
--all-features --no-deps -- -D warnings`, and the nextest matrix in CI.
Per-crate migrations also need `cargo build --all-features` to surface
`unsafe(...)` and raw-identifier failures that clippy alone would miss.

## MSRV implications

Rust 2024 requires `rustc >= 1.85` (the edition's stabilisation release).
The workspace MSRV is currently `1.88`, comfortably above the floor.

Forward-looking constraints:

- Any third-party dependency that pins MSRV below 1.85 must be replaced
  before declaring 2024 (already true here).
- The `rust-toolchain.toml` channel (`1.88.0`) is the build-time floor for
  contributors; CI matrices include stable, beta, and nightly.
- Future edition bumps must move `[workspace.package].rust-version`
  alongside the edition, not lazily after the fact - cargo's resolver
  consults `rust-version` when picking dependency versions.
- Document the rationale for any MSRV bump in `CHANGELOG.md` so downstream
  packagers (the Homebrew formula, the Arch container image) can plan.

## References

- The Rust Edition Guide ("Rust 2024" chapter) - definitive list of idiom
  changes and `cargo fix --edition` behaviour.
- Upstream cargo docs on `rust-version` - resolver-2 enforcement semantics.
- `Cargo.toml:170-176` - workspace edition and MSRV declaration.
- `rust-toolchain.toml` - pinned toolchain components and target list.
- `crates/windows-gnu-eh/src/lib.rs` - canonical `#[unsafe(no_mangle)]` site.
- `crates/checksums/src/simd_parity_tests.rs` - canonical `r#gen` raw
  identifier sites.
