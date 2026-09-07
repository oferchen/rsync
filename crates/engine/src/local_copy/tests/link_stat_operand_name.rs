// The name a failed `link_stat` reports for a source operand.
//
// Upstream renders it through `full_fname()` (util1.c:1433-1464), which puts
// `curr_dir` in front of every relative `fn`, so a missing relative operand is
// reported by its absolute path. `curr_dir` is upstream's own *string* - built
// by `push_dir()` appending the operand's `dir` half and cleaning it lexically -
// so `..` and `.` collapse without the kernel being consulted and a symlinked
// component keeps the spelling the operator typed.
//
// Measured against rsync 3.5.0 from `/tmp/t1158` (all `-a <operand> dst/`):
//
// ```text
// nope        -> rsync: [sender] link_stat "/tmp/t1158/nope" failed: ...
// sub/nope    -> rsync: [sender] link_stat "/tmp/t1158/sub/nope" failed: ...
// ./nope      -> rsync: [sender] link_stat "/tmp/t1158/nope" failed: ...
// ../nope     -> rsync: [sender] link_stat "/tmp/t1158/nope" failed: ...  (from sub/)
// link/nope   -> rsync: [sender] link_stat "/tmp/t1158/link/nope" failed: ...
// /tmp/t1158/nope -> rsync: [sender] link_stat "/tmp/t1158/nope" failed: ...
// ```

#[test]
fn a_relative_operand_is_named_by_its_absolute_path() {
    let working_dir = std::env::current_dir().expect("current dir");

    assert_eq!(
        operand_diagnostic_name(Path::new("nope")),
        working_dir.join("nope"),
        "upstream prefixes a relative `fn` with `curr_dir` (util1.c:1445-1452)"
    );
    assert_eq!(
        operand_diagnostic_name(Path::new("sub/nope")),
        working_dir.join("sub/nope"),
        "the operand's `dir` half is pushed onto `curr_dir`, not dropped"
    );
}

#[test]
fn dot_and_dot_dot_collapse_the_way_push_dir_cleans_curr_dir() {
    let working_dir = std::env::current_dir().expect("current dir");

    assert_eq!(
        operand_diagnostic_name(Path::new("./nope")),
        working_dir.join("nope"),
        "`push_dir(\".\")` leaves `curr_dir` where it was"
    );
    assert_eq!(
        operand_diagnostic_name(Path::new("sub/../nope")),
        working_dir.join("nope"),
        "`clean_fname()` cancels the component before a `..`"
    );
}

#[test]
fn an_absolute_operand_is_named_unchanged() {
    // Upstream's `*fn == '/'` branch adds no prefix, and the `dir`/`fn` split
    // reaches the same string for an absolute operand. This is the arm that
    // already matched upstream before the working directory was consulted, so
    // it is the negative control for the two tests above.
    let absolute = Path::new("/no/such/entry");
    assert_eq!(operand_diagnostic_name(absolute), absolute.to_path_buf());
}
