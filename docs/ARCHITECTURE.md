# oc-rsync Architecture Overview

This document outlines the crate boundaries, role separation, and design patterns enforced across the oc-rsync workspace.

---

## Crates and Responsibilities

| Crate         | Responsibility                                            |
|---------------|-----------------------------------------------------------|
| `cli`         | CLI parsing, subcommands, exit code routing               |
| `core`        | Main orchestrator for roles, config, diagnostics          |
| `protocol`    | Protocol 32 tag handling, sender/receiver transitions     |
| `transfer`    | Delta transfer engine, rolling checksum, block matching   |
| `filters`     | Filter grammar parsing, application engine                |
| `daemon`      | `rsyncd.conf` parsing, module selection, session dispatch |
| `embedding`   | Self-exec orchestration (used by `--server`)              |
| `compress`    | Compression formats (zlib), protocol negotiation          |
| `metadata`    | Permissions, xattrs, ACLs, timestamps                     |
| `logging`     | Output formatting, debug/info channels                    |
| `checksums`   | Rolling and strong checksum implementations               |

---

## Role Execution Pipeline

```
main.rs → cli → core → role match
         ↘          ↘
     --server     --daemon
```

---

## Clean Code Rules

- No crate must exceed 1000 LOC/module  
- One module = one concern  
- No cross-crate import cycles  
- No CLI logic outside `cli/`  
- No branding hardcoded anywhere  
- No dead code, unwrap, panic, or TODOs in production

---

## Design Patterns

| Pattern     | Applied In     | Purpose                                      |
|-------------|----------------|----------------------------------------------|
| Command     | CLI            | Subcommand dispatch                          |
| Strategy    | filters, checksums, logging | Runtime-behavior polymorphism        |
| Factory     | core/daemon    | Role routing                                 |
| Builder     | protocol       | Frame and tag construction                   |
| Visitor     | filters        | Rule application with recursion              |
| Adapter     | daemon config  | Map rsyncd.conf into runtime configuration   |
| State FSM   | protocol       | Prevent illegal transitions in protocol FSM  |

---
