//! An empty size value means `0`, per option, exactly as upstream resolves it.
//!
//! ```c
//! /* options.c:1172-1175 - the digit scan stops on the terminator, so the
//!    suffix switch takes def_suf and strtod("") gives 0. */
//! for (arg = size_arg; isDigit(arg); arg++) {}
//! ...
//! dsize = strtod(size_arg, NULL);
//! ```
//!
//! **This is not a "fall back to the default" rule**, which is how the defect
//! was first written up. Whether the resulting 0 is legal, and what it means,
//! is decided by each option's own `min_value` / `unlimited_0`:
//!
//! | option | upstream call | empty resolves to |
//! |---|---|---|
//! | `--block-size` | min 0 (options.c:1802) | 0 -> default block size |
//! | `--min-size` | min 0 (:1809) | 0 -> no lower bound |
//! | `--max-size` | min 0 (:1815) | 0 -> **excludes every non-empty file** |
//! | `--bwlimit` | min 512, `unlimited_0` (:1821) | 0 -> unlimited |
//! | `--max-alloc` | min 1 MiB (:2067) | **rejected** |
//!
//! `--max-size=` excluding everything is why a success-only assertion would be
//! the wrong oracle here, and why the rule cannot live in the shared string
//! parser: folding it in would silently start accepting `--max-alloc=`, undoing
//! the guard that landed for the zero case.
//!
//! Skip conditions (test passes with a printed reason):
//! - The cross-implementation cell needs a built upstream 3.5.0 binary.

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

/// The observable outcome of one run: did it succeed, and did the file arrive.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    success: bool,
    transferred: bool,
}

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temp dir");
        fs::create_dir_all(root.path().join("src")).expect("src dir");
        // Six bytes: non-empty, so a `--max-size=0` bound excludes it.
        fs::write(root.path().join("src/f"), b"hello\n").expect("src file");
        Self { root }
    }

    /// Runs one copy with the given option spelling and reports what happened.
    fn run(&self, binary: &Path, arg: Option<&str>) -> Outcome {
        let dest = self.root.path().join("dst");
        let _ = fs::remove_dir_all(&dest);
        fs::create_dir_all(&dest).expect("dest dir");

        let mut command = Command::new(binary);
        command.arg("-r");
        if let Some(arg) = arg {
            command.arg(arg);
        }
        let status = command
            .arg(format!("{}/", self.root.path().join("src").display()))
            .arg(format!("{}/", dest.display()))
            .status()
            .expect("run binary");

        Outcome {
            success: status.success(),
            transferred: dest.join("f").is_file(),
        }
    }
}

/// Every option whose empty value upstream accepts, and the one it does not.
const OPTIONS: &[&str] = &["--block-size", "--min-size", "--max-size", "--bwlimit"];

/// The whole rule in one assertion: for each option, an empty value must be
/// indistinguishable from `=0`.
///
/// This pins the RESOLVED VALUE rather than mere acceptance - for `--max-size`
/// the two agree by both *excluding* the file, which a success-only check would
/// have missed and an "it falls back to the default" check would have got
/// backwards.
fn assert_empty_matches_zero(binary: &Path, label: &str) {
    let fixture = Fixture::new();
    for option in OPTIONS {
        let empty = fixture.run(binary, Some(&format!("{option}=")));
        let zero = fixture.run(binary, Some(&format!("{option}=0")));
        assert_eq!(
            empty, zero,
            "{label}: `{option}=` must resolve exactly like `{option}=0`"
        );
        assert!(
            empty.success,
            "{label}: `{option}=` must be accepted, got {empty:?}"
        );
    }
}

#[test]
fn oc_resolves_an_empty_size_value_to_zero() {
    assert_empty_matches_zero(&oc_binary(), "oc");
}

/// CROSS-IMPLEMENTATION: the expected column is the oracle's behaviour, so
/// assert it against the oracle rather than trusting the transcription.
#[test]
fn upstream_resolves_an_empty_size_value_to_zero() {
    let Some(upstream) = upstream_binary() else {
        println!(
            "SKIP: upstream 3.5.0 oracle not built \
             (target/interop/upstream-src/rsync-3.5.0/rsync)"
        );
        return;
    };
    assert_empty_matches_zero(&upstream, "upstream 3.5.0");
}

/// THE DISCRIMINATOR. `--max-size=` is not "no limit" and not "the default":
/// it is a real bound of 0 that excludes every non-empty file. Without this
/// case, a build that accepted the empty string and then ignored it would
/// satisfy every assertion above.
#[test]
fn an_empty_max_size_excludes_the_file_rather_than_defaulting() {
    let fixture = Fixture::new();
    let oc = oc_binary();

    let unset = fixture.run(&oc, None);
    let empty = fixture.run(&oc, Some("--max-size="));

    assert!(unset.transferred, "control: an unset --max-size transfers");
    assert!(
        empty.success && !empty.transferred,
        "an empty --max-size must be accepted AND bound the size to 0, got {empty:?}"
    );
}

/// CONTROL: the fix must not accept everything - a real value still parses and
/// still applies.
#[test]
fn a_non_empty_size_value_still_parses_and_applies() {
    let fixture = Fixture::new();
    let oc = oc_binary();

    assert!(
        fixture.run(&oc, Some("--max-size=1K")).transferred,
        "a 6-byte file is under a 1K cap and must transfer"
    );
    assert!(
        !fixture.run(&oc, Some("--max-size=3")).transferred,
        "a 6-byte file is over a 3-byte cap and must be excluded"
    );
    assert!(
        !fixture.run(&oc, Some("--max-size=nonsense")).success,
        "an unparseable size must still be rejected"
    );
}

/// GUARD: `--max-alloc` has a 1 MiB minimum upstream, so 0 - and therefore an
/// empty value - is rejected by both implementations. The empty-to-zero rule is
/// applied per option precisely so this stays rejected; putting it in the
/// shared string parser would have quietly re-opened it.
///
/// upstream: options.c:2067 `parse_size_arg(..., "max-alloc", 1024*1024, ...)`.
#[test]
fn an_empty_max_alloc_is_still_rejected() {
    let fixture = Fixture::new();
    let oc = oc_binary();

    assert!(
        !fixture.run(&oc, Some("--max-alloc=")).success,
        "--max-alloc= must stay rejected"
    );
    assert!(
        !fixture.run(&oc, Some("--max-alloc=0")).success,
        "--max-alloc=0 must stay rejected"
    );
    assert!(
        fixture.run(&oc, Some("--max-alloc=1048576")).success,
        "control: a legal --max-alloc must still be accepted"
    );
}
