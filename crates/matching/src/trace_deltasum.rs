//! `--debug=DELTASUM` producer emissions for the delta-transfer pipeline.
//!
//! Mirrors upstream rsync's `DEBUG_GTE(DELTASUM, n)` output byte-for-byte so
//! diagnostics align across implementations. Every upstream format string has
//! exactly ONE owner function here; the sender scan (this crate), the network
//! sender/receiver/generator drivers (`transfer`), and the local fused
//! delta loop (`engine::local_copy`) all emit through these functions instead
//! of holding their own copies of the text.
//!
//! # Upstream Reference
//!
//! - `match.c` - sender hash-search trace (levels 2-4) and the run totals.
//! - `sender.c` - per-file receive_sums / map / match_sums milestones.
//! - `generator.c` - signature generation geometry and per-chunk sums.
//! - `receiver.c` - basis map, literal/match application, file_sum receipt.
//!
//! Upstream forwards only `--info` words to the remote server
//! (`options.c:3117-3120` builds the server's output option from `info_words`
//! alone), so DELTASUM output is CLIENT-side only: a pull prints the
//! generator+receiver lines, a push prints the sender lines, and a local copy
//! prints both sets. The server-side roles stay silent unless the server
//! itself was invoked with `--debug=deltasum`.
//!
//! # Measured fidelity, against upstream 3.5.0 (`--debug=deltasum{,2,3}`)
//!
//! Fixture: a 256 KB pseudo-random file, 100 bytes overwritten in a backdated
//! basis, `--no-whole-file -t` so the delta path engages.
//!
//! - **Wire pull** (client = generator + receiver): byte-identical at all three
//!   levels, including all 755 lines at level 3.
//! - **Wire push** (client = sender): identical at all three levels except the
//!   two counter lines, whose `hash_hits`/`false_alarms` values differ by the
//!   oc-native counter semantics documented on [`trace_match_counters`].
//!   `matches` and `data` agree exactly (1132 of 1134 level-3 lines identical).
//! - **Local copy**: every line oc emits matches upstream's text exactly, and
//!   the set of emitted lines is a subset of upstream's apart from the same two
//!   counter lines plus the two spelling/geometry residuals below. The ORDER
//!   differs: see the local notes.
//!
//! # Known no-analogue sites and residual divergences
//!
//! - `match.c:210-213` (level 4 `offset=... sum=...`) is emitted by the
//!   sequential scan only; the opt-in parallel chunked scan
//!   (`DeltaGenerator::generate_chunked`) runs its stripes on rayon workers
//!   whose thread-local diagnostic buffers are not collected, so per-match
//!   trace lines from those stripes are not printed. Counters still reach the
//!   merged [`crate::DeltaScript`].
//! - The local fused delta loop writes the reconstructed file directly and
//!   never computes or transfers a whole-file checksum, so it has no analogue
//!   for `sending file_sum` (match.c:465) or `got file_sum` (receiver.c:672).
//! - The local path has no analogue for the sender's `receive_sums()` block
//!   trace (`chunk[%d] len=%d offset=%s sum1=%08x`, sender.c:382-386): nothing
//!   is received: the one in-process signature index serves both roles. Upstream
//!   prints those 375 lines locally only because its local run really does drive
//!   two processes over a socketpair. Nor does it have one for `generating and
//!   sending sums for %d` (generator.c:2363) - the local executor carries no
//!   wire file index, and naming a fabricated one would be inventing output.
//! - **Local ordering**: upstream's local run emits every sender line, then its
//!   receiver process drains the delta and emits `recv mapped` plus all its
//!   `chunk[..] of size ..` lines as a trailing block. oc's local loop is fused,
//!   so it emits `recv mapped` where it opens the basis and each `chunk[..] of
//!   size ..` where it actually copies that block - interleaved with the scan.
//!   The lines are individually byte-identical; only their interleaving differs,
//!   and it differs because the work really is interleaved.
//! - **Local `gen mapped` spelling**: upstream prints `fnamecmp`, relative
//!   because it chdir'd into the destination root. The local executor never
//!   chdirs, so it prints the path the operand resolved to.
//! - **Local `s2length`**: upstream reports `s2length=2` for this fixture, oc
//!   `s2length=16`. That is a pre-existing difference in the local path's
//!   signature geometry (`build_delta_signature` asks for a full 16-byte strong
//!   sum), surfaced - not caused - by this trace. Changing it would change local
//!   block matching, so it is reported rather than adjusted here.

use logging::debug_log;

