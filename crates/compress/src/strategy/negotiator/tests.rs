//! Cross-cutting tests spanning multiple negotiator implementations.

use super::*;

#[test]
fn negotiators_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<DefaultCompressionNegotiator>();
    assert_send_sync::<FixedCompressionNegotiator>();
}

#[test]
fn trait_object_works() {
    let negotiator: Box<dyn CompressionNegotiator> = Box::new(DefaultCompressionNegotiator::new());
    let supported = negotiator.supported_algorithms();
    assert!(supported.contains(&"zlib"));

    let selected = negotiator.select_algorithm(&["zlib"], false);
    assert_eq!(selected, "zlib");
}

#[test]
fn trait_object_fixed() {
    let negotiator: Box<dyn CompressionNegotiator> =
        Box::new(FixedCompressionNegotiator::new("zlib"));
    let selected = negotiator.select_algorithm(&["zstd", "zlib"], false);
    assert_eq!(selected, "zlib");
}
