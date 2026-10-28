//! Oracle: the `-vv` `total:` line of a delta transfer equals rsync 3.5.1's.
//!
//! upstream: match.c:480-489 `match_report()`. `matches` and `data` follow
//! from the delta itself, but `hash_hits` and `false_alarms` are defined by
//! upstream's own `hash_table` (match.c:84-110, 214-271), which oc's matcher
//! does not use; oc replays that table to report them. Each fixture runs as a
//! local copy, a push and a pull, so the local engine and both wire sender
//! roles are held to the same numbers.
//!
//! # Gating
//!
//! Self-skips unless `OC_RSYNC_UPSTREAM_COMPAT=1` and an upstream 3.5.1
//! binary resolves (override with `OC_RSYNC_UPSTREAM_BIN_3_5_1`).
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use test_support::{
    UpstreamVersion, oc_rsync_bin, require_upstream_rsync, upstream_compat_enabled,
};

/// How the transfer reaches the destination.
#[derive(Clone, Copy, Debug)]
enum Mode {
    Local,
    Push,
    Pull,
}

/// Writes `basis` as the destination's old copy and `source` as the new one,
/// backdating the basis so the quick check cannot skip it.
fn write_fixture(root: &Path, source: &[u8], basis: &[u8]) {
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::create_dir_all(root.join("dst")).expect("create dst");
    fs::write(root.join("src/file.bin"), source).expect("write source");
    fs::write(root.join("dst/file.bin"), basis).expect("write basis");
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800);
    fs::File::open(root.join("dst/file.bin"))
        .and_then(|file| file.set_modified(stamp))
        .expect("backdate basis");
    // A remote shell that runs its command locally, dropping the host name.
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
}

/// One delta transfer to compare: the files and any options it needs.
struct Fixture {
    name: &'static str,
    source: Vec<u8>,
    basis: Vec<u8>,
    args: &'static [&'static str],
    modes: &'static [Mode],
}

const ALL_MODES: &[Mode] = &[Mode::Local, Mode::Push, Mode::Pull];

/// Runs one delta transfer and returns its `total:` line.
fn total_line(binary: &Path, fixture: &Fixture, mode: Mode) -> String {
    let (source, basis) = (&fixture.source, &fixture.basis);
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    write_fixture(root, source, basis);
    let src = format!("{}/", root.join("src").display());
    let dst = format!("{}/", root.join("dst").display());
    let (from, to) = match mode {
        Mode::Local => (src, dst),
        Mode::Push => (src, format!("localhost:{dst}")),
        Mode::Pull => (format!("localhost:{src}"), dst),
    };
    let output = Command::new(binary)
        .arg("-rvv")
        .arg("--no-whole-file")
        .args(fixture.args)
        .arg("-e")
        .arg(root.join("rsh"))
        .arg(format!("--rsync-path={}", binary.display()))
        .arg(&from)
        .arg(&to)
        .output()
        .expect("spawn rsync");
    assert!(
        output.status.success(),
        "{} {mode:?} failed: {}",
        binary.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|line| line.starts_with("total: matches="))
        .unwrap_or_else(|| panic!("{} {mode:?} printed no total: line", binary.display()))
        .to_owned()
}

/// A line-numbered text with one line changed: matches on both sides of a
/// literal run, and a short final block.
fn edited_text() -> Fixture {
    let basis: String = (1..=200_000).map(|n| format!("{n}\n")).collect();
    let source = basis.replacen("\n100000\n", "\nCHANGED\n", 1);
    Fixture {
        name: "edited text",
        source: source.into_bytes(),
        basis: basis.into_bytes(),
        args: &[],
        modes: ALL_MODES,
    }
}

/// A basis of identical blocks against a source whose windows collide with
/// them on the weak sum but not the strong one: raising bytes 0 and 3 and
/// lowering 1 and 2 by one keeps both rolling-sum halves. Every probe then
/// walks a long chain of false alarms.
fn weak_sum_collisions() -> Fixture {
    let basis = vec![b'a'; 300_000];
    let mut source = basis.clone();
    for chunk in source.chunks_mut(1_000) {
        chunk[0] += 1;
        chunk[3] += 1;
        chunk[1] -= 1;
        chunk[2] -= 1;
    }
    Fixture {
        name: "weak-sum collisions",
        source,
        basis,
        args: &[],
        modes: ALL_MODES,
    }
}

/// Five 700-byte blocks, two of which the source replaces with windows that
/// share both the weak sum and the first two bytes of the seeded MD5 strong
/// sum. Under `--checksum-seed=1` the first pass accepts those false matches,
/// the whole-file checksum fails, and the redo pass with full-length strong
/// sums counts them as false alarms (receiver.c redo, match.c:268-271).
///
/// The collisions are MD5 collisions, so MD5 is pinned: a negotiated XXH128
/// has none, and the cell would then compare two transfers without a redo.
/// A local copy is left out until it runs the phase-2 redo (task 469).
fn redo_after_strong_collisions() -> Fixture {
    Fixture {
        name: "redo after strong-sum collisions",
        source: include_bytes!("fixtures/delta_total/redo_collision_source.bin").to_vec(),
        basis: include_bytes!("fixtures/delta_total/redo_collision_basis.bin").to_vec(),
        args: &["--checksum-seed=1", "--checksum-choice=md5"],
        modes: &[Mode::Push, Mode::Pull],
    }
}

#[test]
fn delta_total_line_matches_upstream() {
    if !upstream_compat_enabled() {
        return;
    }
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let oc = oc_rsync_bin();
    for fixture in [
        edited_text(),
        weak_sum_collisions(),
        redo_after_strong_collisions(),
    ] {
        for &mode in fixture.modes {
            assert_eq!(
                total_line(&oc, &fixture, mode),
                total_line(upstream.binary(), &fixture, mode),
                "{}, {mode:?}: the total: line differs from upstream",
                fixture.name
            );
        }
    }
}
