use super::common::*;
use super::*;

/// `-F` shortcuts interleave with `--filter`/`-f` rules at their argv position.
///
/// upstream: options.c:1589-1598 - the first `-F` adds `dir-merge
/// /.rsync-filter`, the second adds `exclude .rsync-filter`; each is inserted
/// where the flag appears on the command line.
#[test]
fn short_f_shortcuts_interleave_with_filters_in_argv_order() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-F"),
        OsString::from("--filter"),
        OsString::from("+ foo"),
        OsString::from("-F"),
        OsString::from("--filter"),
        OsString::from("- bar"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(
        parsed.filters,
        vec![
            OsString::from("dir-merge /.rsync-filter"),
            OsString::from("+ foo"),
            OsString::from("exclude .rsync-filter"),
            OsString::from("- bar"),
        ]
    );
}

/// Two `-F` flags with no explicit `--filter` rules still expand to the
/// dir-merge and exclude directives in occurrence order.
#[test]
fn short_f_shortcuts_expand_without_explicit_filters() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-F"),
        OsString::from("-F"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(
        parsed.filters,
        vec![
            OsString::from("dir-merge /.rsync-filter"),
            OsString::from("exclude .rsync-filter"),
        ]
    );
}
