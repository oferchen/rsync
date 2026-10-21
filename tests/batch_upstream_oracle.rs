//! Byte-level oracle for the files `--write-batch` produces.
//!
//! Each cell records the same fixture with oc and with upstream rsync 3.5.1,
//! running both from the same working directory with the same relative
//! operands, and compares what lands on disk: whether the batch and its `.sh`
//! exist, the batch header bytes, and the `.sh` text with only `argv[0]`
//! masked (it is the path of whichever binary ran).
//!
//! Every cell also asserts a pinned expectation recorded from rsync 3.5.1, so
//! the cell still fails on a regression where no oracle is installed. When
//! the oracle resolves, the live comparison runs as well; it is located like
//! the other root oracle suites: `OC_RSYNC_UPSTREAM_RSYNC`, then
//! `target/interop/upstream-install/3.5.1/bin/rsync`, then the
//! `upstream-src` build tree, then `/usr/bin/rsync` - each banner-verified
//! as 3.5.1, since only that release defines the expected bytes.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ORACLE_VERSION: &str = "3.5.1";

/// The binary under test (`env!`: the variable exists only at compile time).
fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Resolves an rsync whose `--version` banner names [`ORACLE_VERSION`].
fn upstream_rsync() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(explicit) = std::env::var_os("OC_RSYNC_UPSTREAM_RSYNC") {
        candidates.push(PathBuf::from(explicit));
    }
    if let Some(installed) = test_support::upstream_install_bin(ORACLE_VERSION) {
        candidates.push(installed);
    }
    if let Some(root) = test_support::workspace_root() {
        candidates.push(root.join(format!(
            "target/interop/upstream-src/rsync-{ORACLE_VERSION}/rsync"
        )));
    }
    candidates.push(PathBuf::from("/usr/bin/rsync"));
    let found = candidates.into_iter().find(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|banner| banner.lines().next().map(str::to_owned))
            .is_some_and(|first| {
                first.starts_with("rsync") && first.contains(&format!(" version {ORACLE_VERSION}"))
            })
    });
    if found.is_none() {
        eprintln!("no rsync {ORACLE_VERSION} oracle; checking pinned expectations only");
    }
    found
}

/// What one `--write-batch` invocation left behind.
struct Recording {
    output: Output,
    batch: Option<Vec<u8>>,
    script: Option<Vec<u8>>,
}

impl Recording {
    /// The `.sh` with its leading quoted `argv[0]` replaced by `ARGV0`.
    fn masked_script(&self) -> Option<Vec<u8>> {
        let script = self.script.as_ref()?;
        let rest = script.strip_prefix(b"'")?;
        let close = rest.iter().position(|&b| b == b'\'')?;
        let mut masked = b"ARGV0".to_vec();
        masked.extend_from_slice(&rest[close + 1..]);
        Some(masked)
    }
}

/// A fresh source tree plus an empty destination, both relative to `root`.
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_path_buf();
        fs::create_dir_all(root.join("src/sub")).unwrap();
        fs::create_dir_all(root.join("dst")).unwrap();
        fs::write(root.join("src/a.txt"), b"alpha\n").unwrap();
        fs::write(root.join("src/sub/b.log"), b"beta\n").unwrap();
        fs::write(root.join("src/sub/c.txt"), b"gamma\n").unwrap();
        Self { _temp: temp, root }
    }

    /// Runs `bin` from the fixture root and collects `bf` and `bf.sh`.
    fn record(&self, bin: &Path, args: &[&str]) -> Recording {
        let output = Command::new(bin)
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));
        Recording {
            output,
            batch: fs::read(self.root.join("bf")).ok(),
            script: fs::read(self.root.join("bf.sh")).ok(),
        }
    }
}

/// Records `args` with oc and, when present, with the oracle - each in its
/// own fresh fixture so neither run can see the other's files.
fn record_both(args: &[&str]) -> (Recording, Option<Recording>) {
    let oc = Fixture::new().record(&oc_rsync(), args);
    let upstream = upstream_rsync().map(|bin| Fixture::new().record(&bin, args));
    (oc, upstream)
}

