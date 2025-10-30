use super::prelude::*;


#[test]
fn run_client_skip_compress_disables_compression_for_matching_suffix() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("archive.gz");
    let destination = tmp.path().join("dest.gz");
    let payload = vec![b'X'; 16 * 1024];
    fs::write(&source, &payload).expect("write source");

    let skip = SkipCompressList::parse("gz").expect("parse list");
    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .compress(true)
        .skip_compress(skip)
        .build();

    let summary = run_client(config).expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), payload);
    assert!(!summary.compression_used());
    assert!(summary.compressed_bytes().is_none());
}

