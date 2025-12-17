# Protocol Timing Resolution Plan

**Status**: DRAFT
**Created**: 2025-12-17
**Objective**: Fix protocol timing issues blocking Protocol 30+ capability negotiation

---

## Problem Statement

### Symptoms
- Upstream rsync clients (3.0.9, 3.1.3, 3.4.1) report "unexpected tag 25" when connecting to oc-rsync daemon
- Server reports "invalid UTF-8 in negotiation string" (reading multiplex frames as plain data)
- Failures occur BEFORE capability negotiation, indicating pre-existing protocol flow issue

### Root Cause
The server and client are out of sync on when to activate multiplex mode:
- **Client**: Activates INPUT multiplex immediately after compat flags
- **Server**: Sends compat flags as plain data, then activates OUTPUT multiplex
- **Result**: Client is reading multiplex frames while server sends plain data (or vice versa)

### Evidence
```
# From interop test logs:
oc-rsync info: protocol 30, role: Receiver          ← Native processing confirmed
unexpected tag 25 [sender]                           ← Client receives wrong frame type
error=invalid UTF-8 in negotiation string            ← Server reads multiplex as plain
```

---

## Investigation Phase

### Phase 0: Validate Protocol 32 Requirements (CRITICAL)

**Objective**: Ensure all Protocol 32 features are implemented before focusing on timing

**Background**: The current implementation may be missing Protocol 32-specific features beyond capability negotiation.

**Tasks**:

1. **Audit Protocol 32 requirements from upstream**
   - [ ] Read `rsync.h` for PROTOCOL_VERSION 32 constants and flags
   - [ ] Read `compat.c` for Protocol 32-specific compatibility checks
   - [ ] Read `NEWS` or `CHANGELOG` for Protocol 32 feature list
   - [ ] Identify all features introduced in Protocol 32:
     - Capability negotiation (checksum/compression) ← **Implemented**
     - Varint file list encoding ← **Check if complete**
     - Extended file list flags ← **Check if complete**
     - New message types ← **Check if complete**
     - Other protocol changes ← **Identify**
   - **Expected result**: Complete Protocol 32 feature checklist

2. **Audit current oc-rsync Protocol 32 implementation**
   - [ ] Check `crates/protocol/src/version.rs` for supported protocols
   - [ ] Verify SUPPORTED_PROTOCOLS includes 32
   - [ ] Check `crates/protocol/src/compatibility.rs` for Protocol 32 flags
   - [ ] Verify all required compat flags are present:
     - `INC_RECURSE` (incremental recursion)
     - `SAFE_FILE_LIST` (safe file list I/O)
     - `CHECKSUM_SEED_FIX` (checksum seed ordering)
     - `VARINT_FLIST_FLAGS` (varint-encoded file list flags)
     - Others as needed for Protocol 32
   - [ ] Check `crates/protocol/src/flist/` for file list encoding
   - [ ] Verify varint encoding is used for Protocol 32
   - **Expected result**: Gap analysis of Protocol 32 features

3. **Check for missing Protocol 32 components**
   - [ ] Message codes (MSG_* constants) - are all Protocol 32 messages defined?
   - [ ] File list encoding - does it match Protocol 32 wire format?
   - [ ] Checksum handling - are all algorithms available?
   - [ ] Compression handling - are all algorithms available?
   - [ ] Extended attributes encoding - if Protocol 32 changed this
   - **Expected result**: List of missing components

4. **Validate against Protocol 30/31 compatibility**
   - [ ] Ensure Protocol 32 code can negotiate down to Protocol 30
   - [ ] Ensure Protocol 32 code can negotiate down to Protocol 31
   - [ ] Verify backward compatibility doesn't break
   - **Expected result**: Multi-protocol compatibility confirmed

**Decision Point**:
- If Protocol 32 components are missing → Implement them BEFORE fixing timing
- If Protocol 32 components are complete → Proceed to Phase 1 (timing investigation)

**Deliverable**: Document `PROTOCOL_32_REQUIREMENTS.md` with feature matrix

**Estimated Effort**: 2-3 hours

**Priority**: CRITICAL (must complete before other phases)

---

### Phase 1: Study Upstream Protocol Flow (HIGH PRIORITY)

