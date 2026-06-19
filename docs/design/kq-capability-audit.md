# KQ-1: kqueue capability audit vs transfer pipeline needs

Parent: KQ (#4189). Sibling docs:

- `docs/design/macos-kqueue-fast-io.md` - design rationale for the
  primitive.
- `docs/design/kqueue-async-file-writer.md` - KQ-2 disk-writer design.
- `docs/design/kqueue-pipeline-audit.md` - surface-by-surface pipeline
  inventory (one row per transfer site).
- `docs/design/xpl-2-kqueue-audit.md` - cross-platform cfg-gating audit.

This doc is the capability inventory pass: which `EVFILT_*` filter types
are wired in `crates/fast_io/src/kqueue/`, which call sites are real vs
stub, and which KQ-S.* sub-tasks fill which capability gap. KQ-2 writer
design and the pipeline surface table are out of scope here.

## Module inventory

`crates/fast_io/src/kqueue/` is split into two files:

- `mod.rs` (479 lines) - `KqueueLoop` event-loop primitive wrapping
  `kqueue(2)` / `kevent(2)`, plus the `KEventFilter` enum, `KEvent`
  result struct, and `submit_read` / `submit_write` / `remove` / `wait`
  surface.
- `timer.rs` (290 lines) - `TimerSleeper` single-shot `EVFILT_TIMER`
  sleep primitive owning a dedicated kqueue fd.

Re-exports flow through `crates/fast_io/src/lib.rs:265-361`:

- Real module: `pub mod kqueue;` gated on `cfg(target_os = "macos")`.
- Stub module: `#[path = "kqueue_stub.rs"] pub mod kqueue;` for every
  non-macOS unix and Windows target.
- Public surface: `KEvent`, `KEventFilter`, `KqueueLoop`, `TimerSleeper`,
  `is_kqueue_available`.

## Wired filter types

| Filter        | Wired in module                               | Real impl                                                                                                                                                                                                                                                                                                                                | Stub                                            |
| ------------- | --------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------- |
| `EVFILT_READ`  | `mod.rs:65`, exposed via `submit_read` `mod.rs:157` | Yes - `EV_ADD \| EV_CLEAR` edge-triggered registration, `EV_EOF` decoded in `KEvent::is_eof` `mod.rs:97-100`.                                                                                                                                                                                                                          | `kqueue_stub.rs:99-104` returns `Unsupported`.  |
| `EVFILT_WRITE` | `mod.rs:66`, exposed via `submit_write` `mod.rs:170` | Yes - same `EV_ADD \| EV_CLEAR` shape as read.                                                                                                                                                                                                                                                                                          | `kqueue_stub.rs:111-116` returns `Unsupported`. |
| `EVFILT_TIMER` | `timer.rs:111`                                | Yes - `EV_ADD \| EV_ONESHOT \| NOTE_NSECONDS` on a dedicated kqueue fd inside `TimerSleeper::sleep` `timer.rs:99-152`. Sub-millisecond resolution verified by `timer.rs::sleep_sub_millisecond_returns_promptly`. Consumed by the bandwidth limiter via `crates/bandwidth/src/limiter/backend.rs:37,103,125-145` (default on macOS). | `kqueue_stub.rs:179-185` returns `Unsupported`. |
| `EVFILT_PROC`  | not wired                                     | Absent. The `KEventFilter` enum only carries `Read` / `Write` (`mod.rs:55-60`).                                                                                                                                                                                                                                                          | n/a (filter does not exist).                    |
| `EVFILT_SIGNAL` | not wired                                    | Absent.                                                                                                                                                                                                                                                                                                                                  | n/a.                                            |
| `EVFILT_VNODE` | not wired                                     | Absent.                                                                                                                                                                                                                                                                                                                                  | n/a.                                            |

## Stub vs real ratio

Three filters of the six the transfer pipeline needs are wired
(`EVFILT_READ`, `EVFILT_WRITE`, `EVFILT_TIMER`); three are absent
(`EVFILT_PROC`, `EVFILT_VNODE`, `EVFILT_SIGNAL`). The stub module at
`kqueue_stub.rs` mirrors the public type surface for cross-platform
compile but returns `io::ErrorKind::Unsupported` from every constructor
(`kqueue_stub.rs:81-86, 179-184`) - callers probe at runtime via
`is_kqueue_available()` (`kqueue_stub.rs:161-163` returns `false`,
`mod.rs:363-365` returns `true`).

## Production callers today

The only production caller of either `KqueueLoop` or `TimerSleeper` on
master is the bandwidth limiter
(`crates/bandwidth/src/limiter/backend.rs`). It uses `TimerSleeper`
through a `OnceLock<TimerSleeper>` keyed on macOS-default behaviour,
falling back to `std::thread::sleep` if construction fails. No transfer
pipeline path (receiver writer, sender reader, daemon accept, SSH child
monitor) currently uses `KqueueLoop`.

## Per-need gap analysis

| Pipeline need                                | Filter needed   | Wired today | Sub-task     |
| -------------------------------------------- | --------------- | ----------- | ------------ |
| Receiver writer parks on writeback pressure  | `EVFILT_WRITE`  | Yes         | KQ-2 / KQ-3  |
| Sender reader parks on read readiness        | `EVFILT_READ`   | Yes         | KQ-4 / KQ-5  |
| Daemon accept loop waits for connection      | `EVFILT_READ`   | Yes         | KQ-S.1       |
| SSH child reap delivered as event            | `EVFILT_PROC`   | No          | KQ-S.2       |
| Daemon multiplex socket I/O across N conns   | `EVFILT_READ` + `EVFILT_WRITE` | Yes  | KQ-S.3       |
| Bandwidth limiter sub-ms timer               | `EVFILT_TIMER`  | Yes         | KQ-S.4       |
| Daemon module auto-reload on config change   | `EVFILT_VNODE`  | No          | KQ-S.5       |

The `KqueueLoop` primitive can land KQ-2, KQ-3, KQ-4, KQ-5, KQ-S.1, and
KQ-S.3 without any new filter support; they reuse the already-wired
`Read` / `Write` filters. KQ-S.2 and KQ-S.5 require extending the
`KEventFilter` enum (and the `from_raw` / `as_raw` mappings at
`mod.rs:63-77`) plus the `submit_*` surface for the new shape -
`EVFILT_PROC` keys on a pid rather than an fd and carries
`NOTE_EXIT`-style `fflags`. KQ-S.4 already has a parallel primitive in
`TimerSleeper`; integration with `KqueueLoop` is a separate composition
question (one fd vs one shared loop), tracked in the KQ-S.4 design pass.

## Sequencing recommendation for KQ-2

KQ-2 (receiver writer design) should be sequenced first because:

1. The required filter (`EVFILT_WRITE`) is already wired and exercised
   by `mod.rs::read_event_fires_on_pipe_write` and the EOF round-trip
   test - no enum extension is needed.
2. The disk-commit thread is the single highest-impact macOS hot path
   currently blocking on `write_all` (see
   `docs/design/kqueue-pipeline-audit.md` row 1).
3. Composition rule "one `KqueueLoop` per long-lived thread" already
   matches the disk-commit thread's lifecycle; the writer redesign does
   not depend on multi-fd multiplexing landing first.
4. KQ-3 implementation behind a feature flag can ride the same
   `KqueueLoop` instance with no new primitive work.

KQ-S.1 (daemon accept) and KQ-S.3 (socket multiplexing) can land in
parallel once KQ-2's `KqueueLoop` ownership patterns are documented in
the writer design doc. KQ-S.2 (`EVFILT_PROC`) and KQ-S.5
(`EVFILT_VNODE`) should land after KQ-2 because they extend
`KEventFilter` and would otherwise force a churn rebase on the writer
work. KQ-S.4 (`EVFILT_TIMER` consolidation) has the lowest urgency
because the standalone `TimerSleeper` already ships the perf win for
the limiter; pooling into a shared loop is a refactor, not a gap.

## macOS-only cfg-gate discipline

Every kqueue-aware module gates real bindings on
`cfg(target_os = "macos")` (`mod.rs` is the macOS path,
`kqueue_stub.rs:13` carries `#![cfg(not(target_os = "macos"))]`). New
filter wiring (KQ-S.2, KQ-S.5) must extend both modules in lockstep:
the real `KEventFilter` variant gains an `EVFILT_*` mapping; the stub
gains the same enum variant with a no-op `Unsupported` path. The
`is_kqueue_available()` probe is the only runtime gate; callers must
not embed `cfg(target_os = "macos")` checks in transfer pipeline code -
they probe and fall back, as `crates/bandwidth/src/limiter/backend.rs`
already does. XPL-2 (`docs/design/xpl-2-kqueue-audit.md`) covers the
unused-import / `unused_mut` discipline for new code under the gate.
