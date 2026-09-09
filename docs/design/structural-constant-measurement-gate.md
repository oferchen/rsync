# The measurement gate for structural constants

Buffer sizes, queue bounds, ring depths, spin thresholds and retry ceilings are
decisions. This note records the rule that governs changing them, the evidence
each existing constant rests on, and what a proposal has to carry before it is
reviewable.

The rule exists because the alternative is invisible: a constant with no cited
basis reads exactly like one with a measured basis, and both read exactly like
one whose value is wrong. Only the provenance distinguishes them, and provenance
has to be written down at the site or it is lost.

## The rule

**A change to a structural constant must name its basis before it is proposed.**
Exactly one of:

1. **Upstream-mirrored.** The value is what upstream rsync uses, cited
   `file.c:line` against the pinned source. Mirroring upstream is the standing
   default: it needs no benchmark, only a correct citation.
2. **Measured.** A named measurement, with the harness and the numbers, showing
   the value sits where the curve says it should.
3. Neither. Then it is not ready to propose.

A pull request that retunes a constant without one of the first two is asking a
reviewer to arbitrate taste. Nobody can do that, so such changes get approved on
plausibility, which is how an unmeasured value becomes load-bearing.

### Three corollaries, each learned from a specific failure

**Establish reachability before measuring.** A sweep of a constant on a code path
production never enters produces a flat curve and a confident conclusion, both
worthless. The zero-copy send path was believed live and measured as dead code
for every transfer between two oc peers; the io_uring path was measured at
roughly zero net effect, and the one real io_uring finding of that period was a
2.47x regression in batch submission that no buffer-size sweep would have found.
Reachability is the first measurement, not an assumption behind it.

**A negative control is part of the measurement.** Shrink the resource and
confirm the metric moves. Without that, "the miss rate is zero" cannot be
distinguished from "the counter is not wired" or "the path is not entered".

**Classify on the site, never on the value.** A platform-derived bound and a
hardcoded one are indistinguishable from the number alone on the platform where
they happen to coincide. `max_path_len()` in `crates/fast_io/src/path_limit.rs`
returns `libc::PATH_MAX`, which is 4096 on Linux; read as a value it looks like a
hardcoded 4096, and an audit that stopped at the value reported a divergence that
does not exist. Read the producing site.

## What upstream-mirrored looks like

Upstream's own constants are the model, and two of them show why the citation
carries information the number does not.

`MIN_FILECNT_LOOKAHEAD` and `MAX_FILECNT_LOOKAHEAD` are defined at
`rsync.h:151-152` (1000 and 10000). The definitions are what a mirror cites. The
consumers are elsewhere - `sender.c:515` and `sender.c:549` pass the minimum to
`send_extra_file_list`, `generator.c:2703` and `generator.c:2775` gate flushing
on half of it, and `io.c:837` bounds the reader against the maximum - and citing
a consumer instead of the definition is a common way for a citation to look
right while pointing at code that can move independently.

`BIGPATHBUFLEN` at `rsync.h:765-769` is the sharper example:

```c
#if MAXPATHLEN < 4096
#define BIGPATHBUFLEN (4096+1024)
#else
#define BIGPATHBUFLEN (MAXPATHLEN+1024)
#endif
```

With `MAXPATHLEN` taken from the system header (`rsync.h:760-762` supplies 1024
only `#ifndef`), Linux takes the second arm and macOS the first, and both land on
5120. oc's `MAX_FILTER_RULE_LEN` is 5120 with that derivation written out at the
site. The value is identical on every supported platform; the note explaining
that it arrives by two different routes is what stops a future reader from
"simplifying" it into a divergence.

`DEFAULT_MAX_ALLOC` (`options.c:209`) and the xattr wire caps are mirrors of the
same kind: cited, and correct because they are cited.

## What measured looks like

Two precedents define the standard.

**The exemplar is a fix, not a tuning.** On a daemon pull of 10,000 files,
upstream took about 941 ms and oc took 1106 ms with io_uring enabled. Disabling
io_uring entirely gave 460 ms, so the feature was costing 2.47x rather than
paying. The cause was that submitting a batch drained the ring between batches,
leaving a one-chunk batch with no concurrency and a round trip it could not
amortise. Routing small batches past the ring gave 450 ms - on the floor set by
the io_uring-off arm, and 2.1x faster than upstream.

What makes that a model:

- Both arms were rebuilt from source. The inherited binaries had identical byte
  sizes, which is a provenance smell, not a coincidence.
- Runs were interleaved three times and system load was recorded falling across
  them, so the improvement is not a quiet machine.
- Correctness was proven separately, by comparing digests of all 10,000 files.
  A transferred-file count cannot detect corruption on a write-path change.
- The threshold is mutation-lethal: restoring it reddens a named test.

**The other precedent is a decision not to build.** A proposal to make the engine
buffer pool self-resizing was measured first and declined: the churn the resizer
would fix was not there. The pool's static, environment-variable-only sizing was
then documented as intentional. A gate that only ever says yes is not a gate, and
"measured, declined" is a legitimate and cheap outcome.

## Constants that currently have no basis

Enumerated from the tree, classified by reading each producing site. These carry
neither an upstream citation nor a named measurement. That is a statement about
their provenance, not an assertion that any value is wrong.

