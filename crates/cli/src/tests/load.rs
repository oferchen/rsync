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

    assert_eq!(
        patterns,
        vec![" include ".to_string(), "pattern".to_string()]
    );
}

#[test]
fn load_filter_file_patterns_skip_semicolon_comments() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("filters-semicolon.txt");
    std::fs::write(&path, b"; leading comment\n  ; spaced comment\nkeep\n").expect("write filters");

    let patterns =
        load_filter_file_patterns(path.as_path()).expect("load filter patterns succeeds");

    assert_eq!(patterns, vec!["keep".to_string()]);
}

#[test]
fn load_filter_file_patterns_handles_invalid_utf8() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("filters.bin");
    std::fs::write(&path, [0xFFu8, b'\n']).expect("write invalid bytes");

    let patterns =
        load_filter_file_patterns(path.as_path()).expect("load filter patterns succeeds");

    assert_eq!(patterns, vec!["\u{fffd}".to_string()]);
}

#[test]
fn load_filter_file_patterns_reads_from_stdin() {
    super::set_filter_stdin_input(b"keep\n# comment\n\ninclude\n".to_vec());
    let patterns = super::load_filter_file_patterns(Path::new("-")).expect("load stdin patterns");

    assert_eq!(patterns, vec!["keep".to_string(), "include".to_string()]);
}
