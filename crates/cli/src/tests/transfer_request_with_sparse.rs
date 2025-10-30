use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn transfer_request_with_sparse_preserves_holes() {
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.bin");
    let mut source_file = std::fs::File::create(&source).expect("create source");
    source_file.write_all(&[0x10]).expect("write leading byte");
    source_file
        .seek(SeekFrom::Start(1024 * 1024))
        .expect("seek to hole");
    source_file.write_all(&[0x20]).expect("write trailing byte");
    source_file.set_len(3 * 1024 * 1024).expect("extend source");

    let dense_dest = tmp.path().join("dense.bin");
    let sparse_dest = tmp.path().join("sparse.bin");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--sparse"),
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let dense_meta = std::fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = std::fs::metadata(&sparse_dest).expect("sparse metadata");

    assert_eq!(dense_meta.len(), sparse_meta.len());
    assert!(sparse_meta.blocks() < dense_meta.blocks());
}
