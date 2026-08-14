//! `--block-size` must accept upstream's whole size grammar, not bare integers.
//!
//! Upstream parses it with the same helper as `--min-size`, `--max-size` and
//! `--bwlimit`:
//!
//! ```c
//! /* options.c:1802 */
//! if ((size = parse_size_arg(arg, 'b', "block-size", 0, max_blength, False)) < 0)
//! ```
//!
//! and `parse_size_arg` (options.c:1163-1265) accepts a decimal number with an
//! optional fraction, an optional `b/k/m/g/t/p` scale, an optional `B` (x1000)
//! or `iB` (x1024) qualifier, and a trailing `+1` / `-1` adjustment.
//!
//! oc reached that parser for the three siblings but not for `--block-size`: a
//! bare-`u64` gate in the argv layer rejected every suffixed spelling before
//! the shared parser ran, so `size.rs`'s tested suffix support was unreachable
//! in production for this one option.
//!
//! Skip conditions (test passes with a printed reason):
//! - The cross-implementation cell needs a built upstream 3.5.0 binary; without
//!   it, it reports why it did not run.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn upstream_binary() -> Option<PathBuf> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/interop/upstream-src/rsync-3.5.0/rsync");
    path.is_file().then_some(path)
}

/// Every spelling measured against the real 3.5.0 binary, with whether it is
/// accepted. `1M` and above exceed `MAX_BLOCK_SIZE` (131072) at protocol >= 30,
/// so they are rejected for a reason unrelated to the grammar - which is what
/// makes them useful: a parser that ignored the scale entirely would accept
/// them.
const GRAMMAR: &[(&str, bool)] = &[
    ("700", true),
    ("1K", true),
    ("1k", true),
    ("1KB", true),
    ("1kb", true),
    ("1KiB", true),
    ("1kib", true),
    ("128K", true),
    ("1.5K", true),
    ("0", true),
    ("1K+1", true),
    ("1K-1", true),
    ("1B", true),
    ("8b", true),
    ("131072", true),
    ("1M", false),
    ("1G", false),
    ("1T", false),
    ("1P", false),
    ("131073", false),
    ("1Z", false),
    ("1KX", false),
];

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temp dir");
        fs::create_dir_all(root.path().join("src")).expect("src dir");
        fs::write(root.path().join("src/f"), b"hello\n").expect("src file");
        Self { root }
    }

    /// Runs a dry-run copy with the given `--block-size` and reports acceptance.
    fn accepts(&self, binary: &Path, value: &str) -> bool {
        Command::new(binary)
            .arg("-n")
            .arg(format!("--block-size={value}"))
            .arg(self.root.path().join("src/").display().to_string())
            .arg(self.root.path().join("dst/").display().to_string())
            .output()
            .expect("run binary")
            .status
            .success()
    }
}

/// oc must accept and reject exactly what the grammar table says.
#[test]
fn block_size_accepts_the_upstream_size_grammar() {
    let fixture = Fixture::new();
    let oc = oc_binary();
    for (value, expected) in GRAMMAR {
        assert_eq!(
            fixture.accepts(&oc, value),
            *expected,
            "--block-size={value} acceptance"
        );
    }
}

/// CROSS-IMPLEMENTATION: the table above is the oracle's, so assert it against
/// the oracle directly rather than trusting a transcribed constant.
#[test]
fn the_grammar_table_matches_the_real_upstream_binary() {
    let Some(upstream) = upstream_binary() else {
        println!(
            "SKIP: upstream 3.5.0 oracle not built \
             (target/interop/upstream-src/rsync-3.5.0/rsync)"
        );
        return;
    };
    let fixture = Fixture::new();
    for (value, expected) in GRAMMAR {
        assert_eq!(
            fixture.accepts(&upstream, value),
            *expected,
            "upstream 3.5.0 --block-size={value} acceptance"
        );
    }
}

/// Acceptance alone would pass on a parser that ignored the scale, so pin the
/// VALUES: `K` is 1024 and `KB` is 1000 (options.c:1197-1202).
///
/// The observable is the delta itemization: a basis differing from the source
/// only in its opening bytes yields a different matched/literal split at 1024
/// than at 1000, so `1K` must behave as `1024` and differ from `1000`.
#[test]
fn the_k_suffix_is_1024_and_kb_is_1000() {
    let fixture = Fixture::new();
    let oc = oc_binary();
    let root = fixture.root.path();

    // Non-periodic filler: a repeating pattern would match at any offset and
    // make every block size look identical.
    let mut data = Vec::with_capacity(8192);
    let mut state: u32 = 0x1234_5678;
    for _ in 0..8192 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        data.push((state & 0xff) as u8);
    }
    fs::write(root.join("src/big"), &data).expect("source");

    let stats_for = |block_size: &str| -> String {
        let dest = root.join(format!("dst-{block_size}"));
        fs::create_dir_all(&dest).expect("dest dir");
        let mut basis = data.clone();
        basis[0..40].fill(0);
        fs::write(dest.join("big"), &basis).expect("basis");

        let output = Command::new(&oc)
            // `--ignore-times` defeats the quick-check: the basis has the same
            // size and a fresh mtime, so without it rsync skips the file and
            // every block size reports 0 matched / 0 literal - which the
            // control assertion below caught when this fixture omitted it.
            .args(["--no-whole-file", "--ignore-times", "--stats", "-v"])
            .arg(format!("--block-size={block_size}"))
            .arg(root.join("src/big").display().to_string())
            .arg(dest.join("big").display().to_string())
            .output()
            .expect("run oc-rsync");
        assert!(
            output.status.success(),
            "--block-size={block_size} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines()
            .filter(|line| line.contains("Matched data") || line.contains("Literal data"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let k = stats_for("1K");
    let plain_1024 = stats_for("1024");
    let kb = stats_for("1KB");
    let plain_1000 = stats_for("1000");

    assert!(!k.is_empty(), "expected Matched/Literal lines in --stats");
    assert_eq!(k, plain_1024, "`1K` must resolve to 1024");
    assert_eq!(kb, plain_1000, "`1KB` must resolve to 1000");
    // The discriminator: without it, a parser that mapped every suffix to the
    // same number would satisfy both equalities above.
    assert_ne!(
        plain_1024, plain_1000,
        "fixture does not distinguish 1024 from 1000, so the two \
         assertions above prove nothing"
    );
}
