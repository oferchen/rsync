use super::prelude::*;


#[test]
fn builder_applies_checksum_seed_to_signature_algorithm() {
    let choice = StrongChecksumChoice::parse("xxh64").expect("checksum choice parsed");
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .checksum_choice(choice)
        .checksum_seed(Some(7))
        .build();

    assert_eq!(config.checksum_seed(), Some(7));
    match config.checksum_signature_algorithm() {
        SignatureAlgorithm::Xxh64 { seed } => assert_eq!(seed, 7),
        other => panic!("unexpected signature algorithm: {other:?}"),
    }
}

