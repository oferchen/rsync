//! Live-binary non-vacuity guard for `--debug=deltasum`.
//!
//! `--debug=deltasum` was parsed and accepted for a long time while emitting
//! NOTHING from the delta scan. A unit test on the trace helpers cannot catch
//! that: the helpers were fine, they simply had no caller on the path the binary
//! actually takes. The previous attempt at this fix wired the flag at the
//! obvious site and was PROVABLY INERT, because the local copy path does not
//! traverse the network delta scanner.
//!
//! So these tests run the real binary over a real delta fixture and assert on
//! its stdout. They are the only check here that can distinguish "the emission
//! is wired" from "the format string exists".
//!
//! They live in the ROOT package, not in `crates/cli`, and resolve the binary
//! through `CARGO_BIN_EXE_oc-rsync`. That is load-bearing: `oc-rsync` is defined
//! by this package, so cargo rebuilds it before these tests run and the env var
//! names that fresh artifact. MEASURED - an earlier revision of this file lived
//! in `crates/cli/tests/` and located the binary by walking up from
//! `current_exe()`. `cargo nextest run -p cli` does not build the root package,
//! so those tests silently graded whatever `target/debug/oc-rsync` happened to
//! be on disk: a mutation that made the level gate always-true left all seven
//! of them GREEN while the real binary printed 2588 lines at level 1.
//!
//! Upstream reference for every asserted line: `match.c` (hash search and the
//! run totals), `sender.c` (`receive_sums`/`send_files` milestones),
//! `generator.c` (signature geometry and per-chunk sums), `receiver.c` (basis
//! map, literal/match application, file_sum receipt).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Block length the 256 KB fixture's signature layout selects, and the count of
/// blocks it yields. Pinned so the asserted lines carry real geometry.
const FIXTURE_LEN: usize = 262_144;

/// The `oc-rsync` cargo just built for this test run.
///
/// `CARGO_BIN_EXE_oc-rsync` is a COMPILE-time variable, so `env!` (not
/// `std::env::var`) is what binds it, and cargo guarantees the named artifact is
/// up to date before the test starts.
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Deterministic pseudo-random bytes (xorshift64), so the delta has real
/// literal runs instead of the long self-similar matches a constant fill gives.
fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// Builds `src/a.bin` and a mutated, backdated `dst/a.bin` so the quick-check
/// cannot skip the file and the delta scan has both matches and a literal run.
fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    let src_dir = root.join("src");
    let dst_dir = root.join("dst");
    fs::create_dir_all(&src_dir).expect("src dir");
    fs::create_dir_all(&dst_dir).expect("dst dir");

    let data = pseudo_random(FIXTURE_LEN, 0x5eed_1234);
    let src = src_dir.join("a.bin");
    fs::write(&src, &data).expect("write src");

    let mut mutated = data;
    mutated[100_000..100_100].fill(b'X');
    let dst = dst_dir.join("a.bin");
    fs::write(&dst, &mutated).expect("write dst");

    // Backdate the basis: rsync's quick-check skips a same-size, same-mtime
    // file, which would make every assertion here vacuous.
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800);
    filetime::set_file_mtime(&dst, filetime::FileTime::from_system_time(old))
        .expect("backdate basis");

    (src, dst)
}

