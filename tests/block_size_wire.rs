//! `-B` / `--block-size` must reach the generator on EVERY transport.
//!
//! The generator - the receiving side - is what sizes the blocks it checksums:
//!
//! ```c
//! /* generator.c:720-721, inside sum_sizes_sqroot() */
//! if (block_size)
//!     blength = block_size;
//! else if (len <= BLOCK_SIZE * BLOCK_SIZE)
//!     blength = BLOCK_SIZE;            /* rsync.h:25 - 700 */
//! ```
//!
//! and the client re-emits the option to the server so the remote generator
//! reaches the same `blength`:
//!
//! ```c
//! /* options.c:2953-2954 */
//! if (block_size) {
//!     if (asprintf(&arg, "-B%u", (int)block_size) < 0)
//! ```
//!
//! WHY this is a class test rather than one regression case: oc has THREE
//! places that decide the receiving side's block length - the local-copy
//! executor, the `--server` argv parser, and the daemon's own client-args
//! parser - and until this test existed only the local one honoured `-B`. A
//! per-path test would have passed on whichever path its author happened to
//! pick. Asserting the same option across every transport in one loop is what
//! makes a newly added or newly forgotten path fail.
//!
//! The assertions are not free-floating expectations; both are arithmetic
//! consequences of the block length, and they were confirmed against rsync
//! 3.5.0 (protocol 32) on this exact fixture:
//!
//! ```text
//! $ rsync -I --no-whole-file --stats -B 2048 src/f.bin host:dest/f.bin
//! Literal data: 2,048 bytes    Matched data: 2,048 bytes
//! $ rsync -I --no-whole-file --stats     src/f.bin host:dest/f.bin
//! Literal data: 700 bytes      Matched data: 3,396 bytes
//! ```
//!
//! - 4096 bytes at `-B 2048` is two blocks; only the first differs, so
//!   literal == matched == 2048.
//! - 4096 bytes at the 700-byte default is five full blocks plus a 596-byte
//!   remainder; only the first differs, so literal == 700 and the rest
//!   (3396 bytes, INCLUDING the short final block) matches.
//!
//! The default case therefore does double duty: it is the non-vacuity control
//! (a run that ignored `-B` would print these numbers for BOTH cases, which the
//! `assert_ne!` below rejects) and it pins the short-final-block match.
//!
//! Skip conditions (test passes with a printed reason):
//! - Not Unix (the remote-shell shim uses `/bin/sh`).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 4096 = 2 x 2048 = 5 x 700 + 596, so the fixture exercises both an exact
/// division and a short final block without changing size between cases.
const FILE_LEN: usize = 4096;

/// Bytes 0..DIFF_LEN are rewritten in the source; everything after is shared
/// with the basis, so exactly one block of any tested size is dirty.
const DIFF_LEN: usize = 50;

