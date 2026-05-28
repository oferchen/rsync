# Transfer Pipeline Lifecycle State Machine

## Overview

The transfer pipeline progresses through a fixed sequence of protocol phases
from handshake to completion. This document defines a typed state machine
that models the lifecycle, enabling compile-time and runtime enforcement of
valid state transitions.

## States

| State              | Description                                                      |
|--------------------|------------------------------------------------------------------|
| `Handshake`        | Binary or legacy ASCII version exchange (`compat.c:602-604`)     |
| `FilterExchange`   | Filter/exclude list transmission (`exclude.c:recv_filter_list`)  |
| `FileListTransfer` | File list build, send, or receive (`flist.c:send_file_list`)     |
| `DeltaTransfer`    | Per-file signature/delta loop (`receiver.c:recv_files`)          |
| `Finalization`     | Phase-done exchange, stats, goodbye (`main.c:read_final_goodbye`)|
| `Complete`         | Terminal state - transfer finished successfully                  |

## Valid Transitions

```text
Handshake --> FilterExchange --> FileListTransfer --> DeltaTransfer
                                                         |
                                                         v
                                                   Finalization --> Complete
```

Every transition is forward-only. No state may transition to itself or to a
prior state. The single valid path is the linear sequence above.

## Transition Triggers

| From               | To                 | Trigger                                              |
|--------------------|--------------------|------------------------------------------------------|
| `Handshake`        | `FilterExchange`   | `setup_protocol()` completes, multiplex activated     |
| `FilterExchange`   | `FileListTransfer` | `write_filter_list()` / `read_filter_list()` done     |
| `FileListTransfer` | `DeltaTransfer`    | File list fully sent or received                     |
| `DeltaTransfer`    | `Finalization`     | All files processed, `NDX_DONE` exchanged            |
| `Finalization`     | `Complete`         | Stats received, goodbye handshake done                |

## Role Applicability

Both the Generator (sender) and Receiver roles traverse the same state
sequence. The operations within each state differ by role, but the lifecycle
ordering is identical. The state machine accepts a `ServerRole` at
construction to enable role-specific assertions in the future without
changing the transition graph.

## Error Model

Invalid transitions produce `InvalidTransition`, which carries the current
state and the rejected target state. This is a logic error (caller bug), not
a wire error - the caller should never attempt an out-of-order transition.

## Concurrency

The state machine is not `Sync`. A single owner (the transfer orchestration
thread) drives it through the lifecycle. Multi-threaded pipelines (rayon,
disk-commit) operate within a single state and do not advance it.

## Integration Plan

This PR defines the enum and its API only. Existing transfer orchestration
code (`run_server_with_handshake`, `ReceiverContext::run`,
`GeneratorContext::run`) is not refactored to use the state machine yet.
A follow-up PR will thread `TransferPipeline` through the orchestration
entry points and assert transitions at each phase boundary.