/// Level 2 `hash search b=%ld len=%s` - scan start with block length and the
/// source length the scan was sized from.
///
/// upstream: match.c:180-182 `hash_search()`.
#[inline]
pub fn trace_hash_search_start(block_length: usize, source_len: u64) {
    debug_log!(Deltasum, 2, "hash search b={block_length} len={source_len}");
}

/// Level 3 `hash search s->blength=%ld len=%s count=%s` - scan parameters.
///
/// upstream: match.c:199-202 `hash_search()`.
#[inline]
pub fn trace_hash_search_params(block_length: usize, source_len: u64, count: u64) {
    debug_log!(
        Deltasum,
        3,
        "hash search s->blength={block_length} len={source_len} count={count}"
    );
}

/// Level 3 `sum=%.8x k=%ld` - the initial window checksum over the first `k`
/// source bytes.
///
/// upstream: match.c:192-193 `hash_search()`.
#[inline]
pub fn trace_initial_sum(sum: u32, k: usize) {
    debug_log!(Deltasum, 3, "sum={sum:08x} k={k}");
}

/// Level 4 `offset=%s sum=%04x%04x` - per-offset rolling sum during the scan.
///
/// upstream: match.c:210-213 `hash_search()`.
#[inline]
pub fn trace_scan_offset(offset: u64, s2: u16, s1: u16) {
    debug_log!(Deltasum, 4, "offset={offset} sum={s2:04x}{s1:04x}");
}

/// Level 3 `potential match at %s i=%ld sum=%08x` - a weak-checksum candidate.
///
/// upstream: match.c:258-262 `hash_search()`. Upstream prints every chain
/// candidate whose weak sum and length match, BEFORE the strong-checksum
/// verify; oc's index probe confirms the strong checksum internally, so this
/// fires only for confirmed candidates and `i` is the confirmed block index
/// rather than an arbitrary chain entry.
#[inline]
pub fn trace_potential_match(offset: u64, i: u64, sum: u32) {
    debug_log!(
        Deltasum,
        3,
        "potential match at {offset} i={i} sum={sum:08x}"
    );
}

/// Level 2 `match at %s last_match=%s j=%d len=%ld n=%ld` - an emitted block
/// match: `j` is the basis block index, `len` its length, `n` the literal
/// bytes accumulated since the previous token.
///
/// upstream: match.c:133-138 `matched()` (printed only for `i >= 0`).
#[inline]
pub fn trace_match(offset: u64, last_match: u64, j: u64, len: usize, n: u64) {
    debug_log!(
        Deltasum,
        2,
        "match at {offset} last_match={last_match} j={j} len={len} n={n}"
    );
}

/// Level 2 `built hash table` - the signature hash index is ready.
///
/// upstream: match.c:436-437 `match_sums()`.
#[inline]
pub fn trace_built_hash_table() {
    debug_log!(Deltasum, 2, "built hash table");
}

/// Level 2 `done hash search` - the scan over the source completed.
///
/// upstream: match.c:441-442 `match_sums()`.
#[inline]
pub fn trace_done_hash_search() {
    debug_log!(Deltasum, 2, "done hash search");
}

/// Level 2 `sending file_sum` - the sender is about to write the whole-file
/// checksum trailer.
///
/// upstream: match.c:465-466 `match_sums()`.
#[inline]
pub fn trace_sending_file_sum() {
    debug_log!(Deltasum, 2, "sending file_sum");
}

/// Level 2 `false_alarms=%d hash_hits=%d matches=%d` - per-file scan counters,
/// printed after the file's checksum trailer.
///
/// upstream: match.c:468-471 `match_sums()`. Counter semantics are oc-native
/// (see [`crate::ProbeCounters`]): `hash_hits` counts bithash-prefilter
/// positives and `false_alarms` counts prefilter positives that produced no
/// confirmed match, where upstream counts bucket hits and strong-sum rejects.
#[inline]
pub fn trace_match_counters(false_alarms: u64, hash_hits: u64, matches: u64) {
    debug_log!(
        Deltasum,
        2,
        "false_alarms={false_alarms} hash_hits={hash_hits} matches={matches}"
    );
}

/// Level 1 `total: matches=%d  hash_hits=%d  false_alarms=%d data=%s` - the
/// once-per-run sender totals (note the double spaces).
///
/// upstream: match.c:479-487 `match_report()`, called after `send_files()`
/// finishes (sender.c:815). The LOCAL copy path renders this line directly
/// from the client summary (`cli::frontend::progress::render`) to keep
/// upstream's position between the name list and the summary trailer; this
/// function is the owner for the network sender drivers only.
#[inline]
pub fn trace_match_totals(matches: u64, hash_hits: u64, false_alarms: u64, data: u64) {
    debug_log!(
        Deltasum,
        1,
        "total: matches={matches}  hash_hits={hash_hits}  false_alarms={false_alarms} data={data}"
    );
}

