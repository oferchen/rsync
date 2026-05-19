# Rust 2024 edition + MSRV reconciliation

Tracking issue: oc-rsync task #2138. Branch: `docs/rust-2024-msrv-2138`.

## Scope

Audit task #2138 reconciles the workspace's declared Rust edition with its
Minimum Supported Rust Version (MSRV) pin. The companion audit
(`rust-2024-edition-migration.md`, task #2125) catalogues each 2024 idiom
shift; this document focuses on the version-pin invariant: which files
carry the MSRV, which floor the 2024 edition imposes, and how to keep the
two in lockstep when either is bumped.

## Current state

Concrete declarations (cited verbatim):

- `Cargo.toml:171` - `edition = "2024"` (under `[workspace.package]`).
- `Cargo.toml:172` - `rust-version = "1.88"` (under `[workspace.package]`).
- `Cargo.toml:5` - `rust-version.workspace = true` (binary crate inherits).
- `Cargo.toml:349` - `rust_version = "0.6.1"` (workspace metadata; this is
  the oc-rsync release version, not the rustc MSRV - the underscore is
  load-bearing).
- `rust-toolchain.toml:2` - `channel = "1.88.0"`.
- `crates/filters/fuzz/Cargo.toml:6` - `edition = "2021"` (cargo-fuzz harness,
  excluded from workspace at `Cargo.toml:165`).
- `crates/protocol/fuzz/Cargo.toml:6` - `edition = "2021"` (likewise excluded
  at `Cargo.toml:166`).

All in-workspace crates inherit `edition.workspace = true` and
`rust-version.workspace = true`; there are no per-crate overrides. The fuzz
crates are the sole holdouts and are excluded from the workspace resolver.

## Rust 2024 edition: idiom changes that affect version pinning

The 2024 edition shipped in Rust 1.85 (February 2025). Each shift below has
a distinct interaction with MSRV.

### 1. Lifetime capture rules for return-position `impl Trait` (RPIT)

In 2024, `-> impl Trait` captures **all** in-scope generic parameters and
lifetimes by default; the new opt-out is `impl Trait + use<...>`. The
`use<>` syntax was stabilised in 1.82, so it predates the edition. Code
that needs the narrower 2021 capture must add `+ use<>` explicitly. Risk
assessment: the workspace's only public RPIT signature returns `'static`
data (`crates/filters/src/set.rs:418`, `cvs_exclusion_rules`), so the
default change is safe.

### 2. `Box<dyn Trait>` defaulting

The 2024 edition does **not** change the elided lifetime in `Box<dyn
Trait>`; the long-discussed "dyn lifetime ergonomics" RFC was deferred. The
edition does sharpen `dyn` syntax requirements (e.g. `dyn Trait` in 2021
without `+ 'static` already produced a warning); the 2024 edition keeps the
status quo. No MSRV implication beyond the edition floor.

### 3. `unsafe extern` blocks

`extern "C" { ... }` declaration blocks must be `unsafe extern "C" { ... }`.
This stabilised alongside the edition in 1.82 with the gate flipped at the
edition boundary. The workspace contains no bare `extern "C"` declaration
blocks (all FFI is mediated through `libc`, `windows`, `nix`, or `exacl`),
so the rule is a non-event.

### 4. Match ergonomics for `ref` bindings

2024 tightens the implicit reference-binding rules: `let Some(x) = &opt`
and similar patterns no longer infer through nested `&mut`/`&` mismatches.
Existing code that relies on explicit `ref`/`ref mut` binders (the daemon
and invocation builder modules use this pattern) is unaffected. The
canonical detector is `cargo fix --edition`.

### 5. `gen` keyword reservation

`gen` is reserved in 2024 (for future `gen { ... }` iterator blocks).
Existing call sites that bind a method literally named `gen` (notably
`rand::Rng::gen`) must use `r#gen`. The workspace already migrated the nine
call sites in `crates/checksums/src/{strong/md4_tests,simd_parity_tests}.rs`.

### 6. `if let` chain rescope

2024 changes drop order so the temporary scrutinee in `if let PAT = expr {
... } else { ... }` is dropped before the `else` branch runs. A grep for
`if let .* = .* &&` returns zero matches in workspace sources, so the
change is inert here. The lint `if_let_rescope` (auto-applied by `cargo fix
--edition`) is the canonical detector.

## Migration cost

`cargo fix --edition` handles the mechanical rewrites for items 3 (unsafe
extern), 5 (raw `r#gen`), and 6 (`if_let_rescope`). The lifetime capture
shift (item 1) is the only change that demands manual review: the tool
cannot infer intent for RPIT signatures whose author wanted the 2021
"capture nothing extra" behaviour. The existing per-crate migration order
in `rust-2024-edition-migration.md` (leaves -> mid-tier -> subsystem ->
top-level -> workspace flip) limits blast radius for any future edition
move.

For the current 2024 pin, no further work is required: every crate already
builds clean on `rustc 1.88.0`, and the fuzz crates can stay on 2021
because `cargo-fuzz` invokes them under their own resolver.

## MSRV pin: 1.88 vs 2024 floor

The 2024 edition's floor is `rustc 1.85`. The workspace pins `1.88` in two
places that must agree:

| File | Line | Value | Role |
|---|---|---|---|
| `Cargo.toml` | 172 | `rust-version = "1.88"` | resolver gate (build-time) |
| `rust-toolchain.toml` | 2 | `channel = "1.88.0"` | toolchain pin (CI + dev) |

`1.88 >= 1.85`, so the pin is compatible. Cargo's resolver (resolver = "2",
declared at `Cargo.toml:168`) consults `rust-version` when picking
dependency versions, so dependencies that pin a higher MSRV will surface as
a build-time error rather than a silent compatibility break.

