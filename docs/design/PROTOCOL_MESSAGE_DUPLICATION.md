# Protocol Version Mismatch Message Duplication

## Issue
The `protocol_version_mismatch` scenario shows message differences with upstream rsync 3.0.9 and 3.1.3:
- Upstream outputs messages twice (once from sender, once from receiver)
- oc-rsync outputs messages once

## Root Cause
This is **expected behavior** and not a bug:

### Upstream rsync (3.0.9, 3.1.3)
- Uses a **fork-based architecture**
- Spawns separate sender and receiver processes
- Each process independently outputs error messages
- Result: Same message appears twice

### oc-rsync
- Uses a **single-process architecture**
- No fork into separate sender/receiver processes
- Error is reported once by the unified process
- Result: Message appears once

## Validation
Testing confirms the exit codes are identical (exit code 2 - protocol incompatibility).
The message content is correct; only the duplication differs.

## Resolution
This difference is **acceptable** and does not indicate a compatibility issue. The single-process
architecture is a design choice that provides benefits in code maintainability and debugging while
maintaining full protocol compatibility.
