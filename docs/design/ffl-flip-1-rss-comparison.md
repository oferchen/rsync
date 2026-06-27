# FFL-FLIP.1: RSS profile - legacy `FileEntry` vs flat `FileEntryHeader` at 1M files

Task: FFL-FLIP.1 (#4008). Branch: `docs/ffl-flip-1-rss-comparison`.
Prerequisites: RSS-A.5.a (24-byte `FileEntryHeader`), RSS-A.9.a (1M-file fixture),
RSS-A.9.b (peak-RSS instrumentation), RSS-A.9.c (comparison methodology).
Downstream: FFL-FLIP.3 (flip decision matrix), FFL-FLIP.4 (Cargo default flip).

## 1. Scope

Synthesise per-entry sizes, project heap delta at 1M files, surface the bench
inputs needed to execute FFL-FLIP.3, and emit a GO / HOLD / REVERT
recommendation for the FFL-FLIP series. Out of scope: throughput regression
(FFL-FLIP.2 owns that), wire-byte parity (RSS-A.6.g already covers it).

## 2. Per-entry sizes

Source: `docs/audits/rss-3-fileentry-size-breakdown.md` (RSS-A.2 layout
audit) and `docs/design/flat-flist-representation.md` (RSS-A.5.a header).

### 2.1 Inline footprint per entry

| Representation | Linux | macOS | Windows | Source |
|---|---|---|---|---|
| Legacy `FileEntry` inline | 88 B | 88 B | 104 B | `crates/protocol/src/flist/entry/tests.rs:299-311` |
| Flat `FileEntryHeader` inline | 24 B | 24 B | 24 B | RSS-A.5.a, asserted `Copy + sized` |
| Per-entry delta | **-64 B** | **-64 B** | **-80 B** | flat is 3.67x smaller (Unix), 4.33x (Windows) |

Windows pays the extra 16 B on legacy because `PathBuf` carries
`Wtf8Buf::is_known_utf8`, padding `OsString` from 24 to 32 B. The flat
header is platform-invariant - `PathHandle` and `ExtrasRef` are both 4 B.

### 2.2 Heap cost per entry (vanilla regular file)

20-byte basename, 12-byte parent directory, no extras, no xattrs.

| Source | `name` heap | `dirname` heap | `extras` heap | Per-entry total |
|---|---|---|---|---|
| Legacy `FileEntry` | 32 B (PathBuf, rounded) | ~0.3 B amortised (`Arc<Path>` via interner) | 0 (None) | ~32 B |
| Flat `FileEntryHeader` | 20 B (PathArena byte-packed) | ~0.012 B amortised (PathArena dedup) | 0 (zero-len ExtrasRef) | **~20 B** |
| Heap delta | -12 B | -0.3 B | 0 | ~12 B/entry, no per-alloc metadata |

Flat eliminates one allocator round-trip per entry. Upstream's `pool_alloc`
hits the allocator ~once per 8K-32K entries; flat matches that envelope via
two `Vec`-backed arenas (`PathArena`, `ExtrasArena`).

### 2.3 Heap cost per entry (full extras: `-A -X -H --atimes --crtimes --checksum`)

| Source | Header | Extras tail | Per-entry total |
|---|---|---|---|
| Legacy `FileEntry` | 88 B inline | 240 B `Box<FileEntryExtras>` (256 B size class) | ~344 B |
| Flat `FileEntryHeader` | 24 B inline | ~200 B length-prefixed packed in `ExtrasArena` | **~224 B** |
| Per-entry delta | -64 B | -40 B | -120 B (-35%) |

## 3. 1M-file projection

### 3.1 Inline-only projection

| Representation | Linux | Windows |
|---|---|---|
| Legacy `FileEntry` x 1M | 88 MB | 104 MB |
| Flat `FileEntryHeader` x 1M | **24 MB** | **24 MB** |
| Inline savings | -64 MB | -80 MB |

### 3.2 Total heap projection (vanilla workload, 1M files, 1000 unique dirs)

| Representation | Inline | Name heap | Dirname heap | Total RSS contribution |
|---|---|---|---|---|
| Legacy `FileEntry` | 88 MB | 32 MB | 32 KB | ~120 MB |
| Flat `FileEntryHeader` | 24 MB | 20 MB | 12 KB | **~44 MB** |
| Savings | -64 MB | -12 MB | negligible | **-76 MB (-63%)** |

### 3.3 Reference points

| Source | Measured RSS at 1M files | Notes |
|---|---|---|
| upstream rsync 3.4.1 (INC_RECURSE) | 7.6 MB | RSS-1.b baseline |
| upstream rsync 3.4.1 (no INC_RECURSE) | 76.8 MB | RSS-1.b baseline |
| oc-rsync legacy (INC_RECURSE) | 197 MB | RSS-1.b; current default |
| oc-rsync legacy (no INC_RECURSE, full hold) | ~198 MB | RSS-1.b |
| Flat-flist target (RSS-A.9.a prediction) | < 85 MB | acceptance ceiling |
| Flat-flist projection (this doc, vanilla) | ~44 MB | header + interned paths only |

Closing the 26x INC_RECURSE gap and the 2.6x no-INC gap requires the flat
representation to ship in production read paths AND in the segment-reclaim
contract (`docs/design/rss-a8b-arena-growth-strategy.md`).

## 4. Decision inputs

Status of RSS-A.LAND bench cells:

| Cell | Workload | Feature set | Required for FFL-FLIP.3 | Status |
|---|---|---|---|---|
| RSS-A.LAND.1 | 1M-file flist build RSS | default (flat-flist OFF) | control baseline | NOT RUN |
| RSS-A.LAND.2 | 1M-file flist build RSS | `--features flat-flist` | dual-write cost | NOT RUN |
| RSS-A.LAND.3 | 1M-file throughput | default (flat-flist OFF) | throughput control | NOT RUN |
| RSS-A.LAND.4 | 1M-file throughput | `--features flat-flist` | throughput delta | NOT RUN |

`RSS-A.LAND.2 / .4 must be re-run before flip-default decision`.
The current `DualFileList` writes both legacy and flat sides on every
`push`, so RSS-A.LAND.2 measures dual-write overhead, NOT flat in
isolation. The isolated saving the projection above describes is realised
only after FFL-7..10 cut reads over and FFL-9..11 remove the legacy side.

Read-side validation gap (FFL-1 sec. 5): no production code path reads
`DualFileList::flat()` today. The FFL-FLIP.4 default flip is unsafe to
schedule before the read-side cutover lands in the same PR.

## 5. Recommendation

**HOLD - dual-keep until RSS-A.LAND.2 + .4 bench numbers land and FFL-7..10
read-side cutover ships in the same PR as FFL-FLIP.4.**

| Verdict | Triggering criteria | Next action |
|---|---|---|
| GO (flip default in next sprint) | RSS-A.LAND.2 - .1 <= 10% RSS overhead AND RSS-A.LAND.4 - .3 <= 5% throughput regression AND FFL-7..10 land in the same PR | execute FFL-FLIP.3 then FFL-FLIP.4 |
| HOLD (current recommendation) | any RSS-A.LAND cell unmeasured OR read-side cutover not staged | run RSS-A.LAND.1..4 in podman bench container, then re-evaluate via FFL-FLIP.3 |
| REVERT (rip flat-flist out) | RSS-A.LAND.2 - .1 > 30% RSS regression that cannot be closed by removing dual-write OR wire-byte divergence found in interop | open revert PR for RSS-A.5/.6/.7/.8/.11, reopen RSS-A series |

The projection in section 3 says flat is a -63% RSS win on vanilla
1M-file workloads once dual-write is removed. The decision-time risk is
that the current `--features flat-flist` build measures the dual-write
floor (legacy + flat both live), not the flat ceiling. Holding until the
bench numbers exist - and staging the FFL-FLIP.4 default flip behind
FFL-7..10 read-side cutover in one PR - matches
`feedback_concurrent_path_discipline.md` (PIP-7 corruption lesson).

## 6. Cross-links

- FFL-4 decision matrix: `docs/design/ffl-4-flat-flist-flip-decision.md`
- FFL-FLIP.3 (decision matrix execution): tracked at task #4010
- FFL-FLIP.4 (Cargo flip PR): tracked at task #4011
- RSS-A.2 layout audit: `docs/audits/rss-3-fileentry-size-breakdown.md`
- RSS-A.5.a header definition: `docs/design/flat-flist-representation.md`
- RSS-A.9.c comparison methodology: `docs/design/flat-flist-rss-comparison.md`
- RSS-A.9.a fixture spec: `docs/design/flat-flist-rss-bench-fixture.md`
- Dual-write overhead audit: `docs/audits/ffl-1-dualfilelist-overhead.md`

## 10. MEASURED RESULT (2026-06-27) — projection INVALIDATED

The 1M in-memory bench (`crates/protocol/benches/flat_flist_rss.rs::bench_rss_profile`,
shared-path distribution, VmRSS via /proc/self/status) was finally executed:

| Representation | Inline | Measured RSS @ 1M |
|---|---|---|
| Legacy `Vec<FileEntry>` | 80 B | 76.3 MiB |
| Flat `FlatFileList` | 48 B header | 95.8 MiB |
| Ratio (flat/legacy) | 1.67x smaller inline | **1.255x LARGER total** |

The Section 2-3 projection assumed a 24-byte `FileEntryHeader`. The header is now
**48 bytes**, and the path-arena interning of 1M unique basenames plus the extras
arena make flat-only **25% larger** than legacy, not 63% smaller. The -63% RSS
premise is empirically false at the current implementation.

**Decision: REVERT (FFL-4 Option C).** Flat-only does not beat legacy, so keeping
the dual path (or flipping to flat-only) cannot deliver the RSS win. Remove the
flat path; keep the legacy `Vec<FileEntry>`.
