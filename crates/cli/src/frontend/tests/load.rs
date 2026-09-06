use super::common::*;
use super::*;

#[test]
fn load_filter_file_patterns_skips_comments_and_trims_crlf() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("filters.txt");
    std::fs::write(&path, b"# comment\r\n\r\n include \r\npattern\r\n").expect("write filters");

    let patterns =
        load_filter_file_patterns(path.as_path()).expect("load filter patterns succeeds");

    assert_eq!(patterns, vec![" include ".to_owned(), "pattern".to_owned()]);
}

/// A `;` comment must start in COLUMN ZERO; an indented one is a pattern.
///
/// upstream: exclude.c:1806 - `if (*line && (word_split || (*line != ';' &&
/// *line != '#')))` tests the FIRST BYTE of the line, with no trimming first.
/// MEASURED against rsync 3.5.0: an `--exclude-from` file holding only `  #a`
/// excludes a file literally named `  #a`; oc used to transfer it, because the
/// reader trimmed before testing.
#[test]
fn load_filter_file_patterns_skip_only_column_zero_semicolon_comments() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("filters-semicolon.txt");
    std::fs::write(&path, b"; leading comment\n  ; spaced comment\nkeep\n").expect("write filters");

    let patterns =
        load_filter_file_patterns(path.as_path()).expect("load filter patterns succeeds");

    assert_eq!(
        patterns,
        vec!["  ; spaced comment".to_owned(), "keep".to_owned()]
    );
}

#[test]
fn load_filter_file_patterns_handles_invalid_utf8() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("filters.bin");
    std::fs::write(&path, [0xFFu8, b'\n']).expect("write invalid bytes");

    let patterns =
        load_filter_file_patterns(path.as_path()).expect("load filter patterns succeeds");

    assert_eq!(patterns, vec!["\u{fffd}".to_owned()]);
}

#[test]
fn load_filter_file_patterns_reads_from_stdin() {
    super::set_filter_stdin_input(b"keep\n# comment\n\ninclude\n".to_vec());
    let patterns = super::load_filter_file_patterns(Path::new("-")).expect("load stdin patterns");

    assert_eq!(patterns, vec!["keep".to_owned(), "include".to_owned()]);
}
