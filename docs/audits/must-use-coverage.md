# `#[must_use]` coverage audit on public APIs

Tracking issue: #2123. Companion to `must-use-coverage-audit.md`, which
covers `Result`/`Option` returners. This audit focuses on **non-fallible**
public functions where dropping the return value is silently meaningless:
builders that consume `self`, predicate/validator probes returning `bool`,
and value-returning constructors. `Result` and `Option` already lint via
`#[must_use]` on the type itself; bare-value returns do not.

## 1. Current state

`std::result::Result` and `std::option::Option` carry `#[must_use]` at the
type level, so any `pub fn -> Result<_>` already triggers
`unused_must_use` on a dropped return. Functions returning `Self`,
`&Self`, `bool`, `usize`, `&str`, etc. do **not** lint unless we annotate
the function. Discovery query (run from worktree root):

```sh
rg -n 'pub fn .* -> (Self|&Self|&mut Self|bool|usize|&str|Cow<.*>)' crates/<crate>/src
```

A workspace sweep across the highest-leverage crates (`cli`, `core`,
`daemon`, `engine`, `protocol`, `transfer`, `filters`) returned 442
`-> Self` declarations and 126 `-> bool` declarations. None of the bare
`-> bool` predicates and very few `-> Self` builders carry `#[must_use]`
today.

## 2. Cases that benefit from explicit `#[must_use]`

- **Consuming builders** (`fn with_x(mut self, ...) -> Self`). Dropping
  the return throws away the configured field silently because the caller
  forgot to rebind. Example sites:
  `crates/core/src/message/message_impl/mutators.rs::with_source`,
  `crates/core/src/client/config/builder/*::with_connect_program`,
  `crates/core/src/version/report/renderer.rs::with_metadata`.
- **Predicates / probes** (`fn is_x(&self) -> bool`,
  `fn has_pending(&self) -> bool`). Calling them for side-effect is
  always wrong; dropping the result indicates a logic bug.
- **Pure value constructors** (`fn new(...) -> Self`, `fn from_x(...) -> Self`)
  on types whose construction has no side effect.
- **Re-exports of `Result`-returning helpers** that re-wrap an inner
  error type. The outer wrapper inherits `must_use` only if the wrapper
  type also carries it; bare type aliases do not propagate.

## 3. Categorisation

| Category | Treatment | Rationale |
|----------|-----------|-----------|
| (a) `&mut self` mutators (`set_x`, `push_x`) returning `()` | leave alone | side-effect is the point |
| (b) Consuming builders returning `Self`/`&mut Self` | add `#[must_use]` | dropping discards configuration |
| (c) Predicates returning `bool` (`is_x`, `has_x`, `should_x`) | add `#[must_use]` | pure query, must observe |
| (d) Value constructors (`new`, `from_*`, `with_capacity`) | add `#[must_use]` | pure, allocation visible only via return |
| (e) Re-export wrappers (`pub use foo::bar`) | annotate at definition | attribute applies at the source |

Existing convention: clippy's `must_use_candidate` lint matches (b)-(d)
exactly; we already deny `clippy::pedantic` selectively in some crates
but not workspace-wide.

## 4. Per-crate triage

Counts of `pub fn -> Self` and `pub fn -> bool` (excluding tests):

| Crate | `-> Self` | `-> bool` | Priority |
|-------|-----------|-----------|----------|
| `cli` | 2 | 0 | low (mostly internal frontend) |
| `core` | 61 | 12 | **high** - CLI + daemon orchestration facade |
| `daemon` | 20 | 22 | **high** - lifecycle predicates + builders |
| `protocol` | 88 | 32 | **high** - capability flag predicates, frame builders |
| `engine` | 188 | 32 | medium - many internal builders |
| `transfer` | 42 | 20 | medium |
| `filters` | 12 | 8 | medium - chain predicates |

Highest-value targets: `core`, `daemon`, `protocol`. These are the public
crates an embedder of `oc-rsync` would import. `engine`/`transfer` carry
volume but most surface area is internal-by-convention even when `pub`.

## 5. Plan

1. **Phase 1 - bulk-add to `core`, `daemon`, `protocol`.** For each `pub
   fn` matching `(b)`, `(c)`, `(d)` annotate `#[must_use]`. Use one
   targeted PR per crate to keep review bounded.
2. **Phase 2 - sweep `engine`, `transfer`, `filters`, `signature`,
   `checksums`.** Same rules; one PR per crate.
3. **Phase 3 - long tail** (`bandwidth`, `branding`, `compress`, `flist`,
   `match`, `metadata`, `platform`, `rsync_io`, `fast_io`, `apple-fs`,
   `logging`, `logging-sink`, `embedding`, `batch`).
4. **Phase 4 - clippy gate.** Once coverage is broad, enable
   `clippy::must_use_candidate` at `warn` workspace-wide via
   `[workspace.lints.clippy]` in `Cargo.toml`. After a clean cycle,
   promote to `deny`. Skip `clippy::must_use_unit` because trivial
   wrappers around `()` are not actionable.
5. **Verification.** Each phase is verified by `cargo clippy --workspace
   --all-targets --all-features --no-deps -- -D warnings` (already a CI
   gate). Newly-flagged internal call sites either bind the result or
   propagate it; never `let _ = ...` to silence the lint.

The accompanying scanner for the `Result`/`Option` audit
(`tools/audit/must_use_audit.py`) should be extended to recognise the
three new categories above so progress is tracked against the same
coverage table.
