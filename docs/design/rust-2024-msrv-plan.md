# Rust 2024 Edition + MSRV Reconciliation Plan (#2138)

## 1. Current state

- Workspace `Cargo.toml` (lines 170-176) declares the shared `[workspace.package]`:
  - `edition = "2024"`
  - `rust-version = "1.88"`
- `rust-toolchain.toml` pins `channel = "1.88.0"` with `profile = "minimal"` and the
  `rustfmt`, `clippy`, `rust-src`, `llvm-tools-preview` components plus the seven build
  targets used by CI.
- All workspace crates inherit edition + MSRV via `edition.workspace = true` and
  `rust-version.workspace = true`. There is no per-crate override.
- `resolver = "2"` is in effect; resolver 3 is available but not required.

The codebase already compiles on the 2024 edition. The remaining work is to formalise
the MSRV policy and align documentation, CI, and reproducibility scripts so the choice
is durable across releases.

## 2. Rust 2024 edition (stabilised in Rust 1.85)

Edition-level changes that are visible in this codebase:

- Tail-expression drop order. Temporaries in the tail expression of a block now drop
  before locals declared earlier in that block. Audit `Drop`-bearing temporaries
  (file handles, mutex guards, `BufferPool` RAII slots) for behaviour shifts.
- Lifetime capture rules for `impl Trait` (`use<>` syntax). Returned `impl Trait`
  values now capture every in-scope generic by default; rewrite over-capturing sites
  with explicit `use<'a, T>` bounds where ambiguity surfaces.
- Captures in `async fn`. Lifetimes appearing in arguments are captured in the
  returned future; review async transports (`SshConnection`, daemon listener) for
  borrow-checker fallout.
- `let chains` (`if let A && let B = ...`) stable - usable in new code without nightly.
- Reserved syntax: `gen` blocks remain nightly-only; do not adopt yet.
- Other notable items: unsafe `extern` blocks; `unsafe_op_in_unsafe_fn` warn-by-default;
  `static mut` references warn-by-default; `cargo` resolver 3 available opt-in.

## 3. MSRV policy

- Default expectation: bump MSRV when a release needs it, not on a fixed cadence.
  We will not promise the "last N stable releases" guarantee that some libraries
  publish, because oc-rsync ships binaries, not a public API surface that downstream
  crates link against.
- Distro reality check: Debian stable currently ships Rust 1.78, which is below our
  needs; we explicitly do not target distro Rust. Users compile from source via
  `rustup` or use the published binaries.
- Bumps go through a normal PR with `chore:` prefix, update `rust-toolchain.toml`,
  `[workspace.package].rust-version`, README badge, and the release-notes template.
- Each release branch records its MSRV in `Cargo.toml`; release CI verifies the
  workspace builds on the declared MSRV before publishing artifacts.

## 4. Reconciliation

- Hold MSRV at **1.85** as the floor that enables the 2024 edition. Anyone tracking
  the project can stay on 1.85 until they consume a feature that requires more.
- Keep the daily toolchain pin in `rust-toolchain.toml` at **1.88.0** (current value).
  This is the version used for fmt, clippy, nextest, and release artifacts.
- Lower the workspace `rust-version` field from `1.88` to `1.85` so the manifest
  reflects the policy. The pinned 1.88 toolchain remains the build/test version.
- Add an MSRV CI job that runs `cargo +1.85.0 check --workspace --all-features`
  on every PR; it is advisory until the next release, then blocking.

## 5. Migration steps

1. Run `cargo fix --edition --workspace --all-features --allow-dirty` against a
   fresh clone; commit only auditable diffs (no formatting churn).
2. Re-run `cargo fmt --all` and `cargo clippy --workspace --all-targets --all-features
   --no-deps -- -D warnings` to confirm clean output on 1.88.
3. Push the branch and let CI run the full nextest matrix (stable Linux, Windows,
   macOS, musl). Address any 2024-edition fallout (lifetime capture, drop order)
   before merging.
4. Add a `.github/workflows/msrv.yml` job that installs `1.85.0` via
   `dtolnay/rust-toolchain@stable` with `toolchain: 1.85.0` and runs
   `cargo check --workspace --all-features --locked`.
5. Update `README.md` to advertise "Edition 2024, MSRV 1.85, build toolchain 1.88.0",
   and update the release-notes template (`.github/RELEASE_TEMPLATE.md`) with the
   same triplet so each release is self-describing.
6. Document the policy in `AGENTS.md` so future contributors do not accidentally
   reach for a feature that lifts MSRV without an explicit bump PR.