/// Level 3 `count=%s n=%ld rem=%ld` - the sender's view of the received sum
/// header.
///
/// upstream: sender.c:348-350 `receive_sums()`.
#[inline]
pub fn trace_receive_sums_head(count: u64, block_length: usize, remainder: u32) {
    debug_log!(
        Deltasum,
        3,
        "count={count} n={block_length} rem={remainder}"
    );
}

/// Level 3 `chunk[%d] len=%d offset=%s sum1=%08x` - one received signature
/// block on the sender.
///
/// upstream: sender.c:382-386 `receive_sums()`.
#[inline]
pub fn trace_receive_sums_chunk(i: u64, len: usize, offset: u64, sum1: u32) {
    debug_log!(
        Deltasum,
        3,
        "chunk[{i}] len={len} offset={offset} sum1={sum1:08x}"
    );
}

/// Level 2 `send_files mapped %s%s%s of size %s` - the sender opened its
/// source for scanning.
///
/// upstream: sender.c:760-763 `send_files()` (the `%s%s%s` is
/// path/slash/fname; oc passes the joined path).
#[inline]
pub fn trace_send_files_mapped(path: &dyn std::fmt::Display, size: u64) {
    debug_log!(Deltasum, 2, "send_files mapped {path} of size {size}");
}

/// Level 2 `calling match_sums %s%s%s` - the sender is entering the delta
/// scan for this file.
///
/// upstream: sender.c:768-769 `send_files()`.
#[inline]
pub fn trace_calling_match_sums(path: &dyn std::fmt::Display) {
    debug_log!(Deltasum, 2, "calling match_sums {path}");
}

/// Level 3 `gen mapped %s of size %s` - the generator opened the basis it
/// will checksum.
///
/// upstream: generator.c:2358-2361 `recv_generator()`.
#[inline]
pub fn trace_gen_mapped(path: &dyn std::fmt::Display, size: u64) {
    debug_log!(Deltasum, 3, "gen mapped {path} of size {size}");
}

/// Level 2 `generating and sending sums for %d` - the generator is producing
/// the signature for file index `ndx`.
///
/// upstream: generator.c:2363-2364 `recv_generator()`.
#[inline]
pub fn trace_generating_sums(ndx: i32) {
    debug_log!(Deltasum, 2, "generating and sending sums for {ndx}");
}

/// Level 2 `count=%s rem=%ld blength=%ld s2length=%d flength=%s` - the
/// signature geometry chosen for a basis.
///
/// upstream: generator.c:765-770 `sum_sizes_sqroot()`.
#[inline]
pub fn trace_sum_geometry(
    count: u64,
    remainder: u32,
    block_length: usize,
    s2length: u8,
    flength: u64,
) {
    debug_log!(
        Deltasum,
        2,
        "count={count} rem={remainder} blength={block_length} s2length={s2length} flength={flength}"
    );
}

/// Level 3 `chunk[%s] offset=%s len=%ld sum1=%08lx` - one generated signature
/// block on the generator.
///
/// upstream: generator.c:817-822 `generate_and_send_sums()`.
#[inline]
pub fn trace_gen_chunk(i: u64, offset: u64, len: usize, sum1: u32) {
    debug_log!(
        Deltasum,
        3,
        "chunk[{i}] offset={offset} len={len} sum1={sum1:08x}"
    );
}

/// Level 2 `recv mapped %s of size %s` - the receiver opened the basis it
/// will copy matched blocks from.
///
/// upstream: receiver.c:498-501 `receive_data()`.
#[inline]
pub fn trace_recv_mapped(path: &dyn std::fmt::Display, size: u64) {
    debug_log!(Deltasum, 2, "recv mapped {path} of size {size}");
}

/// Level 3 `data recv %d at %s` - a literal run arrived at output offset
/// `offset`.
///
/// upstream: receiver.c:552-555 `receive_data()`.
#[inline]
pub fn trace_data_recv(len: usize, offset: u64) {
    debug_log!(Deltasum, 3, "data recv {len} at {offset}");
}

/// Level 3 `chunk[%d] of size %ld at %s offset=%s%s` - a matched block is
/// applied from basis offset `offset2` at output offset `offset`; `seek` adds
/// upstream's ` (seek)` marker for the in-place skip fast path.
///
/// upstream: receiver.c:609-614 `receive_data()`.
#[inline]
pub fn trace_recv_chunk(i: u64, len: usize, offset2: u64, offset: u64, seek: bool) {
    debug_log!(
        Deltasum,
        3,
        "chunk[{i}] of size {len} at {offset2} offset={offset}{}",
        if seek { " (seek)" } else { "" }
    );
}

