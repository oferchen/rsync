//! A filter rule's whitespace is pattern text, not decoration.
//!
//! upstream reads a merge file byte for byte and skips a line only when it is
//! EMPTY or when its FIRST byte is `;`/`#` (`exclude.c:1806`):
//!
//! ```c
//! if (*line && (word_split || (*line != ';' && *line != '#')))
//!     parse_filter_str(listp, line, template, xflags);
//! ```
//!
//! `parse_rule_tok` then skips leading whitespace only under
//! `FILTRULE_WORD_SPLIT` (`exclude.c:1249-1253`) and takes the pattern length
//! as `len = strlen((char*)s)` (`exclude.c:1465`). So trailing whitespace is
//! part of the pattern, exactly ONE separator is consumed after the rule
//! character (`exclude.c:1444-1445`, `if (*s) s++`), and a leading-whitespace
//! or whitespace-only line reaches the prefix switch's `default:` arm and dies
//! at `exclude.c:1363` with `Unknown filter rule`.
//!
//! TWO oc readers trimmed. The engine dir-merge parser trimmed both ends of
//! every line, and the CLI `--exclude-from`/`--include-from` reader trimmed
//! before testing for a blank or a comment. Because a filter pattern is
//! matched literally, both changed WHICH FILES TRANSFER at exit 0 - silent
//! data selection divergence, not cosmetics. The third reader, the CLI
//! `--filter='merge FILE'` loop (`crates/cli/.../filter_rules/merge.rs`),
//! already mirrored upstream, which is what made this a one-of-three lag
//! rather than a uniform choice.
//!
//! MEASURED against rsync 3.5.0 with a source holding `a` and `a ` (trailing
//! space) and a `.rsync-filter` of `- a `: upstream excludes `a ` and copies
//! `a`; oc excluded `a` and copied `a `. Every cell below is asserted against
//! that measured upstream behaviour.
//!
//! These assert on the DESTINATION TREE rather than on a parsed pattern
//! string, because the destination tree is the property that matters.

mod integration;

use integration::helpers::{RsyncCommand, TestDir};

/// The plain name.
const PLAIN: &str = "a";
/// The same name with one trailing space. Legal on every supported filesystem.
const TRAILING: &str = "a ";
/// An unrelated file, the negative control: no cell here may exclude it.
const OTHER: &str = "b";

/// Seeds `<dir>/src` with `a`, `a ` and `b`, plus a `.rsync-filter` holding
/// `body`, and creates an empty `<dir>/dest`.
fn seed(dir: &TestDir, body: &str) {
    dir.mkdir("src").expect("src");
    dir.mkdir("dest").expect("dest");
    dir.write_file("src/a", b"plain\n").expect("a");
    dir.write_file("src/a ", b"trailing\n").expect("a-space");
    dir.write_file("src/b", b"other\n").expect("b");
    dir.write_file("src/.rsync-filter", body.as_bytes())
        .expect("filter");
}

/// Runs `oc-rsync -r -F src/ dest/` inside `dir`.
fn transfer(dir: &TestDir) -> RsyncCommand {
    let mut command = RsyncCommand::new();
    command
        .arg("-r")
        .arg("-F")
        .arg(format!("{}/", dir.path().join("src").display()))
        .arg(format!("{}/", dir.path().join("dest").display()));
    command
}

/// `- a ` excludes the file whose name ENDS IN A SPACE, and leaves `a` alone.
///
/// This is the headline cell. Trimming inverted it: oc excluded `a` and copied
/// `a `, the exact opposite of upstream, at exit 0.
#[test]
fn a_trailing_space_belongs_to_the_pattern() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir, "- a \n");

    transfer(&dir).assert_success();

    assert!(
        dir.exists(&format!("dest/{PLAIN}")),
        "`- a ` must NOT exclude `{PLAIN}`: the pattern is `a ` with the \
         trailing space, and upstream matches it literally (exclude.c:1465)"
    );
    assert!(
        !dir.exists(&format!("dest/{TRAILING}")),
        "`- a ` must exclude `{TRAILING}`: upstream takes the pattern length \
         as strlen, so the trailing space is pattern text (exclude.c:1465)"
    );
    assert!(
        dir.exists(&format!("dest/{OTHER}")),
        "the unrelated file must still transfer"
    );
}

