//! A filter file's records end at `\r` as well as at `\n`.
//!
//! upstream reads a filter file one character at a time and ends the record on
//! either terminator, collapsing `\r\n` into exactly one (`exclude.c:1774-1793`):
//!
//! ```c
//! if (eol_nulls? !ch : (ch == '\n' || ch == '\r')) {
//!     if (ch == '\r') {   /* CRLF is one line, not two */
//!         ...
//!         } else if (nxt != '\n' && ungetc(nxt, fp) == EOF) {
//!             pending = nxt;
//!         }
//!     }
//!     break;
//! }
//! ```
//!
//! All four oc readers spelled the split as Rust's `str::lines`/`read_until`,
//! which breaks only on `\n` and merely strips a `\r` that sits immediately
//! before one. A `\r`-separated filter file therefore collapsed into ONE
//! record whose pattern carried the `\r` and every following rule as literal
//! bytes - so it matched nothing, and each file those rules were meant to
//! exclude transferred instead, at exit 0.
//!
//! MEASURED against rsync 3.5.0 with a source holding `a` and `a ` (trailing
//! space) and a filter file of `- a \r`: upstream excludes `a ` and copies
//! `a`; oc copied both. The same divergence reproduced through a `.rsync-filter`
//! dir-merge, through `--filter='. FILE'`, and through `--exclude-from`. Every
//! cell below is asserted against that measured upstream behaviour.
//!
//! These assert on the DESTINATION TREE rather than on a parsed record list,
//! because the destination tree is the property that matters.

mod integration;

use integration::helpers::{RsyncCommand, TestDir};

/// The plain name.
const PLAIN: &str = "a";
/// The same name with one trailing space, so the record boundary decides the
/// match: a `\r` swallowed into the pattern makes the rule match neither name.
const TRAILING: &str = "a ";
/// An unrelated file, the negative control: no cell here may exclude it.
const OTHER: &str = "b";

/// Seeds `<dir>/src` with `a`, `a ` and `b`, and creates an empty `<dir>/dest`.
fn seed(dir: &TestDir) {
    dir.mkdir("src").expect("src");
    dir.mkdir("dest").expect("dest");
    dir.write_file("src/a", b"plain\n").expect("a");
    dir.write_file("src/a ", b"trailing\n").expect("a-space");
    dir.write_file("src/b", b"other\n").expect("b");
}

/// Runs `oc-rsync -r <extra...> src/ dest/` inside `dir`.
fn transfer(dir: &TestDir, extra: &[String]) -> RsyncCommand {
    let mut command = RsyncCommand::new();
    command.arg("-r");
    for argument in extra {
        command.arg(argument);
    }
    command
        .arg(format!("{}/", dir.path().join("src").display()))
        .arg(format!("{}/", dir.path().join("dest").display()));
    command
}

/// Asserts that `a ` was excluded while `a` and `b` landed.
fn assert_trailing_excluded(dir: &TestDir, how: &str) {
    assert!(
        !dir.exists(&format!("dest/{TRAILING}")),
        "{how}: the record ends at the carriage return, so the pattern is \
         `a ` and upstream excludes `{TRAILING}` (exclude.c:1774)"
    );
    assert!(
        dir.exists(&format!("dest/{PLAIN}")),
        "{how}: the pattern `a ` must not match `{PLAIN}`"
    );
    assert!(
        dir.exists(&format!("dest/{OTHER}")),
        "{how}: the unrelated file must still transfer"
    );
}

/// A dir-merge file whose only record is terminated by a lone `\r`.
#[test]
fn a_lone_carriage_return_ends_a_dir_merge_record() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    dir.write_file("src/.rsync-filter", b"- a \r")
        .expect("filter");

    transfer(&dir, &["-F".to_owned()]).assert_success();

    assert_trailing_excluded(&dir, "dir-merge, lone CR");
}

/// A `\r` BETWEEN two records separates them; the second one still applies.
#[test]
fn a_lone_carriage_return_separates_merge_file_records() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    let rules = dir.path().join("rules");
    std::fs::write(&rules, b"- zzz\r- a \n").expect("rules");

    transfer(&dir, &[format!("--filter=. {}", rules.display())]).assert_success();

    assert_trailing_excluded(&dir, "merge file, embedded CR");
}

/// `--exclude-from` reads its records through the same boundary.
#[test]
fn a_lone_carriage_return_separates_exclude_from_records() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    let rules = dir.path().join("rules");
    std::fs::write(&rules, b"zzz\ra \n").expect("rules");

    transfer(&dir, &[format!("--exclude-from={}", rules.display())]).assert_success();

    assert_trailing_excluded(&dir, "--exclude-from, embedded CR");
}

/// CRLF is ONE terminator, not two: it must not manufacture an empty record
/// that then fails as `Unknown filter rule`.
///
/// upstream: `exclude.c:1775-1791` looks one character past the `\r` and
/// swallows a following `\n`.
#[test]
fn crlf_is_one_terminator() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    dir.write_file("src/.rsync-filter", b"- zzz\r\n- a \r\n")
        .expect("filter");

    transfer(&dir, &["-F".to_owned()]).assert_success();

    assert_trailing_excluded(&dir, "dir-merge, CRLF");
}

/// The negative control: a plain LF-terminated file is untouched by all of
/// this, and a rule WITHOUT trailing whitespace still means what it says.
#[test]
fn a_plain_newline_file_is_unaffected() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    dir.write_file("src/.rsync-filter", b"- b\n")
        .expect("filter");

    transfer(&dir, &["-F".to_owned()]).assert_success();

    assert!(
        !dir.exists(&format!("dest/{OTHER}")),
        "`- b` must still exclude `{OTHER}`"
    );
    for name in [PLAIN, TRAILING] {
        assert!(
            dir.exists(&format!("dest/{name}")),
            "`- b` must not touch `{name}`"
        );
    }
}