**Objective**: Understand exact sequence of events in upstream rsync daemon mode

**Tasks**:

1. **Map upstream server initialization** (`main.c:start_server()`)
   - [ ] Read `main.c:1187-1262` (start_server function)
   - [ ] Identify when `setup_protocol()` is called
   - [ ] Identify when multiplex is activated (OUTPUT vs INPUT)
   - [ ] Note any differences between daemon mode and SSH mode
   - **Expected result**: Timeline diagram of protocol events

2. **Map upstream receiver flow** (`main.c:do_server_recv()`)
   - [ ] Read `main.c:1144-1180` (do_server_recv function)
   - [ ] Identify INPUT multiplex activation point (supposedly line 1167)
   - [ ] Identify when filter list is read
   - [ ] Identify when file list is read
   - **Expected result**: Receiver-specific event sequence

3. **Map upstream compat exchange** (`compat.c:setup_protocol()`)
   - [ ] Read `compat.c:572-644` (setup_protocol function)
   - [ ] Identify when compat flags are sent (line ~736-738)
   - [ ] Confirm whether this happens BEFORE or AFTER multiplex
   - [ ] Check if there's a flush between compat and multiplex
   - **Expected result**: Compat exchange timing relative to multiplex

4. **Map upstream capability negotiation** (`compat.c:negotiate_the_strings()`)
   - [ ] Read `compat.c:534-585` (negotiate_the_strings function)
   - [ ] Identify exact call site in the protocol flow
   - [ ] Confirm whether negotiation happens before or after multiplex
   - [ ] Check if negotiation uses plain or multiplexed I/O
   - **Expected result**: Negotiation timing in protocol sequence

**Deliverable**: Document `UPSTREAM_PROTOCOL_FLOW.md` with annotated timeline

**Estimated Effort**: 2-4 hours of careful source reading

---

### Phase 2: Diagnose Current Implementation (HIGH PRIORITY)

**Objective**: Identify where oc-rsync deviates from upstream timing

**Tasks**:

1. **Trace current daemon flow**
   - [ ] Read `crates/daemon/src/daemon/sections/module_access.rs:600-690`
   - [ ] Note when HandshakeResult is created (line ~646)
   - [ ] Note `compat_exchanged=false` setting (line 652)
   - [ ] Note when `run_server_with_handshake()` is called (line 667)
   - **Expected result**: Current event sequence diagram

2. **Trace current server setup**
   - [ ] Read `crates/core/src/server/mod.rs:141-245`
   - [ ] Note when `setup_protocol()` is called (line 173)
   - [ ] Note when OUTPUT multiplex is activated (line 203-205)
   - [ ] Note when MSG_IO_TIMEOUT is sent (line 209-217)
   - [ ] Note that INPUT multiplex activation is deferred (line 219-221)
   - **Expected result**: Server initialization event sequence

3. **Compare with upstream**
   - [ ] Create side-by-side comparison of event sequences
   - [ ] Identify timing deviations
   - [ ] Identify missing flushes or sync points
   - **Expected result**: Gap analysis document

**Deliverable**: Document `TIMING_GAP_ANALYSIS.md`

**Estimated Effort**: 1-2 hours

---

## Implementation Phase

### Phase 3: Fix Compat Flags Exchange Timing (HIGH PRIORITY)

**Objective**: Ensure compat flags are sent at the correct protocol layer

**Hypothesis**: Compat flags must be sent BEFORE OUTPUT multiplex is activated

**Tasks**:

1. **Move compat exchange earlier**
   - [ ] In `crates/core/src/server/mod.rs`, move `setup_protocol()` call
   - [ ] Ensure it happens BEFORE `ServerWriter::activate_multiplex()`
   - [ ] Add explicit `flush()` after compat flags write
   - **Expected result**: Compat flags sent as plain data

2. **Verify with protocol tracing**
   - [ ] Enable `TracingWriter` in daemon mode (already present)
   - [ ] Run interop test with rsync 3.0.9
   - [ ] Check `/tmp/rsync-trace/daemon-write-*.bin` for compat varint
   - [ ] Verify varint appears BEFORE any multiplex frames
   - **Expected result**: Trace shows correct byte sequence

