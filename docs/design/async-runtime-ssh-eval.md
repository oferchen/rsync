# Async Runtime Evaluation for SSH Transport

Tracking issue: #1411

## 1. Scope

This document evaluates the *runtime choice* (tokio vs smol vs async-std)
for any future async SSH transport work. It is distinct from #1593, which
evaluates async I/O on the SSH path itself; #1593 focuses on whether to
overlap pipe reads and writes, while #1411 answers which executor runs
those futures. The two issues are complementary: a runtime decision must
land before async-ssh implementation work begins.

## 2. Prior art in this repo

- **#1779 (done)** - audit of tokio dependency scope. Tokio is already a
  build-time dependency (daemon listener, async accept loop, sync workers
  in `daemon-async-accept-sync-workers.md`, async listener implementation
  in `daemon-tokio-async-listener-impl.md`). Removing it is not on the
  table; the cost is paid.
- **#1780 (done)** - "no second async runtime" rule. The workspace stays
  on a single executor. Mixing tokio with smol or async-std would force
  bridging adapters (`async-compat`), duplicate timer wheels, and split
  reactor responsibilities. Rejected.

## 3. Runtime options

| Runtime    | Pros                                                  | Cons                                                                         |
|------------|-------------------------------------------------------|------------------------------------------------------------------------------|
| tokio      | Already in workspace; mature; russh native target      | Larger feature surface than strictly needed for SSH stdio                    |
| smol       | Small footprint; minimal dependencies                  | Would be a *second* runtime; needs `async-compat` to bridge tokio code paths |
| async-std  | std-shaped API                                         | Maintenance has slowed upstream; second runtime; same bridging problem       |

Given #1779 and #1780, only tokio is viable without violating the
single-runtime invariant. smol and async-std are rejected on those
grounds before performance enters the discussion.

## 4. Async SSH crate options

- **russh** - tokio-native, actively maintained, pure-Rust SSH client and
  server. Added as an optional dependency in #1782; staging code lives
  under `crates/rsync_io/src/ssh/embedded/`.
- **thrussh** - the predecessor to russh. Deprecated upstream; the
  maintainer's repository points new users at russh. Not a candidate.
- **libssh2-sys / ssh2** - C-binding crates, blocking API. Would require
  `spawn_blocking` wrapping and contribute no async value. Rejected.

russh is the only async-native option that aligns with the chosen
runtime.

## 5. Recommendation

- Use the already-imported tokio runtime for any async SSH work. Do not
  pull in smol or async-std; #1780 forbids a second runtime.
- Adopt russh (#1782) when async SSH is implemented. thrussh is
  deprecated; ssh2 is blocking.
- Defer the actual migration behind a `--features embedded-ssh` cargo
  feature so the synchronous `std::process` SSH transport remains default
  until benched per #1593.

This decision is non-binding on schedule: it commits the project to
tokio + russh whenever async SSH ships, and forecloses a smol/async-std
detour. Implementation tracking continues under #1593.