/// Runs the binary with `--debug=<word>` on the local delta fixture.
fn run_local(root: &Path, word: &str) -> String {
    let (src, dst) = fixture(root);
    let out = Command::new(binary())
        .arg("--no-whole-file")
        .arg(format!("--debug={word}"))
        .arg("-t")
        .arg(&src)
        .arg(&dst)
        .output()
        .expect("run oc-rsync");
    assert!(
        out.status.success(),
        "transfer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A stand-in `ssh` that drops the host argument and execs the rest locally, so
/// a wire transfer runs over a real pipe pair without needing a daemon or sshd.
fn fake_rsh(root: &Path) -> PathBuf {
    let path = root.join("fake-rsh.sh");
    let mut f = fs::File::create(&path).expect("create rsh");
    f.write_all(b"#!/bin/sh\nshift\nexec \"$@\"\n")
        .expect("write rsh");
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    }
    path
}

/// Runs a wire transfer in the given direction over the fake rsh.
#[cfg(unix)]
fn run_wire(root: &Path, word: &str, push: bool) -> String {
    let (src, dst) = fixture(root);
    let rsh = fake_rsh(root);
    let bin = binary();
    let mut cmd = Command::new(&bin);
    cmd.arg("--no-whole-file")
        .arg(format!("--debug={word}"))
        .arg("-t")
        .arg("-e")
        .arg(&rsh)
        .arg(format!("--rsync-path={}", bin.display()));
    if push {
        cmd.arg(&src).arg(format!("fake:{}", dst.display()));
    } else {
        cmd.arg(format!("fake:{}", src.display())).arg(&dst);
    }
    let out = cmd.output().expect("run oc-rsync over fake rsh");
    assert!(
        out.status.success(),
        "transfer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn assert_contains(haystack: &str, needle: &str, cell: &str) {
    assert!(
        haystack.contains(needle),
        "{cell}: expected a line containing {needle:?}, got:\n{haystack}"
    );
}

/// The whole point of the task: the flag must produce delta-scan output from the
/// LOCAL path, which is a fused loop that the network delta scanner never runs.
///
/// upstream: match.c:180-182 `hash search`, :133-138 `match at`, :441-442
/// `done hash search`, :468-471 the counters.
#[test]
fn local_delta_scan_emits_deltasum2() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run_local(tmp.path(), "deltasum2");

    assert_contains(&out, "built hash table", "local L2");
    assert_contains(&out, "calling match_sums ", "local L2");
    assert_contains(&out, "send_files mapped ", "local L2");
    assert_contains(
        &out,
        &format!("hash search b=700 len={FIXTURE_LEN}"),
        "local L2",
    );
    assert_contains(&out, "match at 0 last_match=0 j=0 len=700 n=0", "local L2");
    assert_contains(&out, "done hash search", "local L2");
    assert_contains(&out, "false_alarms=", "local L2");
    assert_contains(&out, "total: matches=", "local L2");
}

/// Level 3 adds the generator's per-chunk sums and the receiver's per-block
/// application, both of which the local fused loop really performs.
///
/// upstream: generator.c:817-822 `chunk[%s] offset=...`, receiver.c:609-614
/// `chunk[%d] of size %ld at %s offset=%s`, :552-555 `data recv %d at %s`.
#[test]
fn local_delta_scan_emits_deltasum3_chunk_detail() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run_local(tmp.path(), "deltasum3");

    assert_contains(&out, "gen mapped ", "local L3");
    assert_contains(&out, "chunk[0] offset=0 len=700 sum1=", "local L3");
    assert_contains(&out, "chunk[0] of size 700 at 0 offset=0", "local L3");
    assert_contains(&out, "data recv ", "local L3");
    assert_contains(&out, "potential match at 0 i=0 sum=", "local L3");
}

/// Level 1 is the run-total line only. It must appear, and the per-offset and
/// per-chunk detail must NOT: a level gate that is wired but always-true is as
/// wrong as one that never fires.
///
/// upstream: match.c:479-487 `match_report()` prints only `total:` at
/// DEBUG_GTE(DELTASUM, 1).
#[test]
fn local_deltasum1_is_the_total_line_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run_local(tmp.path(), "deltasum");

    assert_contains(&out, "total: matches=", "local L1");
    // Checked per line, not as a substring: the `total:` line legitimately
    // carries a `false_alarms=` field of its own, so a naive `contains` would
    // pass for the wrong reason.
    for line in out.lines() {
        for forbidden in [
            "hash search b=",
            "done hash search",
            "false_alarms=",
            "match at ",
            "chunk[",
        ] {
            assert!(
                !line.starts_with(forbidden),
                "local L1: {forbidden:?} is level >= 2 and must not appear at level 1, \
                 but the line {line:?} does. Full output:\n{out}"
            );
        }
    }
}