3. **Test incrementally**
   - [ ] Test with protocol 29 (no compat flags, no negotiation)
   - [ ] Test with protocol 30 (compat flags, no negotiation yet)
   - [ ] Test with protocol 31 (add MSG_IO_TIMEOUT)
   - [ ] Test with protocol 32 (prepare for negotiation)
   - **Expected result**: Incremental validation

**Deliverable**: Working compat flags exchange for protocol 30+

**Estimated Effort**: 2-3 hours

---

### Phase 4: Fix Multiplex Activation Timing (HIGH PRIORITY)

**Objective**: Match upstream multiplex activation sequence exactly

**Hypothesis**: OUTPUT multiplex must be activated AFTER compat exchange, but INPUT activation timing differs by role

**Tasks**:

1. **Implement upstream activation sequence**
   - [ ] In `crates/core/src/server/mod.rs`, reorder events:
     ```rust
     1. setup_protocol() - sends compat flags as PLAIN data
     2. stdout.flush()   - ensure compat flags are sent
     3. Activate OUTPUT multiplex (if protocol >= 23)
     4. Send MSG_IO_TIMEOUT via multiplex (if protocol >= 31)
     5. Defer INPUT multiplex to role-specific handlers
     ```
   - **Expected result**: Correct layer separation

2. **Verify INPUT multiplex timing per role**
   - [ ] Receiver: Activate INPUT after reading filter list (do_server_recv:1167)
   - [ ] Generator: Activate INPUT based on need_messages_from_generator
   - [ ] Update `ReceiverContext::run()` and `GeneratorContext::run()`
   - **Expected result**: INPUT multiplex at correct points

3. **Add sync points**
   - [ ] Add `flush()` before each stream transformation
   - [ ] Add `flush()` after compat flags write
   - [ ] Add `flush()` before multiplex activation
   - **Expected result**: No buffered data crossing layer boundaries

**Deliverable**: Multiplex activation matching upstream timing

**Estimated Effort**: 3-4 hours

---

### Phase 5: Re-enable Capability Negotiation (MEDIUM PRIORITY)

**Objective**: Integrate `negotiate_capabilities()` at the correct protocol flow point

**Prerequisites**:
- Phase 3 complete (compat flags working)
- Phase 4 complete (multiplex timing correct)

**Tasks**:

1. **Identify negotiation integration point**
   - [ ] Based on upstream source reading (Phase 1, Task 4)
   - [ ] Determine if negotiation happens:
     - Option A: After compat flags, BEFORE OUTPUT multiplex (plain I/O)
     - Option B: After OUTPUT multiplex (multiplexed I/O)
   - [ ] Determine correct stream references (plain stdin/stdout vs wrapped)
   - **Expected result**: Integration point specification

2. **Implement negotiation at correct point**
   - [ ] Un-comment negotiation call in `setup_protocol()` if Option A
   - [ ] Or move to `run_server_with_handshake()` after multiplex if Option B
   - [ ] Pass correct stream references (plain or multiplexed)
   - [ ] Store negotiated algorithms in `HandshakeResult` or `ServerConfig`
   - **Expected result**: Negotiation properly integrated

3. **Test negotiation with upstream clients**
   - [ ] Test protocol 30: Should negotiate MD5/zlib (default)
   - [ ] Test protocol 31: Should negotiate per client preference
   - [ ] Test protocol 32: Should negotiate zstd if client supports
   - [ ] Verify negotiated algorithms are logged
   - **Expected result**: Successful algorithm negotiation

**Deliverable**: Working capability negotiation for protocol 30+

**Estimated Effort**: 2-3 hours

---

### Phase 6: Thread Negotiated Algorithms (MEDIUM PRIORITY)

**Objective**: Use negotiated algorithms for actual checksum/compression operations

**Tasks**:

1. **Store negotiation results**
   - [ ] Add fields to `HandshakeResult`:
     ```rust
     pub negotiated_checksum: Option<ChecksumAlgorithm>,
     pub negotiated_compression: Option<CompressionAlgorithm>,
     ```
   - [ ] Populate from `negotiate_capabilities()` result
   - **Expected result**: Algorithms available to role handlers

2. **Wire checksum algorithm**
   - [ ] Pass `negotiated_checksum` to `ReceiverContext`
   - [ ] Use in signature generation (currently hardcoded)
   - [ ] Use in delta verification
   - **Expected result**: Correct checksum algorithm used