/// Level 2 `got file_sum` - the receiver consumed the sender's whole-file
/// checksum trailer.
///
/// upstream: receiver.c:671-673 `receive_data()`.
#[inline]
pub fn trace_got_file_sum() {
    debug_log!(Deltasum, 2, "got file_sum");
}

#[cfg(test)]
mod tests {
    use super::*;
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    fn setup(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.deltasum = level;
        init(cfg);
        let _ = drain_events();
    }

    fn messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Deltasum,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    /// Pins every format string byte-for-byte against upstream 3.5.0 output
    /// captured from `--debug=deltasum3` runs (match.c / sender.c /
    /// generator.c / receiver.c, see per-fn citations).
    #[test]
    fn formats_match_upstream() {
        setup(4);
        trace_hash_search_start(700, 262144);
        trace_hash_search_params(700, 262144, 375);
        trace_initial_sum(0x760c_feda, 700);
        trace_scan_offset(3, 0xf16c, 0xfc9a);
        trace_potential_match(0, 320, 0x760c_feda);
        trace_match(700, 700, 1, 700, 0);
        trace_built_hash_table();
        trace_done_hash_search();
        trace_sending_file_sum();
        trace_match_counters(0, 377, 374);
        trace_match_totals(374, 377, 0, 700);
        trace_receive_sums_head(375, 700, 344);
        trace_receive_sums_chunk(0, 700, 0, 0x760c_feda);
        trace_send_files_mapped(&"/src/a.bin", 262144);
        trace_calling_match_sums(&"/src/a.bin");
        trace_gen_mapped(&"a.bin", 262144);
        trace_generating_sums(0);
        trace_sum_geometry(375, 344, 700, 2, 262144);
        trace_gen_chunk(1, 700, 700, 0xf164_ff4a);
        trace_recv_mapped(&"a.bin", 262144);
        trace_data_recv(700, 99400);
        trace_recv_chunk(374, 344, 261800, 261800, false);
        trace_recv_chunk(5, 700, 3500, 3500, true);
        trace_got_file_sum();

        let msgs = messages();
        let expected = [
            "hash search b=700 len=262144",
            "hash search s->blength=700 len=262144 count=375",
            "sum=760cfeda k=700",
            "offset=3 sum=f16cfc9a",
            "potential match at 0 i=320 sum=760cfeda",
            "match at 700 last_match=700 j=1 len=700 n=0",
            "built hash table",
            "done hash search",
            "sending file_sum",
            "false_alarms=0 hash_hits=377 matches=374",
            "total: matches=374  hash_hits=377  false_alarms=0 data=700",
            "count=375 n=700 rem=344",
            "chunk[0] len=700 offset=0 sum1=760cfeda",
            "send_files mapped /src/a.bin of size 262144",
            "calling match_sums /src/a.bin",
            "gen mapped a.bin of size 262144",
            "generating and sending sums for 0",
            "count=375 rem=344 blength=700 s2length=2 flength=262144",
            "chunk[1] offset=700 len=700 sum1=f164ff4a",
            "recv mapped a.bin of size 262144",
            "data recv 700 at 99400",
            "chunk[374] of size 344 at 261800 offset=261800",
            "chunk[5] of size 700 at 3500 offset=3500 (seek)",
            "got file_sum",
        ];
        assert_eq!(msgs, expected, "DELTASUM formats drifted from upstream");
    }

    /// Every emission must respect its upstream level gate: nothing below its
    /// own level, everything at it.
    #[test]
    fn level_gates_match_upstream() {
        setup(1);
        trace_hash_search_start(1, 1);
        trace_built_hash_table();
        trace_got_file_sum();
        trace_data_recv(1, 0);
        assert!(messages().is_empty(), "level-2/3 lines leaked at level 1");

        setup(1);
        trace_match_totals(0, 0, 0, 0);
        assert_eq!(messages().len(), 1, "the total line is level 1");

        setup(2);
        trace_data_recv(1, 0);
        trace_potential_match(0, 0, 0);
        trace_initial_sum(0, 0);
        assert!(messages().is_empty(), "level-3 lines leaked at level 2");

        setup(3);
        trace_scan_offset(0, 0, 0);
        assert!(
            messages().is_empty(),
            "the level-4 offset line leaked at level 3"
        );
        setup(4);
        trace_scan_offset(0, 0, 0);
        assert_eq!(messages().len(), 1);
    }
}