/// The network SENDER path is a different scanner from the local one, so it
/// needs its own live assertion.
///
/// upstream: sender.c:348-350 `count=/n=/rem=`, :760-763 `send_files mapped`,
/// :768-769 `calling match_sums`, match.c:465-466 `sending file_sum`.
#[cfg(unix)]
#[test]
fn wire_push_sender_emits_deltasum2() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run_wire(tmp.path(), "deltasum2", true);

    assert_contains(&out, "send_files mapped ", "push L2");
    assert_contains(&out, "calling match_sums ", "push L2");
    assert_contains(&out, "built hash table", "push L2");
    assert_contains(
        &out,
        &format!("hash search b=700 len={FIXTURE_LEN}"),
        "push L2",
    );
    assert_contains(&out, "match at 0 last_match=0 j=0 len=700 n=0", "push L2");
    assert_contains(&out, "done hash search", "push L2");
    assert_contains(&out, "sending file_sum", "push L2");
    assert_contains(&out, "total: matches=", "push L2");

    // Ordering is load-bearing: upstream prints the per-file counters AFTER the
    // checksum trailer announcement, which is why the scan hands its counters
    // back rather than printing them itself (match.c:465-471).
    let sending = out.find("sending file_sum").expect("sending file_sum");
    let counters = out.find("false_alarms=").expect("counter line");
    assert!(
        sending < counters,
        "push L2: `sending file_sum` must precede the counter line, got:\n{out}"
    );
}

/// The client PULL path runs the generator and the receiver, a third distinct
/// set of emission sites.
///
/// upstream: generator.c:2358-2361 `gen mapped`, :2363-2364 `generating and
/// sending sums`, :765-770 the geometry, receiver.c:498-501 `recv mapped`,
/// :671-673 `got file_sum`.
#[cfg(unix)]
#[test]
fn wire_pull_generator_and_receiver_emit_deltasum2() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run_wire(tmp.path(), "deltasum2", false);

    assert_contains(&out, "generating and sending sums for 0", "pull L2");
    assert_contains(&out, "recv mapped a.bin of size ", "pull L2");
    assert_contains(&out, "got file_sum", "pull L2");
    assert_contains(
        &out,
        &format!("count=375 rem=344 blength=700 s2length=2 flength={FIXTURE_LEN}"),
        "pull L2",
    );

    // upstream prints `generating and sending sums` BEFORE the geometry line
    // that sum_sizes_sqroot() produces (generator.c:2363 then :765).
    let announce = out.find("generating and sending sums").expect("announce");
    let geometry = out.find("count=375 rem=").expect("geometry");
    assert!(
        announce < geometry,
        "pull L2: the announce must precede the geometry line, got:\n{out}"
    );
}

/// Level 3 on the pull adds the generator's per-chunk sums and the receiver's
/// per-block application.
#[cfg(unix)]
#[test]
fn wire_pull_emits_deltasum3_chunk_detail() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run_wire(tmp.path(), "deltasum3", false);

    assert_contains(&out, "gen mapped a.bin of size ", "pull L3");
    assert_contains(&out, "chunk[0] offset=0 len=700 sum1=", "pull L3");
    assert_contains(&out, "chunk[0] of size 700 at 0 offset=0", "pull L3");
}

/// `--debug=deltasum` must stay inert when it is not asked for: a producer that
/// ignores its gate would flood every ordinary `-t` transfer.
#[test]
fn no_deltasum_output_without_the_flag() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (src, dst) = fixture(tmp.path());
    let out = Command::new(binary())
        .arg("--no-whole-file")
        .arg("-t")
        .arg(&src)
        .arg(&dst)
        .output()
        .expect("run oc-rsync");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for forbidden in ["hash search", "match at ", "total: matches=", "chunk["] {
        assert!(
            !stdout.contains(forbidden),
            "{forbidden:?} leaked without --debug=deltasum, got:\n{stdout}"
        );
    }
}