3. **Wire compression algorithm**
   - [ ] Pass `negotiated_compression` to compression layer
   - [ ] Use for compressing deltas
   - [ ] Use for decompressing received data
   - **Expected result**: Correct compression algorithm used

**Deliverable**: Functional use of negotiated algorithms

**Estimated Effort**: 2-3 hours

---

## Validation Phase

### Phase 7: Comprehensive Interoperability Testing (HIGH PRIORITY)

**Objective**: Validate all protocol versions against upstream rsync

**Tasks**:

1. **Test matrix**
   - [ ] Protocol 30 + rsync 3.0.9 client → oc-rsync daemon
   - [ ] Protocol 30 + oc-rsync client → rsync 3.0.9 daemon
   - [ ] Protocol 31 + rsync 3.1.3 client → oc-rsync daemon
   - [ ] Protocol 31 + oc-rsync client → rsync 3.1.3 daemon
   - [ ] Protocol 32 + rsync 3.4.1 client → oc-rsync daemon
   - [ ] Protocol 32 + oc-rsync client → rsync 3.4.1 daemon
   - **Expected result**: All combinations pass

2. **Verify protocol traces**
   - [ ] Capture wire traces for each combination
   - [ ] Compare byte-for-byte with upstream rsync traces
   - [ ] Verify compat flags match
   - [ ] Verify negotiation strings match
   - **Expected result**: Wire-compatible protocol implementation

3. **Run full interop suite**
   - [ ] `bash tools/ci/run_interop.sh` must pass
   - [ ] All three versions must succeed
   - [ ] No "unexpected tag" errors
   - [ ] Files transferred successfully
   - **Expected result**: Clean interop test run

**Deliverable**: Passing interoperability tests

**Estimated Effort**: 1-2 hours

---

## Encoding and Character Set Considerations

### Phase 8: Charset Negotiation (FUTURE WORK - LOW PRIORITY)

**Objective**: Support iconv-based filename encoding conversion

**Background**:
- Current implementation assumes UTF-8 for algorithm names (safe)
- Filenames may use different encodings (Windows codepages, macOS normalization)
- Upstream rsync supports `--iconv` for charset conversion

**Tasks** (deferred until basic protocol is stable):

1. **Research upstream iconv implementation**
   - [ ] Read `options.c` iconv parsing
   - [ ] Read `util.c` iconv conversion functions
   - [ ] Identify how charset is negotiated
   - **Expected result**: Charset negotiation specification

2. **Implement charset negotiation**
   - [ ] Add charset field to `NegotiationResult`
   - [ ] Parse client charset from capabilities
   - [ ] Negotiate mutual charset (or conversion plan)
   - **Expected result**: Charset negotiation ready

3. **Implement filename conversion**
   - [ ] Add iconv-style conversion to filename encoding
   - [ ] Apply during file list transmission
   - [ ] Test cross-platform (Linux ↔ macOS ↔ Windows)
   - **Expected result**: Filenames correctly encoded

**Deliverable**: Cross-platform filename encoding support

**Estimated Effort**: 6-8 hours (future work)

---

## Risk Mitigation

### Known Risks

1. **Risk**: Upstream rsync source is complex and hard to follow
   - **Mitigation**: Focus on specific function flows, use ctags/cscope, add printf debugging to upstream build

2. **Risk**: Timing changes might break SSH mode
   - **Mitigation**: Test both daemon mode AND SSH mode after each phase

3. **Risk**: Protocol traces are hard to interpret
   - **Mitigation**: Use existing `TracingReader/TracingWriter`, add hex dump formatting

4. **Risk**: Pre-existing protocol bugs might surface
   - **Mitigation**: Fix incrementally, one protocol version at a time

### Testing Strategy

1. **Unit tests**: Capability negotiation module (already 8/8 passing)
2. **Integration tests**: Protocol timing with mock clients
3. **Interop tests**: Against real upstream rsync binaries
4. **Regression tests**: Ensure SSH mode still works

---

## Success Criteria

