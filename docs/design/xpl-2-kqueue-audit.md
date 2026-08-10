# XPL-2: kqueue cross-platform cfg-gating audit

## Scope

Audit the kqueue surface in `fast_io` for the cross-platform CI hazards
catalogued in the project Cross-Platform section:

- Unused imports/variables behind `#[cfg(target_os = "...")]` on the other
  platform.
- `#[cfg(unix)]` test modules where individual tests are unix-only but the
  module declaration is not gated.
- `unused_mut` on Windows when `let mut x` is only mutated inside
  `#[cfg(unix)]` blocks.
- Dead `enum` variants on the non-target platform (gate the variant, not the
  `impl`).
- Missing no-op stubs for unsupported platforms (returning `Ok(None)` or
  `Ok(())`).
- Rustdoc link breaks on re-exports (use backtick-only `` `Type` `` instead of
  `` [`Type`] ``).

The kqueue surface lives in two files plus its re-export site:

- `crates/fast_io/src/kqueue/mod.rs` - macOS implementation (real `kqueue(2)` /
  `kevent(2)` wrapper).
- `crates/fast_io/src/kqueue_stub.rs` - non-macOS fallback that mirrors the
  public API and returns `io::ErrorKind::Unsupported` for every constructor.
- `crates/fast_io/src/lib.rs` lines 211-227 + 282 - cfg-switched module mount
  and re-export of the public symbols.

There are currently no consumers of `KqueueLoop` outside this module; the
foundation primitive is defined ahead of the planned `AsyncFileWriter` /
disk-commit / daemon-accept migrations described in
`docs/design/macos-kqueue-fast-io.md`.

## Methodology

1. Read both module files end to end.
2. Grep the codebase for every `kqueue` / `KqueueLoop` / `KEventFilter`
   reference outside the two implementation files. Only `lib.rs` re-exports
   the surface and only a single unrelated doc comment in
   `crates/rsync_io/src/ssh/aux_channel.rs:290` mentions `kqueue(2)` in a
   `poll(2)`/`epoll(7)` enumeration.
3. Re-read `crates/fast_io/Cargo.toml` to confirm which dependencies are
   target-gated. `libc` is `cfg(unix)` only, so the stub cannot reference it
   on Windows.
4. Walk every cfg attribute on the kqueue path and classify the hazard.

## Findings

Hazard counts: **0 CI-fatal / 0 Warning / 8 Clean.**

### Clean items

#### 1. Module mount switch in `lib.rs` is symmetric and exhaustive

`crates/fast_io/src/lib.rs:223-227` mounts exactly one of the two modules and
both branches publish the same module name (`kqueue`):

```rust
#[cfg(target_os = "macos")]
pub mod kqueue;
#[cfg(not(target_os = "macos"))]
#[path = "kqueue_stub.rs"]
pub mod kqueue;
```

`target_os = "macos"` and `not(target_os = "macos")` partition every Rust
target, so there is no platform on which the `kqueue` module is missing. The
`#[path]` redirect lets the stub live in a flat `.rs` file rather than a
parallel `kqueue/` directory.

#### 2. Stub file-level cfg is correct

`crates/fast_io/src/kqueue_stub.rs:13`:

```rust
#![cfg(not(target_os = "macos"))]
#![allow(dead_code)]
```

The inner `cfg` short-circuits the entire file on macOS even though `lib.rs`
already excludes it; this defends against a future direct `mod kqueue_stub;`
mount slipping through. `#![allow(dead_code)]` is the deliberate choice for
the stub: every `KEvent` field and every `KEventFilter` variant is unread on
non-macOS (the kernel never constructs them), and we still want them present
so the stub matches the real public surface byte-for-byte.

#### 3. `RawFd` alias compiles on every target

`crates/fast_io/src/kqueue_stub.rs:17-28`:

```rust
#[cfg(not(unix))]
use std::os::raw::c_int;
...
#[cfg(unix)]
pub type RawFd = std::os::unix::io::RawFd;
#[cfg(not(unix))]
pub type RawFd = c_int;
```

