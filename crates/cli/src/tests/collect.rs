use super::common::*;
use super::*;

#[test]
fn collect_filter_arguments_merges_shortcuts_with_filters() {
    use std::ffi::OsString;

    let filters = vec![OsString::from("+ foo"), OsString::from("- bar")];
    let filter_indices = vec![5_usize, 9_usize];
    let rsync_indices = vec![1_usize, 7_usize];

    let merged = collect_filter_arguments(&filters, &filter_indices, &rsync_indices);

    assert_eq!(
        merged,
        vec![
            OsString::from("dir-merge /.rsync-filter"),
            OsString::from("exclude .rsync-filter"),
            OsString::from("+ foo"),
            OsString::from("dir-merge .rsync-filter"),
            OsString::from("- bar"),
        ]
    );
}

#[test]
fn collect_filter_arguments_handles_shortcuts_without_filters() {
    use std::ffi::OsString;

    let filters: Vec<OsString> = Vec::new();
    let filter_indices: Vec<usize> = Vec::new();
    let merged = collect_filter_arguments(&filters, &filter_indices, &[2_usize, 4_usize]);

    assert_eq!(
        merged,
        vec![
            OsString::from("dir-merge /.rsync-filter"),
            OsString::from("exclude .rsync-filter"),
            OsString::from("dir-merge .rsync-filter"),
        ]
    );
}