When raising MSRV, both lines must move together. Updating only
`rust-toolchain.toml` masks dependency MSRV violations from local builds
(the toolchain compiles fine) while CI and downstream packagers using
`rust-version` will diverge. The opposite (raising `rust-version` without
the toolchain) breaks the dev-shell because the pinned compiler is too
old. CI's stable/beta matrix catches the second case faster than the first.

## Risks

1. **Dependency MSRV drift.** Crates that ship breaking MSRV bumps in patch
   releases (historic offenders: `tokio`, `serde`, `clap`) can force a
   workspace MSRV bump unexpectedly. Resolver-2 plus `rust-version = "1.88"`
   prevents the build from selecting an incompatible point release, but
   newly added direct dependencies must be vetted for their own MSRV.
   Lockfile audits (`cargo update --dry-run`) before each release reduce
   drift surprises.
2. **Lockfile drift.** `Cargo.lock` is committed (binary crate) and pins
   transitive versions; any unsynchronised `cargo update` between branches
   can pull in a transitive crate whose MSRV exceeds 1.88. CI runs
   `cargo build` on the locked versions, so a drift will fail the matrix
   instead of leaking to release.
3. **Downstream consumers on older toolchains.** The Homebrew formula and
   the Arch container image (`localhost/oc-rsync-bench:latest`) build with
   the active stable toolchain at the time of release. Distros packaging
   against older Rust (Debian stable, Alpine LTS) may lag 1.85+. The
   workspace pin is honest: `rust-version = "1.88"` advertises the floor
   and the resolver enforces it. Documenting MSRV in `CHANGELOG.md` for
   every bump (per the existing process) keeps packagers informed.
4. **Fuzz harness divergence.** The two `crates/*/fuzz` packages still pin
   `edition = "2021"`. They build under nightly (cargo-fuzz requires
   nightly's `-Zinstrument-coverage`/sanitiser flags), so the production
   2024 edition does not affect them. Risk: a future cargo-fuzz template
   change may force 2024; this is a no-op for the workspace because the
   fuzz crates are excluded.
5. **`workspace.metadata.oc_rsync.rust_version`.** The release version
   string at `Cargo.toml:349` shares spelling with the cargo MSRV key. A
   future contributor renaming variables across the file must not collapse
   the two. The release-process documentation already lists both as
   distinct bump targets.

## Recommendation

The workspace is already on the 2024 edition with a compatible MSRV. No
migration work is required; the recommendation set covers ratchet hygiene
for the next edition (2027+) and the next MSRV bump.

### When to migrate (next edition)

- Wait for the post-`v0.6.x` release window. A version cut creates a stable
  baseline branch, so a parallel edition migration on `master` does not
  collide with hotfixes.
- Hold the migration until the new edition has been on stable for at least
  two releases (six months) so dependency ecosystems catch up. This was the
  cadence used for the 2024 pin.
- Promote the fuzz crates only after `cargo-fuzz` upstream advertises
  support; they are excluded from the workspace and do not gate.

### Which crates first

Per-crate ordering for the next edition transition mirrors the existing
plan in `rust-2024-edition-migration.md`:

1. **Leaf crates first** (no internal dependents):
   `branding`, `bandwidth`, `logging`, `logging-sink`, `apple-fs`,
   `windows-gnu-eh`, `platform`, `test-support`. Per-crate
   `edition = "<new>"` override, `cargo fix --edition -p <crate>`, push.
2. **Mid-tier crates**: `checksums`, `compress`, `filters`, `signature`,
   `match`, `metadata`, `flist`, `rsync_io`, `protocol`, `batch`,
   `embedding`, `fast_io`.
3. **Subsystem crates**: `engine`, `transfer`, `daemon`.
4. **Top-level crates**: `core`, `cli`, `xtask`, then the binary at the
   workspace root.
5. **Workspace flip.** Once every crate carries an explicit
   `edition = "<new>"` line and CI is green on every matrix, change
   `[workspace.package].edition` and remove the per-crate overrides in a
   single commit. Bump `rust-version` to the new edition's floor in the
   same commit and update `rust-toolchain.toml` channel to match. Document
   the bump in `CHANGELOG.md` so downstream packagers can plan.

### MSRV bump checklist

When the next MSRV change is necessary (driven either by a dependency or by
the next edition):

1. Identify the rustc release that introduced the required feature.
2. Update `rust-toolchain.toml` channel to that release (or newer).
3. Update `Cargo.toml:172` `rust-version` to match.
4. Run `cargo build --workspace --all-features` and the workspace
   clippy/fmt gates (`cargo fmt --all -- --check` and `cargo clippy
   --workspace --all-targets --all-features --no-deps -- -D warnings`).
5. Push and let CI exercise stable, beta, nightly, Linux musl, macOS,
   Windows.
6. Add a `CHANGELOG.md` entry under `### Other Changes`.
7. Notify the Homebrew formula and Arch container maintainers (release
   workflow auto-PRs Homebrew; the container needs a manual `podman build`
   to pull a newer Rust base image).

## References

- `Cargo.toml:170-176` - workspace edition and MSRV declaration.
- `rust-toolchain.toml` - pinned toolchain components and target list.
- `docs/audits/rust-2024-edition-migration.md` (task #2125) - exhaustive
  per-idiom risk-site catalogue and per-crate migration playbook.
- The Rust Edition Guide ("Rust 2024" chapter) - definitive list of idiom
  changes and `cargo fix --edition` behaviour.
- Cargo reference, "rust-version" field - resolver-2 enforcement semantics.
