# Fuzz coverage gap followups

Companion to `docs/audits/fuzz-coverage-matrix.md`. The matrix ranks the
remaining gaps; this document decomposes them into individually filable
follow-up tasks with concrete parse-function citations, `Arbitrary` input
recommendations, and invariant assertions.

The audit that produced these tasks comes out of the FCV-3 (#2316)
breakdown. FCV-6 has already shipped `flist_entry_decode` (top-level
`fuzz/fuzz_targets/flist_entry_decode.rs`), so the file-list state
machine is no longer the open P0 in row 1 of the matrix. The two
remaining P0 gaps from Table 2 (`negotiation_prologue` and
`capability_vstring`) are first in the queue below, followed by P1
candidates that are ready to file without further design work.

For each gap we list:

- **Task slot** - placeholder FCV-NN identifier reserved for the
  follow-up issue. Reuse the matrix's section-3 ordering so the queue
  stays readable.
- **Parse function** - `file:line` of the public entry point that
  consumes untrusted bytes.
- **Arbitrary input shape** - raw `&[u8]` when libFuzzer's
  coverage-guided generator is enough on its own, or a structured
  `Arbitrary` shape when the parser needs context (protocol version,
  capability flags, preserve flags, role) to reach interesting branches.
- **Invariants** - panic vs no-panic expectations, plus any roundtrip
  or convergence properties that the harness should assert.
- **Priority justification** - why this surface deserves the slot it
  has, with reference to upstream rsync's reachability analysis.

## P0 follow-ups

### FCV-7 - `negotiation_prologue`

- **Parse function**: `crates/protocol/src/negotiation/sniffer/observe.rs:12`
  (`NegotiationPrologueSniffer::observe`), plus the byte-at-a-time
  driver `observe_byte` at line 65 and the reader-driven `read_from`
  at line 78.
- **Arbitrary input shape**: raw `&[u8]`. The sniffer is a pure
  byte-stream classifier; libFuzzer's coverage-guided generation finds
  branches in `legacy_prefix_complete`, `needs_more_legacy_prefix_bytes`,
  and `planned_prefix_bytes_for_observation` without structured help.
  Drive the slice through three modes per fuzz iteration:
  1. `observe(data)` in one call.
  2. byte-by-byte loop calling `observe_byte` for each byte.
  3. `read_from(&mut Cursor::new(data))` to exercise the `Interrupted`
     and `UnexpectedEof` paths.
- **Invariants**:
  - panic-only across all three drivers.
  - `(decision, consumed)` from `observe` must satisfy
    `consumed <= data.len()`.
  - `reset()` followed by `observe(data)` must return the same
    decision as a fresh sniffer (idempotence under reset).
  - `legacy_prefix_complete()` is monotonic - once true it stays true
    until `reset()` is called.
- **Priority justification**: this is the very first parser any
  connection touches. A panic here is a pre-authentication remote DoS
  reachable by any peer that opens a TCP socket. The sniffer also owns
  buffered prefix retention (`try_reserve_exact`), so adversarial inputs
  that maximise reserved bytes are a memory-pressure vector worth
  exploring early.

### FCV-8 - `capability_vstring`

- **Parse function**: `crates/protocol/src/negotiation/capabilities/negotiate.rs:493`
  (`read_vstring`), wired into `negotiate_capabilities` at line 121 and
  `negotiate_capabilities_with_override` at line 163.
- **Arbitrary input shape**: structured `Arbitrary` with a raw-byte
  payload tail.
  ```rust
  #[derive(Arbitrary, Debug)]
  struct Input {
      protocol: u8,            // mod 5 -> ProtocolVersion::V28..V32
      role: bool,              // is_server
      mode: bool,              // is_daemon_mode
      send_compression: bool,
      payload: Vec<u8>,        // pre-staged peer reply stream
  }
  ```
  Pre-stage `payload` as the peer's vstring stream into a
  `Cursor<Vec<u8>>` for `stdin` and a sink `Vec<u8>` for `stdout`. Run
  both `negotiate_capabilities` and
  `negotiate_capabilities_with_override` so the `checksum_override` /
  `compression_override` arms are reached.
- **Invariants**:
  - panic-only.
  - `read_vstring` must reject lengths above `MAX_NSTR_STRLEN` (256)
    with `InvalidData`, never panic.
  - Non-UTF-8 payloads must surface as `InvalidData`, never panic.
  - `negotiate_capabilities*` must never allocate more than
    `2 * MAX_NSTR_STRLEN` bytes for the remote string buffers.
- **Priority justification**: vstring exchange runs before any
  encryption, compression, or framing context exists. Reachable on
  every connection that negotiates protocol >= 30 (which is every
  modern transfer). A panic here matches the threat model of the
  prologue sniffer: unauthenticated remote DoS.

### FCV-9 - `legacy_sniffer` (P0 fill-in)

- **Parse function**: `crates/protocol/src/negotiation/sniffer/legacy.rs:16`
  (`read_legacy_daemon_line`), with the greeting wrappers at lines 81
  (`read_and_parse_legacy_daemon_greeting`) and 91
  (`read_and_parse_legacy_daemon_greeting_details`).
- **Arbitrary input shape**: raw `&[u8]` fed through a
  `Cursor<Vec<u8>>`. Pre-stage the sniffer by feeding the canonical
  `@RSYNCD:` prefix into `observe()` first, then call
  `read_legacy_daemon_line` against the cursor. Repeat with the
  greeting wrappers so the version-line parser is exercised.
- **Invariants**:
  - panic-only.
  - `try_reserve` failures must surface as `io::Error`
    (`map_reserve_error_for_io`), never panic.
  - Line length is bounded - exhaust the cursor before allocation
    exceeds a reasonable cap (assert in the harness that the resulting
    `line.len()` is below e.g. 64 KiB after the call).
- **Priority justification**: this is the P0 row "partial" cell in
  Table 2 - `fuzz_legacy_greeting` reaches the pure parsers but the
  reader-driven helpers are unfuzzed. Reachable on every legacy
  daemon handshake.

## P1 follow-ups

### FCV-10 - `compat_flags`

- **Parse function**: `crates/protocol/src/compatibility/flags.rs:175`
  (`CompatibilityFlags::read_from`), with slice variants at lines 184
  (`decode_from_slice`) and 212 (`decode_from_slice_mut`).
- **Arbitrary input shape**: raw `&[u8]`, driven through both
  `read_from(Cursor::new(data))` and `decode_from_slice(data)` in the
  same harness so the varint backing both paths is hit twice.
- **Invariants**:
  - panic-only.
  - `decode_from_slice_mut` must leave the input slice untouched on
    error (existing contract from the doc example).
  - `decode_from_slice(data).map(|(_, rest)| rest.len()) <= data.len()`.
- **Priority justification**: P1 in Table 2. Compatibility flags gate
  every later codec switch (varint30, longints, incremental flist). A
  miscount here corrupts every subsequent parse, so even though the
  payload is tiny the blast radius is large.

### FCV-11 - `filter_list_wire`

- **Parse function**: `crates/protocol/src/filters/wire.rs:181`
  (`read_filter_list`).
- **Arbitrary input shape**: structured
  `Arbitrary { protocol: u8, payload: Vec<u8> }`. The protocol selector
  controls how `parse_wire_rule` interprets the inner record; the
  payload bytes are the wire stream including the 4-byte LE length
  prefixes and the zero terminator.
- **Invariants**:
  - panic-only.
  - Negative lengths must surface as `InvalidData`.
  - `Vec::with_capacity(len as usize)` is reachable - cap the per-rule
    length the harness will accept (e.g., skip iterations where any
    length prefix exceeds 16 MiB) so libFuzzer does not focus on OOM
    rather than logic bugs.
- **Priority justification**: P1 in Table 2. Distinct surface from the
  text-grammar parser already covered by `fuzz_filter_parse`. Reachable
  whenever the sender ships a filter list (every `--filter`,
  `--exclude*`, `--include*` invocation).

### FCV-12 - `ndx_codec`

- **Parse function**: `crates/protocol/src/codec/ndx/state.rs:97`
  (`NdxState::read_ndx`), with the stateless goodbye sentinel at
  `crates/protocol/src/codec/ndx/goodbye.rs:48` (`read_goodbye`).
- **Arbitrary input shape**: structured
  `Arbitrary { protocol_version: u8, payload: Vec<u8> }`. Maintain a
  single `NdxState` across multiple `read_ndx` calls until the cursor
  drains so the prev-positive / prev-negative caches are exercised.
  Drive `read_goodbye` against the residual bytes.
- **Invariants**:
  - panic-only.
  - Returned indexes must satisfy `i32` bounds (no overflow during the
    `(high << 24) | ...` reassembly).
  - `read_goodbye` must never panic on truncated or wrong-sentinel
    input - only return `InvalidData` / `UnexpectedEof`.
- **Priority justification**: P1 in Table 2. NDX codec gates the
  post-flist phase of every transfer; the byte-reduction encoding has
  enough branches (`0xFF`, `0xFE`, `0x00`, high-bit-set) that
  hand-written tests miss interactions.

### FCV-13 - `incremental_flist`

- **Parse function**: `crates/protocol/src/flist/incremental/streaming.rs:57`
  (`StreamingFileList::next_ready`) and line 86 (`read_all`). Both sit
  on `FileListReader::read_entry` at
  `crates/protocol/src/flist/read/mod.rs:469`.
- **Arbitrary input shape**: reuse `flist_entry_decode`'s structured
  shape (protocol selector, preserve-flag matrix, payload) and wrap the
  cursor in `StreamingFileList::new`. The added value is exercising
  the segment / parent-dir state machine that sits on top of the
  per-entry decoder.
- **Invariants**:
  - panic-only.
  - `ready_count() + pending_count()` must equal the number of
    successfully-decoded entries.
  - `is_finished_reading()` is monotonic.
- **Priority justification**: P1 in Table 2. Stacks on the FCV-6
  surface but exposes additional state. Reachable on every transfer
  that negotiates `CF_INC_RECURSE` (default for protocol 30+).

### FCV-14 - `idlist`

- **Parse function**: `crates/protocol/src/idlist/mod.rs:232`
  (`IdList::read`) and line 256 (`read_with_kind`).
- **Arbitrary input shape**: structured
  `Arbitrary { protocol_version: u8, id0_names: bool, kind: u8, payload: Vec<u8> }`
  with `kind % 3` selecting `None` / `Some(Uid)` / `Some(Gid)`. Pass a
  stub `name_to_id` closure that returns `None` half the time and
  `Some(arbitrary u32)` the rest (use a counter rather than a fresh
  `Arbitrary` per name so the harness stays deterministic).
- **Invariants**:
  - panic-only.
  - No name allocation may exceed the upstream cap (mirror
    `MAX_NSTR_STRLEN` semantics if applicable; otherwise cap at
    `i16::MAX as usize`).
- **Priority justification**: P1 in Table 2. ID lists arrive
  unauthenticated from the daemon; the varint30 path also overlaps
  with FCV-10 coverage so a finding here narrows the blame quickly.

### FCV-15 - `compress_decoders`

- **Parse function**:
  `crates/compress/src/zlib/helpers.rs:17` (`decompress_to_vec`),
  `crates/compress/src/lz4/raw.rs:217,253`
  (`decompress_block`, `decompress_block_to_vec`),
  `crates/compress/src/lz4/frame.rs:182,190`
  (`decompress_to_vec`, `decompress_into`),
  `crates/compress/src/zstd.rs:237,245`
  (`decompress_to_vec`, `decompress_into`).
- **Arbitrary input shape**: structured
  `Arbitrary { codec: u8, payload: Vec<u8> }` with `codec % 4`
  selecting zlib / lz4-raw / lz4-frame / zstd. The existing
  `fuzz/fuzz_targets/decompressor_{zlib,zstd}.rs` cover the zlib and
  zstd wrappers but not the LZ4 paths.
- **Invariants**:
  - panic-only.
  - Output `Vec` must not exceed a configurable cap
    (e.g., `64 * input.len() + 4 KiB`) - the harness asserts this
    rather than letting the wrapper OOM the fuzzer.
- **Priority justification**: P1 in Table 2. The underlying codec
  crates are well-fuzzed upstream, but our thin wrappers compute
  output bounds and call `Vec::with_capacity` - a bad length prefix
  is a memory-blowup vector specific to our code.

## Filing order

The five P0 / P1 follow-ups above (FCV-7 through FCV-11) are the
recommended initial batch. They are each in-process, sub-millisecond
targets that fit the existing nightly fuzz workflow's budget. FCV-12
through FCV-15 can land in a second batch once the prologue and
vstring targets have soaked overnight without false positives.

P2 / P3 gaps (`acl_wire`, `xattr_wire`, `files_from`, `iconv_spec`,
`rsyncd_conf`, `bwlimit_parse`, `skip_compress_list`) are deferred
behind the queue above. They share the same pattern - raw `&[u8]`
through the entry points cited in the matrix - and are listed in
Section 3 of `fuzz-coverage-matrix.md` so they remain visible without
needing per-gap decomposition here.
