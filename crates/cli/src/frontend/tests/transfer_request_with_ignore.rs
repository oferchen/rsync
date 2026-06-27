use super::common::*;
use super::*;

#[test]
fn transfer_request_with_ignore_existing_leaves_destination_unchanged() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&source, b"updated").expect("write source");
    std::fs::write(&destination, b"original").expect("write destination");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--ignore-existing"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"original"
    );
}

#[test]
fn ignore_existing_verbose_reports_exists_in_upstream_format() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src_dir = tmp.path().join("src");
    let dst_dir = tmp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");
    std::fs::write(src_dir.join("keep.txt"), b"updated").expect("write src");
    std::fs::write(dst_dir.join("keep.txt"), b"original").expect("write dst");

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-rvv"),
        OsString::from("--ignore-existing"),
        {
            let mut p = src_dir.into_os_string();
            p.push("/");
            p
        },
        dst_dir.clone().into_os_string(),
    ]);
    let out = String::from_utf8_lossy(&stdout);

    assert_eq!(code, 0, "transfer should succeed: {out}");
    // upstream: generator.c:1409 - `rprintf(FINFO, "%s exists\n", fname)`.
    assert!(
        out.lines().any(|l| l == "keep.txt exists"),
        "expected upstream `keep.txt exists` line: {out}"
    );
    // The oc-invented `skipping existing file "..."` wording must be gone.
    assert!(
        !out.contains("skipping existing file"),
        "must not emit the non-upstream `skipping existing file` wording: {out}"
    );
    assert_eq!(
        std::fs::read(dst_dir.join("keep.txt")).expect("read dst"),
        b"original",
        "ignored-existing file must keep its original content"
    );
}

#[test]
fn transfer_request_with_ignore_missing_args_skips_missing_sources() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let missing = tmp.path().join("missing.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&destination, b"existing").expect("write destination");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--ignore-missing-args"),
        missing.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"existing"
    );
}
