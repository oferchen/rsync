use super::prelude::*;


#[cfg(unix)]
#[test]
fn run_client_sparse_copy_creates_holes() {
    use std::os::unix::fs::MetadataExt;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("sparse-source.bin");
    let mut source_file = fs::File::create(&source).expect("create source");
    source_file.write_all(&[0x11]).expect("write leading");
    source_file
        .seek(SeekFrom::Start(1024 * 1024))
        .expect("seek to hole");
    source_file.write_all(&[0x22]).expect("write middle");
    source_file
        .seek(SeekFrom::Start(4 * 1024 * 1024))
        .expect("seek to tail");
    source_file.write_all(&[0x33]).expect("write tail");
    source_file.set_len(6 * 1024 * 1024).expect("extend source");

    let dense_dest = tmp.path().join("dense.bin");
    let sparse_dest = tmp.path().join("sparse.bin");

    let dense_config = ClientConfig::builder()
        .transfer_args([
            source.clone().into_os_string(),
            dense_dest.clone().into_os_string(),
        ])
        .permissions(true)
        .times(true)
        .build();
    let summary = run_client(dense_config).expect("dense copy succeeds");
    assert!(summary.events().is_empty());

    let sparse_config = ClientConfig::builder()
        .transfer_args([
            source.into_os_string(),
            sparse_dest.clone().into_os_string(),
        ])
        .permissions(true)
        .times(true)
        .sparse(true)
        .build();
    let summary = run_client(sparse_config).expect("sparse copy succeeds");
    assert!(summary.events().is_empty());

    let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");

    assert_eq!(dense_meta.len(), sparse_meta.len());
    assert!(sparse_meta.blocks() < dense_meta.blocks());
}