/// Two trailing spaces make a pattern that matches NOTHING in the fixture.
///
/// The negative control for the cell above: it proves the rule is matched
/// literally rather than merely "one trailing space is special".
#[test]
fn two_trailing_spaces_match_no_file_here() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir, "- a  \n");

    transfer(&dir).assert_success();

    for name in [PLAIN, TRAILING, OTHER] {
        assert!(
            dir.exists(&format!("dest/{name}")),
            "`- a  ` names the pattern `a  `, which matches nothing here, so \
             `{name}` must transfer"
        );
    }
}

/// Only ONE separator is consumed after the rule character, so `-  a` is a
/// rule for the pattern ` a`, not `a`.
///
/// upstream: `exclude.c:1444-1445` - the modifier loop stops at the first
/// ` `/`_` and `if (*s) s++` consumes exactly that one byte.
#[test]
fn only_one_separator_follows_the_rule_character() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir, "-  a\n");

    transfer(&dir).assert_success();

    assert!(
        dir.exists(&format!("dest/{PLAIN}")),
        "`-  a` names the pattern ` a` (leading space), so `{PLAIN}` must \
         transfer: upstream consumes exactly one separator (exclude.c:1444)"
    );
    assert!(
        dir.exists(&format!("dest/{TRAILING}")),
        "`-  a` must not touch `{TRAILING}` either"
    );
}

/// The long-keyword form takes its pattern verbatim too.
#[test]
fn a_keyword_rule_keeps_its_trailing_space() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir, "exclude a \n");

    transfer(&dir).assert_success();

    assert!(
        dir.exists(&format!("dest/{PLAIN}")),
        "`exclude a ` names the pattern `a `, not `a`"
    );
    assert!(
        !dir.exists(&format!("dest/{TRAILING}")),
        "`exclude a ` must exclude `{TRAILING}`"
    );
}

/// A whitespace-only line is a fatal syntax error, not a blank line.
///
/// upstream: `exclude.c:1806` skips only an EMPTY line, so `   ` reaches
/// `parse_rule_tok`, takes the prefix switch's `default:` arm and dies at
/// `exclude.c:1363`. MEASURED against rsync 3.5.0: exit 1, nothing transfers.
#[test]
fn a_whitespace_only_line_is_a_syntax_error() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir, "   \n- b\n");

    let output = transfer(&dir).assert_failure();

    assert!(
        !dir.exists(&format!("dest/{PLAIN}")),
        "a rejected filter file must abort the transfer, not partly apply it"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unknown filter rule"),
        "expected upstream's `Unknown filter rule` refusal, got: {stderr}"
    );
}

/// A rule indented by whitespace is a fatal syntax error, not an indented rule.
#[test]
fn a_leading_whitespace_rule_is_a_syntax_error() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir, "  - a\n");

    let output = transfer(&dir).assert_failure();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unknown filter rule"),
        "expected upstream's `Unknown filter rule` refusal, got: {stderr}"
    );
}

/// A `#` comment must start at column ZERO. An indented one is a rule, and an
/// unparsable one at that.
///
/// upstream: `exclude.c:1806` tests `*line`, the FIRST byte, so `  # x` is
/// never a comment.
#[test]
fn a_comment_marker_must_be_the_first_byte() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir, "  # x\n- b\n");

    let output = transfer(&dir).assert_failure();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unknown filter rule"),
        "expected upstream's `Unknown filter rule` refusal, got: {stderr}"
    );
}

/// `;` at column zero IS a comment, exactly like `#`.
///
/// upstream: `exclude.c:1806` skips a line whose first byte is `;` OR `#`. oc
/// tested only `#`, so a `;` comment was parsed as a rule and exited 1.
#[test]
fn a_semicolon_at_column_zero_is_a_comment() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir, "; a comment\n- b\n");

    transfer(&dir).assert_success();

    assert!(
        dir.exists(&format!("dest/{PLAIN}")),
        "`; ...` is a comment, so only the `- b` rule applies"
    );
    assert!(
        !dir.exists(&format!("dest/{OTHER}")),
        "the `- b` rule after the `;` comment must still be honoured"
    );
}

/// A file literally named `   ` (three spaces).
const SPACES: &str = "   ";
/// A file literally named `  #a` - an indented comment marker.
const INDENTED_HASH: &str = "  #a";

