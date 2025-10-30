use super::prelude::*;


#[test]
fn run_client_with_compress_records_compressed_bytes() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.bin");
    let destination = tmp.path().join("dest.bin");
    let payload = vec![b'Z'; 32 * 1024];
    fs::write(&source, &payload).expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .compress(true)
        .build();

    let summary = run_client(config).expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), payload);
    assert!(summary.compression_used());
    let compressed = summary
        .compressed_bytes()
        .expect("compressed bytes recorded");
    assert!(compressed > 0);
    assert!(compressed <= summary.bytes_copied());
}