| Constant / cluster | Site | Gate that would settle it |
| --- | --- | --- |
| Pipeline queue bounds (13 sites) | `crates/transfer/src/pipeline/mod.rs`, `disk_commit/config.rs`, `reorder_buffer/mod.rs`, `delta_pipeline/mod.rs`, `parallel_io.rs`, `generator/transfer/transfer_loop.rs` | Sweep each bound independently on the throughput harnesses. A bound whose sweep is flat should be derived by the governor or removed, not retuned. |
| `SPIN_LIMIT` = 512 | `crates/transfer/src/pipeline/spsc.rs:30` | Sweep a decade either side recording wall-clock *and* idle CPU. The spin-then-park shape is measured; the threshold is not. |
| `MAX_RETRY_COUNT` = 2 | `crates/transfer/src/pipeline/job.rs:23` | Not a sweep. A retry ceiling is a policy question against the standing no-retry preference: justify the mechanism or remove it. |
| io_uring depth, session pool, registered buffers | `crates/fast_io/src/io_uring_depth.rs:31`, `io_uring/session_pool.rs:62`, `io_uring/registered_buffers/mod.rs:88` | Reachability first. Then a sweep with miss counters exposed and a shrink-the-pool negative control. |
| `send_zc` slot sizing | `crates/fast_io/src/io_uring/send_zc.rs:282,291,301` | Blocked on reachability: this path was measured unreachable from an oc client. Prove entry with a counter or a syscall trace before any sweep. |
| PBUF_RING shape (64 x 64 KiB) | `crates/fast_io/src/io_uring_common.rs:465-470` | High-fan-out sweep with a negative control. Without the control a flat result cannot separate "well sized" from "not on the path". |
| Daemon listener and request-line bounds | `crates/daemon/src/async_listener.rs:73`, `daemon/async_session/listener.rs:25`, `client_args/request_line.rs:13`, `sections/name_converter.rs:210`, `server_runtime/listener.rs:155` | Not a benchmark. These face a peer: find upstream's counterpart and cite it, or state what an unauthenticated peer can force at this value versus half and double, and pin the boundary with a test that fires *at* the cap. |
| BitHash sizing factor | `crates/matching/src/index/mod.rs:73` | Measure the post-tag rejection rate on a real basis corpus at 4x, 8x, 16x. |
| fd-limit target | `crates/core/src/fd_limit.rs:11` | No benchmark: derive it from upstream's own formula and show the arithmetic. |
| Recursive-dir depth, 100 versus 1000 | `crates/engine/src/local_copy/executor/directory/recursive/mod.rs:57,59` | No benchmark: two bounds on one concept disagree, so one is wrong. Reconcile to a single owner. |

The peer-facing row is first among equals. For everything else an arbitrary value
costs throughput; there, it is what an untrusted peer can make the daemon accept.

## Three that looked mirrored and were not

Value equality is not provenance. Each of these matched upstream's value while
resting on a rule that did not hold in general, and all three have since been
closed. The entries stay, with their closures recorded, because the shape recurs
and because each fix is the evidence that the reading was right.

**The hash table mirrored a floor and dropped the growth.** `TAG_TABLE_SIZE` is
`1 << 16` at `crates/matching/src/index/mod.rs:68`, documented as matching
upstream's `TABLESIZE`. Upstream's constant is named `TRADITIONAL_TABLESIZE`
(`match.c:45`) and is a floor, not a size: `build_hash_table` computes
`tablesize = (s->count/8) * 10 + 11` and raises it to the floor only if it comes
out smaller (`match.c:84-88`), with the stated intent of holding hash load near
80 percent for big files. Upstream then runs different insert and probe paths
depending on whether it grew (`match.c:98`, `match.c:215`). Above roughly 52k
blocks upstream's table grew and oc's did not, so oc's chains lengthened as block
count rose. Closed by #7790: `CompactLookup` grows on upstream's rule and carries
both addressing modes, and `TAG_TABLE_SIZE` is documented as the tag-array size
alone, explicitly not the growth-bearing structure. The warning in the original
entry held - the two-path branch and the odd-number constraint did travel with
the formula.

**A digest bound was written where upstream derives one.** `MAX_XATTR_DIGEST_LEN`
was the literal 16 in `crates/protocol/src/xattr/mod.rs`; upstream writes
`#define MAX_XATTR_DIGEST_LEN MD5_DIGEST_LEN` (`xattrs.c:48`). The values agreed
because MD5 is 16 bytes, so a digest change would have moved upstream's bound and
not oc's, and the symptom would have been a wire-length mismatch in a decoder
rather than a compile error. Closed by #7786: the bound is derived from the hasher
that fills the buffer, with a const assertion pinning it to 16 so the derivation
cannot drift into a wire-format change.

**One bound was copied at every backend.** `MAX_INPUT_SIZE` (1 MiB) was defined
independently in each SIMD checksum backend under
`crates/checksums/src/simd_batch/`. Each copy decides whether its backend bails
to the scalar path, so one edit could desynchronise batch eligibility across
architectures, and parity tests that compare outputs cannot see that, because
both paths produce correct output. Closed by #7787: one definition at
`simd_batch/mod.rs:46`, every backend importing it, and the bound itself tested
for the first time. The count in the original entry was low - the sweep found
twelve sites, eleven constants plus one bare literal that no grep for the name
could reach.

## What a proposal has to carry

- The constant, its site, and its current basis, using the three categories above.
- For an upstream mirror: the citation, resolving against the pinned source.
- For a measurement: the harness, the arms, the numbers, and a negative control.
  Rebuild both arms from source and say so.
- For a peer-facing bound: what changes about what an untrusted peer can force.
- A test that fails if the constant moves back, so the decision is pinned rather
  than remembered.