/// Seeds `<dir>/src` with the two exotic names plus `keep`, and writes an
/// `--exclude-from` file holding `body`.
fn seed_exclude_from(dir: &TestDir, body: &str) {
    dir.mkdir("src").expect("src");
    dir.mkdir("dest").expect("dest");
    dir.write_file("src/keep", b"keep\n").expect("keep");
    dir.write_file(&format!("src/{SPACES}"), b"spaces\n")
        .expect("spaces");
    dir.write_file(&format!("src/{INDENTED_HASH}"), b"hash\n")
        .expect("hash");
    dir.write_file("filt", body.as_bytes()).expect("filt");
}

/// Runs `oc-rsync -r --exclude-from=filt src/ dest/` inside `dir`.
fn transfer_exclude_from(dir: &TestDir) -> RsyncCommand {
    let mut command = RsyncCommand::new();
    command
        .arg("-r")
        .arg(format!(
            "--exclude-from={}",
            dir.path().join("filt").display()
        ))
        .arg(format!("{}/", dir.path().join("src").display()))
        .arg(format!("{}/", dir.path().join("dest").display()));
    command
}

/// A whitespace-only `--exclude-from` line is a PATTERN, not a blank line.
///
/// upstream: `exclude.c:1806` tests `*line`, the first byte, so `   ` is not
/// empty. MEASURED against rsync 3.5.0: it excludes the file named `   `.
#[test]
fn a_whitespace_only_exclude_from_line_is_a_pattern() {
    let dir = TestDir::new().expect("scratch dir");
    seed_exclude_from(&dir, "   \n");

    transfer_exclude_from(&dir).assert_success();

    assert!(
        !dir.exists(&format!("dest/{SPACES}")),
        "an `--exclude-from` line of three spaces is the pattern `   `, which \
         must exclude the file of that name (exclude.c:1806)"
    );
    assert!(dir.exists("dest/keep"), "`keep` must still transfer");
    assert!(
        dir.exists(&format!("dest/{INDENTED_HASH}")),
        "`{INDENTED_HASH}` must still transfer"
    );
}

/// An INDENTED `#` in an `--exclude-from` file is a pattern, not a comment.
///
/// upstream: `exclude.c:1806` - only a `;`/`#` in column zero is a comment.
#[test]
fn an_indented_hash_in_an_exclude_from_file_is_a_pattern() {
    let dir = TestDir::new().expect("scratch dir");
    seed_exclude_from(&dir, "  #a\n");

    transfer_exclude_from(&dir).assert_success();

    assert!(
        !dir.exists(&format!("dest/{INDENTED_HASH}")),
        "`  #a` is indented, so it is the pattern `  #a` and must exclude the \
         file of that name (exclude.c:1806)"
    );
    assert!(dir.exists("dest/keep"), "`keep` must still transfer");
    assert!(
        dir.exists(&format!("dest/{SPACES}")),
        "`{SPACES}` must still transfer"
    );
}

/// The `--exclude-from` control: a `#` in COLUMN ZERO really is a comment, so
/// the fix must not turn every comment into a pattern.
#[test]
fn a_column_zero_hash_in_an_exclude_from_file_is_still_a_comment() {
    let dir = TestDir::new().expect("scratch dir");
    seed_exclude_from(&dir, "#a\n;b\nkeep\n");

    transfer_exclude_from(&dir).assert_success();

    assert!(
        !dir.exists("dest/keep"),
        "the `keep` rule after the two comments must be honoured"
    );
    for name in [SPACES, INDENTED_HASH] {
        assert!(
            dir.exists(&format!("dest/{name}")),
            "`#a`/`;b` in column zero are comments, so `{name}` must transfer"
        );
    }
}

/// The unchanged baseline: a rule with no stray whitespace behaves as before.
///
/// This is the control that must stay green under the mutation that reverts
/// the fix - a test that fails both ways would prove nothing about the cells
/// above.
#[test]
fn a_plain_rule_is_unaffected() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir, "- a\n");

    transfer(&dir).assert_success();

    assert!(
        !dir.exists(&format!("dest/{PLAIN}")),
        "`- a` must exclude `{PLAIN}`"
    );
    assert!(
        dir.exists(&format!("dest/{TRAILING}")),
        "`- a` must not exclude `{TRAILING}`"
    );
    assert!(
        dir.exists(&format!("dest/{OTHER}")),
        "`- a` must not exclude `{OTHER}`"
    );
}