/// Deterministic, NON-PERIODIC filler.
///
/// An arithmetic sequence (`i * k + c`) repeats with a short period, which
/// creates duplicate blocks; with duplicates present the rolling-hash probe
/// order decides which basis block a window resolves to, and nothing in the
/// protocol makes that choice canonical. A comparison built on such data can
/// diverge between two correct runs. A seeded xorshift keeps the fixture
/// reproducible without that hazard.
fn filler(seed: u64, len: usize) -> Vec<u8> {
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

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// A remote-shell stand-in: drops the leading options and the host token, then
/// execs the server command, so the "remote" is this same binary running for
/// real as `--server`.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    fs::write(
        &script,
        "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
         case \"$1\" in\n\
         -*) shift ;;\n\
         *) break ;;\n\
         esac\n\
         done\n\
         shift || true\n\
         exec \"$@\"\n",
    )
    .expect("write rsh shim");
    let mut perms = fs::metadata(&script).expect("stat shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("chmod shim");
    script
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transport {
    Local,
    Push,
    Pull,
}

impl Transport {
    const ALL: [Self; 3] = [Self::Local, Self::Push, Self::Pull];
}

/// The `--stats` literal/matched split for one transfer.
#[derive(Debug, PartialEq, Eq)]
struct Split {
    literal: u64,
    matched: u64,
}

fn parse_split(output: &str) -> Split {
    let field = |label: &str| -> u64 {
        output
            .lines()
            .find_map(|line| line.strip_prefix(label))
            .and_then(|rest| rest.split_whitespace().next())
            .map(|n| n.replace(',', ""))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no `{label}` line in --stats output:\n{output}"))
    };
    Split {
        literal: field("Literal data: "),
        matched: field("Matched data: "),
    }
}

/// Runs one delta transfer and returns its literal/matched split.
///
/// `-I` defeats the quick-check (source and basis share a size, and without it
/// a same-second mtime would skip the transfer outright and report 0/0);
/// `--no-whole-file` forces the delta algorithm on the local path, which
/// otherwise copies whole files.
fn run(transport: Transport, block_size: Option<u32>) -> Split {
    let tmp = tempfile::tempdir().expect("tempdir");
    let shim = write_rsh_shim(tmp.path());
    let binary = oc_binary();

    let src_dir = tmp.path().join("src");
    let dst_dir = tmp.path().join("dst");
    fs::create_dir_all(&src_dir).expect("mkdir src");
    fs::create_dir_all(&dst_dir).expect("mkdir dst");

    let basis = filler(0x5eed_1234, FILE_LEN);
    let mut source = basis.clone();
    source[..DIFF_LEN].copy_from_slice(&filler(0xd1ff_9876, DIFF_LEN));

    let src = src_dir.join("f.bin");
    let dst = dst_dir.join("f.bin");
    fs::write(&src, &source).expect("write source");
    fs::write(&dst, &basis).expect("write basis");

    let mut cmd = Command::new(&binary);
    cmd.arg("-I").arg("--no-whole-file").arg("--stats");
    if let Some(size) = block_size {
        cmd.arg("-B").arg(size.to_string());
    }
    match transport {
        Transport::Local => {
            cmd.arg(&src).arg(&dst);
        }
        Transport::Push => {
            cmd.arg("--rsh")
                .arg(&shim)
                .arg("--rsync-path")
                .arg(&binary)
                .arg(&src)
                .arg(format!("bshost:{}", dst.display()));
        }
        Transport::Pull => {
            cmd.arg("--rsh")
                .arg(&shim)
                .arg("--rsync-path")
                .arg(&binary)
                .arg(format!("bshost:{}", src.display()))
                .arg(&dst);
        }
    }

    let out = cmd.output().expect("run oc-rsync");
    assert!(
        out.status.success(),
        "{transport:?} with -B {block_size:?} failed: {}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(
        fs::read(&dst).expect("read dest"),
        source,
        "{transport:?} must still reconstruct the file byte-for-byte",
    );

    let split = parse_split(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(
        split.literal + split.matched,
        FILE_LEN as u64,
        "{transport:?}: literal + matched must account for the whole file",
    );
    split
}

/// upstream: generator.c:720-721 - `-B` displaces the square-root heuristic on
/// whichever side runs the generator, so every transport must land on the same
/// block length. Before this test, the wire paths silently used the 700-byte
/// default and produced the `no -B` numbers even when `-B 2048` was given.
#[test]
fn block_size_is_honoured_on_every_transport() {
    let with_b: Vec<_> = Transport::ALL.map(|t| (t, run(t, Some(2048)))).into();
    for (transport, split) in &with_b {
        assert_eq!(
            (split.literal, split.matched),
            (2048, 2048),
            "{transport:?}: 4096 bytes at -B 2048 is two blocks, one of them dirty",
        );
    }
}

/// The non-vacuity control for [`block_size_is_honoured_on_every_transport`]:
/// with no `-B` the heuristic picks upstream's 700-byte `BLOCK_SIZE`
/// (rsync.h:25), which yields a DIFFERENT split. A path that ignores `-B`
/// prints these numbers in both tests, so the `assert_ne!` is what turns the
/// pair into a discriminating oracle.
#[test]
fn default_block_size_differs_and_matches_the_short_final_block() {
    for transport in Transport::ALL {
        let split = run(transport, None);
        assert_eq!(
            (split.literal, split.matched),
            (700, 3396),
            "{transport:?}: 5 x 700 + a 596-byte tail, first block dirty, tail matched",
        );
        assert_ne!(
            (split.literal, split.matched),
            (2048, 2048),
            "{transport:?}: the default must not coincide with -B 2048",
        );
    }
}

/// A block size larger than the file is one block, and that block is dirty, so
/// the delta degenerates to a whole-file send. This pins the upper edge of the
/// range: a path that clamped or ignored a large `-B` would still find matches.
#[test]
fn block_size_at_least_the_file_length_matches_nothing() {
    for transport in Transport::ALL {
        let split = run(transport, Some(FILE_LEN as u32));
        assert_eq!(
            (split.literal, split.matched),
            (FILE_LEN as u64, 0),
            "{transport:?}: one whole-file block, and it differs",
        );
    }
}
