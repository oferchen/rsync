//! A filter file that cannot be opened is fatal with `RERR_FILEIO` (11).
//!
//! ```c
//! /* exclude.c:1712-1719 - parse_filter_file(), the single site upstream
//!    reports every failed filter-file open from. */
//! if (!fp) {
//!     if (xflags & XFLG_FATAL_ERRORS) {
//!         ...
//!         rsyserr(FERROR, errno, "failed to open %sclude file %s",
//!                 template->rflags & FILTRULE_INCLUDE ? "in" : "ex", fname);
//!         exit_cleanup(RERR_FILEIO);
//!     }
//!     merge_depth--;
//!     return;
//! }
//! ```
//!
//! CLASS, not one option: `XFLG_FATAL_ERRORS` is passed by every operator-named
//! filter file - `--exclude-from` and `--include-from` (options.c:1648) and the
//! non-per-directory merge rules `merge FILE` / `. FILE` (exclude.c:1587). All
//! of them therefore share one rule, and the table below drives them together.
//!
//! Two things the table deliberately pins as NOT fatal, because getting either
//! wrong would be a worse bug than the one being fixed:
//!
//! - `dir-merge` (`:` / `-F`) passes no fatal flag (exclude.c:912), so a missing
//!   per-directory filter file is normal and must stay silent. A fix that keyed
//!   off "filter file missing" rather than off the call site would break every
//!   `-F` transfer.
//! - `--files-from` is a different site with a different code: upstream exits 1
//!   there (main.c:1886), measured. Folding both onto one helper because the
//!   messages look alike would change an exit code nobody asked to change.
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

/// One option spelling, and what upstream does when its file is missing.
struct Case {
    /// Formatted with the missing path.
    argument: &'static str,
    /// `None` when the missing file is not an error at all.
    expected: Option<Expected>,
}

struct Expected {
    code: i32,
    /// Substring both implementations must render, minus the path.
    wording: &'static str,
}

const FATAL_EXCLUDE: Option<Expected> = Some(Expected {
    code: 11,
    wording: "failed to open exclude file ",
});
const FATAL_INCLUDE: Option<Expected> = Some(Expected {
    code: 11,
    wording: "failed to open include file ",
});

/// Every operator-named filter file, plus the two that must NOT be fatal.
const CASES: &[Case] = &[
    Case {
        argument: "--exclude-from={path}",
        expected: FATAL_EXCLUDE,
    },
    Case {
        argument: "--include-from={path}",
        expected: FATAL_INCLUDE,
    },
    Case {
        argument: "--filter=merge {path}",
        expected: FATAL_EXCLUDE,
    },
    Case {
        argument: "--filter=. {path}",
        expected: FATAL_EXCLUDE,
    },
    // The `+` modifier flips the wording, which is the only evidence that the
    // include/exclude word is derived rather than hardcoded per option.
    Case {
        argument: "--filter=.+ {path}",
        expected: FATAL_INCLUDE,
    },
    Case {
        argument: "--filter=merge,+ {path}",
        expected: FATAL_INCLUDE,
    },
    // Per-directory merges carry no fatal flag: a missing .rsync-filter is the
    // normal case for -F and must not fail the transfer.
    Case {
        argument: "--filter=dir-merge {path}",
        expected: None,
    },
    Case {
        argument: "-F",
        expected: None,
    },
];

struct Outcome {
    code: i32,
    stderr: String,
    transferred: bool,
}

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

    /// The path that does not exist. Kept free of shell metacharacters and
    /// spaces so the `--filter=merge {path}` spelling stays unambiguous.
    fn missing(&self) -> PathBuf {
        self.root.path().join("absent-filter-file")
    }

    fn run(&self, binary: &Path, argument: &str) -> Outcome {
        let dest = self.root.path().join("dst");
        let _ = fs::remove_dir_all(&dest);
        fs::create_dir_all(&dest).expect("dest dir");

        let argument = argument.replace("{path}", &self.missing().display().to_string());
        let output = Command::new(binary)
            .arg("-r")
            .arg(argument)
            .arg(format!("{}/", self.root.path().join("src").display()))
            .arg(format!("{}/", dest.display()))
            .output()
            .expect("run binary");

        Outcome {
            code: output.status.code().expect("exited normally"),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            transferred: dest.join("f").is_file(),
        }
    }
}

/// The whole table against one binary. Asserts the exit CODE and the WORDING,
/// not merely that something failed: a build that exited 11 with an unrelated
/// message, or produced upstream's text under exit 1, would be a different bug
/// and must not pass here.
fn assert_table(binary: &Path, label: &str) {
    let fixture = Fixture::new();
    let missing = fixture.missing().display().to_string();

    for case in CASES {
        let outcome = fixture.run(binary, case.argument);
        match &case.expected {
            Some(expected) => {
                assert_eq!(
                    outcome.code, expected.code,
                    "{label}: `{}` must exit {}, got {} - {}",
                    case.argument, expected.code, outcome.code, outcome.stderr
                );
                assert!(
                    outcome.stderr.contains(expected.wording),
                    "{label}: `{}` must report `{}`, got: {}",
                    case.argument,
                    expected.wording,
                    outcome.stderr
                );
                assert!(
                    outcome.stderr.contains(&missing),
                    "{label}: `{}` must name the file it could not open, got: {}",
                    case.argument,
                    outcome.stderr
                );
                assert!(
                    !outcome.transferred,
                    "{label}: `{}` aborts before transferring",
                    case.argument
                );
            }
            None => {
                assert_eq!(
                    outcome.code, 0,
                    "{label}: `{}` must succeed - a missing per-directory filter \
                     file is the normal case, got: {}",
                    case.argument, outcome.stderr
                );
                assert!(
                    outcome.transferred,
                    "{label}: `{}` must still transfer the file",
                    case.argument
                );
            }
        }
    }
}

#[test]
fn oc_reports_an_unopenable_filter_file_like_upstream() {
    assert_table(&oc_binary(), "oc");
}

/// CROSS-IMPLEMENTATION: the expected column is upstream's behaviour, so assert
/// it against upstream rather than trusting the transcription.
#[test]
fn upstream_reports_an_unopenable_filter_file_as_expected() {
    let Some(upstream) = upstream_binary() else {
        println!(
            "SKIP: upstream 3.5.0 oracle not built \
             (target/interop/upstream-src/rsync-3.5.0/rsync)"
        );
        return;
    };
    assert_table(&upstream, "upstream 3.5.0");
}

/// CONTROL: a filter file that DOES open must still work, so the fix cannot be
/// satisfied by failing on every filter file.
#[test]
fn a_readable_filter_file_still_applies() {
    let fixture = Fixture::new();
    let rules = fixture.root.path().join("rules");
    fs::write(&rules, b"f\n").expect("write rules");

    let outcome = fixture.run(
        &oc_binary(),
        &format!("--exclude-from={}", rules.display()).replace("{path}", ""),
    );

    assert_eq!(outcome.code, 0, "a readable filter file must not fail");
    assert!(
        !outcome.transferred,
        "the rule inside the file must still be applied"
    );
}

/// GUARD: `--files-from` shares the "operator names a file that is missing"
/// shape but NOT the exit code - upstream reports it from main.c and exits 1.
/// Pinned here so a later tidy-up that unifies the two messages has to notice.
#[test]
fn a_missing_files_from_stays_exit_1() {
    let fixture = Fixture::new();
    let outcome = fixture.run(&oc_binary(), "--files-from={path}");

    assert_eq!(
        outcome.code, 1,
        "--files-from is a different upstream site and keeps exit 1: {}",
        outcome.stderr
    );
}