### Minimum Viable Product (MVP)
- [ ] Upstream rsync 3.0.9 client can transfer files via oc-rsync daemon (protocol 30)
- [ ] Upstream rsync 3.1.3 client can transfer files via oc-rsync daemon (protocol 31)
- [ ] Upstream rsync 3.4.1 client can transfer files via oc-rsync daemon (protocol 32)
- [ ] No "unexpected tag" errors
- [ ] Compat flags exchanged correctly
- [ ] `tools/ci/run_interop.sh` passes completely

### Full Feature Parity
- [ ] All MVP criteria met
- [ ] Capability negotiation active for protocol 30+
- [ ] Negotiated checksum algorithms are used
- [ ] Negotiated compression algorithms are used
- [ ] Algorithm choices logged in daemon output
- [ ] SSH mode still works (regression test)

### Future Enhancements
- [ ] Charset negotiation for filenames
- [ ] iconv-style encoding conversion
- [ ] Cross-platform filename compatibility

---

## Timeline Estimate

| Phase | Effort | Dependencies | Priority | Status |
|-------|--------|--------------|----------|--------|
| Phase 0 (Protocol 32 Req) | 2-3h | None | CRITICAL | ⏳ TODO |
| Phase 1 (Investigation) | 2-4h | Phase 0 | HIGH | ⏳ TODO |
| Phase 2 (Diagnosis) | 1-2h | Phase 1 | HIGH | ⏳ TODO |
| Phase 3 (Compat Flags) | 2-3h | Phase 2 | HIGH | ⏳ TODO |
| Phase 4 (Multiplex) | 3-4h | Phase 3 | HIGH | ⏳ TODO |
| Phase 5 (Negotiation) | 2-3h | Phase 4 | MEDIUM | ⏸️ BLOCKED |
| Phase 6 (Wire Algorithms) | 2-3h | Phase 5 | MEDIUM | ⏸️ BLOCKED |
| Phase 7 (Validation) | 1-2h | Phase 6 | HIGH | ⏸️ BLOCKED |
| Phase 8 (Charset) | 6-8h | Phase 7 | LOW | 📋 FUTURE |

**Total MVP Effort**: 13-21 hours (was 11-18h, +2-3h for Phase 0)
**Total Full Feature**: 15-24 hours (was 13-21h, +2-3h for Phase 0)
**Total with Charset**: 21-32 hours (was 19-29h, +2-3h for Phase 0)

**Critical Path**: Phase 0 → Phase 1 → Phase 2 → Phase 3 → Phase 4 → Phase 7

**Note**: Phase 0 is a potential blocker. If Protocol 32 components are incomplete, additional implementation work will be needed before timing fixes can succeed.

---

## Notes and Observations

### Key Insights from Current Implementation

1. **Native processing works**: The gatekeeper bug is fixed (commit fa0c4350), requests are handled natively
2. **Negotiation module is complete**: All code and tests are ready, just needs correct integration
3. **Protocol tracing exists**: `TracingReader/TracingWriter` already available for debugging
4. **The issue is timing**: Not functionality, just the order of operations

### Critical Questions to Answer (Phase 1)

1. Does upstream activate OUTPUT multiplex BEFORE or AFTER compat flags?
2. Does upstream send compat flags as plain data or multiplexed data?
3. When does upstream activate INPUT multiplex relative to filter list reading?
4. Is capability negotiation plain I/O or multiplexed I/O?

### Implementation Philosophy

- **Incremental**: Fix one protocol version at a time (30 → 31 → 32)
- **Test-driven**: Verify each change with interop tests before proceeding
- **Trace-based**: Use wire traces to validate byte-level correctness
- **Upstream-aligned**: Match upstream timing exactly, don't innovate

---

## References

- **VALIDATION_REPORT.md**: Original analysis identifying the gatekeeper bug
- **CLAUDE.md**: Workspace conventions and protocol timing notes
- **Upstream rsync 3.4.1**: Reference implementation
  - `main.c:1187-1262` (start_server)
  - `main.c:1144-1180` (do_server_recv)
  - `compat.c:572-644` (setup_protocol)
  - `compat.c:534-585` (negotiate_the_strings)
- **Existing implementation**:
  - `crates/protocol/src/negotiation/capabilities.rs` (complete, tested)
  - `crates/core/src/server/setup.rs` (needs timing fix)
  - `crates/core/src/server/mod.rs` (needs event reordering)

---

**END OF PLAN**