The `c_int` import is gated to `cfg(not(unix))`, matching the only branch that
uses it. There is no `libc` reference in the stub - critical, because
`crates/fast_io/Cargo.toml` declares `libc` under
`[target.'cfg(unix)'.dependencies]` and would not resolve on
`x86_64-pc-windows-msvc` or `aarch64-pc-windows-msvc`.

#### 4. `KEventFilter` variants are not platform-dependent

`crates/fast_io/src/kqueue_stub.rs:31-37`: `enum KEventFilter { Read, Write }`
is declared identically on both sides. Both variants exist on both platforms;
on the stub side they are simply never produced. No "gate the variant, not the
impl" hazard applies because the enum has no platform-only variants. The
module-level `#![allow(dead_code)]` covers the "never constructed" diagnostic.

#### 5. `KEvent` struct fields are `pub`, no platform-dependent dead-field hazard

`crates/fast_io/src/kqueue_stub.rs:39-66`: every field is `pub`, so the
`dead_code` lint applies only to fields the crate never reads internally. The
module-level allow keeps the lint quiet for the few internal read paths
(`is_eof` / `is_error` return constants).

#### 6. Real implementation `Send` impl is correctly scoped

`crates/fast_io/src/kqueue/mod.rs:296-301`: the unsafe `Send` impl lives in
the macOS-only module, so the `unsafe impl Send for KqueueLoop {}` is never
compiled on Windows or musl. The stub `KqueueLoop` does not implement `Send`
because it never holds a real fd; this is acceptable because every constructor
returns `Unsupported` before any caller could move it across threads.

#### 7. Test modules are correctly gated through their parent

`crates/fast_io/src/kqueue_stub.rs:165-179` and
`crates/fast_io/src/kqueue/mod.rs:363-474` both wrap their test modules in
`#[cfg(test)]` and inherit the file-level cfg
(`#![cfg(not(target_os = "macos"))]` or implicit-via-`lib.rs`-mount). The
macOS tests use `libc::pipe`, `libc::pipe(2)` etc., which only compile when
the parent module is included, and the parent module is only included on
macOS where `libc` resolves. No `#[cfg(unix)]` test inside a non-gated module
exists on either side.

#### 8. Re-export uses backtick-only doc references

`crates/fast_io/src/lib.rs:282`: `pub use kqueue::{KEvent, KEventFilter,
KqueueLoop, is_kqueue_available};` has no intra-doc link on a re-exported
type. The module docs at lines 211-222 reference the symbols only by the
unqualified name through prose ("a thin safe wrapper over `kqueue(2)`") so the
known re-export-link breakage (`[`TypeName`]` rendering as text rather than a
link) does not bite.

## Items considered and rejected

- **`unused_mut` on Windows.** No `let mut` appears in either kqueue file
  outside macOS-only `tests` and macOS-only impl bodies. Both are excluded on
  Windows; no hazard.
- **Missing no-op stub.** The stub provides every public method with the same
  signature as the real impl, returning `io::ErrorKind::Unsupported`. The
  `is_kqueue_available()` probe returns `false` so callers can branch at
  runtime without `#[cfg]`. This is the documented project pattern.
- **`KEvent::is_eof` / `is_error` divergence.** Stub returns `false`; real
  impl checks `EV_EOF` / `EV_ERROR` flags. Both have the same signature and
  return type. Behavioural divergence is intentional and documented in the
  rustdoc.
- **`as_raw_fd` returning `-1` on stub.** Matches the documented stub
  behaviour. Callers must check `is_kqueue_available()` before treating the
  return as a real fd, identical to the io_uring stub convention.

## Recommendation

No code changes required. The kqueue cfg-gating is already aligned with the
project Cross-Platform conventions established by SEC-1, the io_uring stub,
the landlock stub, and the apple-fs crate landed in XPL-1 (#4743). This audit
is a documentation-only artifact for XPL-2.

Subsequent XPL-N audits should re-evaluate this surface only when the first
consumer (disk-commit thread, daemon accept loop) lands and introduces new
cross-platform call sites.