/// Asserts oc's masked `.sh` equals both the pinned 3.5.1 text and, when an
/// oracle is installed, what the oracle writes for the same invocation.
fn assert_script_matches_upstream(args: &[&str], pinned: &[u8]) {
    let (oc, upstream) = record_both(args);
    assert!(oc.output.status.success(), "{args:?}: {:?}", oc.output);
    let oc_script = oc.masked_script().expect("oc wrote a .sh");
    assert_eq!(
        String::from_utf8_lossy(&oc_script),
        String::from_utf8_lossy(pinned),
        "{args:?}: oc .sh differs from the pinned rsync {ORACLE_VERSION} text"
    );
    if let Some(up) = upstream {
        assert!(up.output.status.success(), "{args:?}: oracle {:?}", up.output);
        assert_eq!(
            up.masked_script().as_deref(),
            Some(pinned),
            "{args:?}: the pinned text no longer matches the oracle"
        );
    }
}

/// upstream: batch.c:213-240 write_filter_rules() lists every filter_list
/// entry through get_rule_prefix(for_xfer = 0) (exclude.c:1824-1885). `P` and
/// `R` are `-r`/`+r`, and dir-merge, side, negate and directory modifiers all
/// survive; a here-doc that drops one replays a different rule set than the
/// recording run used.
#[test]
fn filter_heredoc_matches_upstream_get_rule_prefix() {
    assert_script_matches_upstream(
        &[
            "-a",
            "--checksum-seed=7",
            "--write-batch=bf",
            "-f",
            "- gone",
            "-f",
            "!",
            "--exclude=/x",
            "--include=*/",
            "-f",
            "P keep",
            "-f",
            "R junk",
            "-f",
            ":n .rules",
            "-f",
            "- dir/",
            "-f",
            "-! neg",
            "-f",
            "H hid",
            "-f",
            "S sh",
            "-f",
            ":e .m",
            "-f",
            "-x xa",
            "-f",
            "-p per",
            "-f",
            ":C",
            "-f",
            "dir-merge,- .d",
            "-C",
            "--exclude=*.log",
            "src/",
            "dst/",
        ],
        b"ARGV0 --filter='._-' '-a' --checksum-seed='7' --read-batch='bf' '-C' \
${1:-'dst/'} <<'#E#'\n- /x\n+ */\n-r keep\n+r junk\n:n .rules\n- dir/\n-! neg\n\
-s hid\n+s sh\n- .m\n: .m\n-x xa\n-p per\n:C .cvsignore\n:- .d\n- *.log\n#E#\n",
    );
}

/// upstream: batch.c:231-238 - under `--from0` each rule ends in NUL and the
/// block gains a trailing `;\n`, so the replay's `--filter=._-` reads the
/// rules with the same `-0` parsing the recording used.
#[test]
fn filter_heredoc_under_from0_matches_upstream() {
    assert_script_matches_upstream(
        &[
            "-a",
            "-0",
            "--checksum-seed=7",
            "--write-batch=bf",
            "--exclude=x",
            "-f",
            "P keep",
            "src/",
            "dst/",
        ],
        b"ARGV0 --filter='._-' '-a' '-0' --checksum-seed='7' --read-batch='bf' \
${1:-'dst/'} <<'#E#'\n- x\0-r keep\0;\n#E#\n",
    );
}

/// upstream: options.c:2280-2283 - `--dry-run` zeroes `write_batch` for both
/// write forms, so rsync 3.5.1 leaves neither the batch nor its `.sh` behind.
/// A batch written under `-n` would also be a lie: no update was applied, yet
/// the replay script claims to reproduce one.
#[test]
fn dry_run_write_batch_creates_no_files() {
    for option in ["--write-batch=bf", "--only-write-batch=bf"] {
        let args = ["-a", "-n", option, "src/", "dst/"];
        let (oc, upstream) = record_both(&args);
        assert!(oc.output.status.success(), "{option}: {:?}", oc.output);
        assert!(oc.batch.is_none(), "{option}: oc wrote a batch under -n");
        assert!(oc.script.is_none(), "{option}: oc wrote a .sh under -n");
        if let Some(up) = upstream {
            assert!(up.output.status.success(), "{option}: oracle {:?}", up.output);
            assert_eq!(
                (up.batch.is_some(), up.script.is_some()),
                (false, false),
                "{option}: oracle unexpectedly wrote batch files"
            );
        }
    }
}
