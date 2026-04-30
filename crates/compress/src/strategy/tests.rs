use super::*;
use crate::zlib::CompressionLevel;

const TEST_DATA: &[u8] = b"The quick brown fox jumps over the lazy dog. \
                            This is test data for compression algorithms.";
const COMPRESSIBLE_DATA: &[u8] = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
                                    bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\
                                    cccccccccccccccccccccccccccccccccccccc";

#[test]
fn algorithm_kind_name() {
    assert_eq!(CompressionAlgorithmKind::None.name(), "none");
    assert_eq!(CompressionAlgorithmKind::Zlib.name(), "zlib");
    #[cfg(feature = "zstd")]
    assert_eq!(CompressionAlgorithmKind::Zstd.name(), "zstd");
    #[cfg(feature = "lz4")]
    assert_eq!(CompressionAlgorithmKind::Lz4.name(), "lz4");
}

#[test]
fn algorithm_kind_is_available() {
    assert!(CompressionAlgorithmKind::None.is_available());
    assert!(CompressionAlgorithmKind::Zlib.is_available());
}

#[test]
fn algorithm_kind_default_level() {
    assert_eq!(
        CompressionAlgorithmKind::None.default_level(),
        CompressionLevel::None
    );
    assert_eq!(
        CompressionAlgorithmKind::Zlib.default_level(),
        CompressionLevel::Default
    );
}

#[test]
fn algorithm_kind_from_name() {
    assert_eq!(
        CompressionAlgorithmKind::from_name("none"),
        Some(CompressionAlgorithmKind::None)
    );
    assert_eq!(
        CompressionAlgorithmKind::from_name("zlib"),
        Some(CompressionAlgorithmKind::Zlib)
    );
    assert_eq!(
        CompressionAlgorithmKind::from_name("ZLIB"),
        Some(CompressionAlgorithmKind::Zlib)
    );
    #[cfg(feature = "zstd")]
    assert_eq!(
        CompressionAlgorithmKind::from_name("zstd"),
        Some(CompressionAlgorithmKind::Zstd)
    );
    assert_eq!(CompressionAlgorithmKind::from_name("invalid"), None);
}

#[test]
fn algorithm_kind_all() {
    let all = CompressionAlgorithmKind::all();
    assert!(!all.is_empty());
    assert!(all.contains(&CompressionAlgorithmKind::None));
    assert!(all.contains(&CompressionAlgorithmKind::Zlib));
}

#[test]
fn algorithm_kind_for_protocol_version() {
    // Pre-30 always defaults to Zlib (upstream: compat.c:556-563 - no
    // vstring negotiation, fallback is "zlib").
    assert_eq!(
        CompressionAlgorithmKind::for_protocol_version(28),
        CompressionAlgorithmKind::Zlib
    );
    assert_eq!(
        CompressionAlgorithmKind::for_protocol_version(29),
        CompressionAlgorithmKind::Zlib
    );

    // Protocol 30+ prefers Zstd when the zstd feature is compiled in
    // (upstream: compat.c:101-102 valid_compressions_items[]). Without
    // the feature the default falls back to Zlib.
    #[cfg(feature = "zstd")]
    {
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(30),
            CompressionAlgorithmKind::Zstd
        );
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(32),
            CompressionAlgorithmKind::Zstd
        );
    }
    #[cfg(not(feature = "zstd"))]
    {
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(30),
            CompressionAlgorithmKind::Zlib
        );
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(32),
            CompressionAlgorithmKind::Zlib
        );
    }
}

#[test]
fn no_compression_strategy_roundtrip() {
    let strategy = NoCompressionStrategy::new();
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    let comp_bytes = strategy.compress(TEST_DATA, &mut compressed).unwrap();
    assert_eq!(comp_bytes, TEST_DATA.len());
    assert_eq!(&compressed, TEST_DATA);

    let decomp_bytes = strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(decomp_bytes, TEST_DATA.len());
    assert_eq!(&decompressed, TEST_DATA);
}

#[test]
fn no_compression_strategy_algorithm_name() {
    let strategy = NoCompressionStrategy::new();
    assert_eq!(strategy.algorithm_name(), "none");
    assert_eq!(strategy.algorithm_kind(), CompressionAlgorithmKind::None);
}

#[test]
fn zlib_strategy_compress_decompress() {
    let strategy = ZlibStrategy::new(CompressionLevel::Default);
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    let comp_bytes = strategy.compress(TEST_DATA, &mut compressed).unwrap();
    assert!(comp_bytes > 0);
    assert!(comp_bytes < TEST_DATA.len());

    let decomp_bytes = strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(decomp_bytes, TEST_DATA.len());
    assert_eq!(&decompressed, TEST_DATA);
}

#[test]
fn zlib_strategy_algorithm_name() {
    let strategy = ZlibStrategy::with_default_level();
    assert_eq!(strategy.algorithm_name(), "zlib");
    assert_eq!(strategy.algorithm_kind(), CompressionAlgorithmKind::Zlib);
}

#[test]
fn zlib_strategy_different_levels() {
    let fast = ZlibStrategy::new(CompressionLevel::Fast);
    let best = ZlibStrategy::new(CompressionLevel::Best);

    let mut fast_out = Vec::new();
    let mut best_out = Vec::new();

    fast.compress(COMPRESSIBLE_DATA, &mut fast_out).unwrap();
    best.compress(COMPRESSIBLE_DATA, &mut best_out).unwrap();

    // Best should generally produce smaller or similar output
    assert!(best_out.len() <= fast_out.len() + 5);

    // Both should decompress correctly
    let mut fast_decompressed = Vec::new();
    let mut best_decompressed = Vec::new();
    fast.decompress(&fast_out, &mut fast_decompressed).unwrap();
    best.decompress(&best_out, &mut best_decompressed).unwrap();
    assert_eq!(&fast_decompressed, COMPRESSIBLE_DATA);
    assert_eq!(&best_decompressed, COMPRESSIBLE_DATA);
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_strategy_compress_decompress() {
    let strategy = ZstdStrategy::new(CompressionLevel::Default);
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    let comp_bytes = strategy.compress(TEST_DATA, &mut compressed).unwrap();
    assert!(comp_bytes > 0);

    let decomp_bytes = strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(decomp_bytes, TEST_DATA.len());
    assert_eq!(&decompressed, TEST_DATA);
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_strategy_algorithm_name() {
    let strategy = ZstdStrategy::with_default_level();
    assert_eq!(strategy.algorithm_name(), "zstd");
    assert_eq!(strategy.algorithm_kind(), CompressionAlgorithmKind::Zstd);
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_strategy_compress_decompress() {
    let strategy = Lz4Strategy::new(CompressionLevel::Default);
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    let comp_bytes = strategy.compress(TEST_DATA, &mut compressed).unwrap();
    assert!(comp_bytes > 0);

    let decomp_bytes = strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(decomp_bytes, TEST_DATA.len());
    assert_eq!(&decompressed, TEST_DATA);
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_strategy_algorithm_name() {
    let strategy = Lz4Strategy::with_default_level();
    assert_eq!(strategy.algorithm_name(), "lz4");
    assert_eq!(strategy.algorithm_kind(), CompressionAlgorithmKind::Lz4);
}

#[test]
fn selector_for_protocol_version_29_returns_zlib() {
    let strategy = CompressionStrategySelector::for_protocol_version(29);
    assert_eq!(strategy.algorithm_name(), "zlib");
}

#[test]
fn selector_for_protocol_version_30_modern_default() {
    let strategy = CompressionStrategySelector::for_protocol_version(30);
    #[cfg(feature = "zstd")]
    assert_eq!(strategy.algorithm_name(), "zstd");
    #[cfg(not(feature = "zstd"))]
    assert_eq!(strategy.algorithm_name(), "zlib");
}

#[test]
fn selector_for_algorithm_zlib() {
    let strategy = CompressionStrategySelector::for_algorithm(
        CompressionAlgorithmKind::Zlib,
        CompressionLevel::Best,
    )
    .unwrap();
    assert_eq!(strategy.algorithm_name(), "zlib");
}

#[test]
fn selector_for_algorithm_none() {
    let strategy = CompressionStrategySelector::for_algorithm(
        CompressionAlgorithmKind::None,
        CompressionLevel::None,
    )
    .unwrap();
    assert_eq!(strategy.algorithm_name(), "none");
}

#[test]
fn selector_negotiate_finds_common_algorithm() {
    #[allow(unused_mut)]
    let mut local = vec![
        CompressionAlgorithmKind::Zlib,
        CompressionAlgorithmKind::None,
    ];
    #[cfg(feature = "zstd")]
    local.insert(0, CompressionAlgorithmKind::Zstd);

    #[allow(unused_mut)]
    let mut remote = vec![CompressionAlgorithmKind::Zlib];
    #[cfg(feature = "lz4")]
    remote.insert(0, CompressionAlgorithmKind::Lz4);

    let strategy =
        CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);
    assert_eq!(strategy.algorithm_name(), "zlib");
}

#[test]
fn selector_negotiate_no_match_returns_none() {
    let local = vec![CompressionAlgorithmKind::Zlib];
    let remote = vec![CompressionAlgorithmKind::None];

    let strategy =
        CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);
    assert_eq!(strategy.algorithm_name(), "none");
}

#[test]
fn selector_concrete_factories() {
    let none = CompressionStrategySelector::none();
    assert_eq!(none.algorithm_name(), "none");

    let zlib = CompressionStrategySelector::zlib_default();
    assert_eq!(zlib.algorithm_name(), "zlib");

    #[cfg(feature = "zstd")]
    {
        let zstd = CompressionStrategySelector::zstd_default();
        assert_eq!(zstd.algorithm_name(), "zstd");
    }

    #[cfg(feature = "lz4")]
    {
        let lz4 = CompressionStrategySelector::lz4_default();
        assert_eq!(lz4.algorithm_name(), "lz4");
    }
}

#[test]
fn strategies_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<NoCompressionStrategy>();
    assert_send_sync::<ZlibStrategy>();
    #[cfg(feature = "zstd")]
    assert_send_sync::<ZstdStrategy>();
    #[cfg(feature = "lz4")]
    assert_send_sync::<Lz4Strategy>();
}

#[test]
fn boxed_strategy_works() {
    let strategies: Vec<Box<dyn CompressionStrategy>> = vec![
        Box::new(NoCompressionStrategy::new()),
        Box::new(ZlibStrategy::with_default_level()),
    ];

    for strategy in &strategies {
        let mut compressed = Vec::new();
        let mut decompressed = Vec::new();

        strategy.compress(TEST_DATA, &mut compressed).unwrap();
        strategy.decompress(&compressed, &mut decompressed).unwrap();
        assert_eq!(&decompressed, TEST_DATA);
    }
}

#[test]
fn empty_input_produces_valid_output() {
    let strategy = ZlibStrategy::with_default_level();
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    strategy.compress(b"", &mut compressed).unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(&decompressed, b"");
}

#[test]
fn consistent_results_across_calls() {
    let strategy = ZlibStrategy::new(CompressionLevel::Default);
    let mut out1 = Vec::new();
    let mut out2 = Vec::new();

    strategy.compress(TEST_DATA, &mut out1).unwrap();
    strategy.compress(TEST_DATA, &mut out2).unwrap();
    assert_eq!(out1, out2);
}

#[test]
fn selector_protocol_version_0_returns_zlib() {
    let strategy = CompressionStrategySelector::for_protocol_version(0);
    assert_eq!(strategy.algorithm_name(), "zlib");
}

#[test]
fn selector_protocol_version_28_returns_zlib() {
    let strategy = CompressionStrategySelector::for_protocol_version(28);
    assert_eq!(strategy.algorithm_name(), "zlib");
}

#[test]
fn selector_protocol_version_legacy_below_30_always_zlib() {
    // Pre-30 has no vstring negotiation; upstream defaults to "zlib".
    for v in 0..30u8 {
        let strategy = CompressionStrategySelector::for_protocol_version(v);
        assert_eq!(
            strategy.algorithm_name(),
            "zlib",
            "protocol {v} must default to zlib"
        );
    }
}

#[cfg(feature = "zstd")]
#[test]
fn selector_protocol_version_modern_30_plus_zstd() {
    // Protocol >= 30 with zstd compiled in: the canonical default kind is
    // zstd (upstream: compat.c:101-102 valid_compressions_items[]).
    for v in [30u8, 31, 32, 40, 100, 255] {
        let strategy = CompressionStrategySelector::for_protocol_version(v);
        assert_eq!(
            strategy.algorithm_name(),
            "zstd",
            "protocol {v} must default to zstd when feature enabled"
        );
    }
}

#[cfg(not(feature = "zstd"))]
#[test]
fn selector_protocol_version_modern_30_plus_zlib_without_zstd() {
    for v in [30u8, 31, 32, 40, 100, 255] {
        let strategy = CompressionStrategySelector::for_protocol_version(v);
        assert_eq!(
            strategy.algorithm_name(),
            "zlib",
            "protocol {v} must fall back to zlib without zstd feature"
        );
    }
}

#[test]
fn kind_for_protocol_below_30_always_zlib() {
    for v in 0..30u8 {
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(v),
            CompressionAlgorithmKind::Zlib,
            "protocol version {v} should default to zlib"
        );
    }
}

#[cfg(feature = "zstd")]
#[test]
fn kind_for_protocol_30_and_above_zstd() {
    // Upstream: compat.c:101-102 - zstd is the first preference whenever
    // SUPPORT_ZSTD is defined and vstring negotiation is available
    // (protocol >= 30).
    for v in [30u8, 31, 32, 40, 100, 255] {
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(v),
            CompressionAlgorithmKind::Zstd,
            "protocol version {v} should default to zstd when feature enabled"
        );
    }
}

#[cfg(not(feature = "zstd"))]
#[test]
fn kind_for_protocol_30_and_above_falls_to_zlib_without_zstd() {
    for v in [30u8, 31, 32, 100, 255] {
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(v),
            CompressionAlgorithmKind::Zlib,
            "protocol version {v} should fall back to zlib without zstd feature"
        );
    }
}

#[cfg(feature = "zstd")]
#[test]
fn negotiate_zstd_preferred_when_both_support() {
    let local = vec![
        CompressionAlgorithmKind::Zstd,
        CompressionAlgorithmKind::Zlib,
        CompressionAlgorithmKind::None,
    ];
    let remote = vec![
        CompressionAlgorithmKind::Zstd,
        CompressionAlgorithmKind::Zlib,
    ];
    let strategy =
        CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);
    assert_eq!(strategy.algorithm_name(), "zstd");
}

#[cfg(feature = "zstd")]
#[test]
fn negotiate_falls_to_zlib_when_remote_lacks_zstd() {
    let local = vec![
        CompressionAlgorithmKind::Zstd,
        CompressionAlgorithmKind::Zlib,
    ];
    let remote = vec![CompressionAlgorithmKind::Zlib];
    let strategy =
        CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);
    assert_eq!(strategy.algorithm_name(), "zlib");
}

#[cfg(feature = "lz4")]
#[test]
fn negotiate_lz4_when_both_support() {
    let local = vec![
        CompressionAlgorithmKind::Lz4,
        CompressionAlgorithmKind::Zlib,
    ];
    let remote = vec![
        CompressionAlgorithmKind::Lz4,
        CompressionAlgorithmKind::Zlib,
    ];
    let strategy =
        CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);
    assert_eq!(strategy.algorithm_name(), "lz4");
}

#[cfg(feature = "lz4")]
#[test]
fn negotiate_lz4_falls_to_zlib_when_remote_lacks_lz4() {
    let local = vec![
        CompressionAlgorithmKind::Lz4,
        CompressionAlgorithmKind::Zlib,
    ];
    let remote = vec![CompressionAlgorithmKind::Zlib];
    let strategy =
        CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);
    assert_eq!(strategy.algorithm_name(), "zlib");
}

#[test]
fn negotiate_empty_local_returns_none() {
    let local: Vec<CompressionAlgorithmKind> = vec![];
    let remote = vec![CompressionAlgorithmKind::Zlib];
    let strategy =
        CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);
    assert_eq!(strategy.algorithm_name(), "none");
}

#[test]
fn negotiate_empty_remote_returns_none() {
    let local = vec![CompressionAlgorithmKind::Zlib];
    let remote: Vec<CompressionAlgorithmKind> = vec![];
    let strategy =
        CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);
    assert_eq!(strategy.algorithm_name(), "none");
}

#[test]
fn negotiate_both_empty_returns_none() {
    let local: Vec<CompressionAlgorithmKind> = vec![];
    let remote: Vec<CompressionAlgorithmKind> = vec![];
    let strategy =
        CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);
    assert_eq!(strategy.algorithm_name(), "none");
}

#[test]
fn negotiate_only_none_kind() {
    let local = vec![CompressionAlgorithmKind::None];
    let remote = vec![CompressionAlgorithmKind::None];
    let strategy =
        CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);
    assert_eq!(strategy.algorithm_name(), "none");
}

#[test]
fn kind_from_name_zlibx_maps_to_zlib() {
    // "zlibx" is an alias for zlib in from_name
    let kind = CompressionAlgorithmKind::from_name("zlibx");
    assert_eq!(kind, Some(CompressionAlgorithmKind::Zlib));
}

#[test]
fn kind_from_name_deflate_maps_to_zlib() {
    let kind = CompressionAlgorithmKind::from_name("deflate");
    assert_eq!(kind, Some(CompressionAlgorithmKind::Zlib));
}

#[test]
fn kind_from_name_case_insensitive() {
    assert_eq!(
        CompressionAlgorithmKind::from_name("NONE"),
        Some(CompressionAlgorithmKind::None)
    );
    assert_eq!(
        CompressionAlgorithmKind::from_name("Zlib"),
        Some(CompressionAlgorithmKind::Zlib)
    );
    assert_eq!(
        CompressionAlgorithmKind::from_name("ZLibX"),
        Some(CompressionAlgorithmKind::Zlib)
    );
}

#[cfg(feature = "zstd")]
#[test]
fn kind_from_name_zstandard_alias() {
    assert_eq!(
        CompressionAlgorithmKind::from_name("zstandard"),
        Some(CompressionAlgorithmKind::Zstd)
    );
}

#[test]
fn kind_display() {
    assert_eq!(format!("{}", CompressionAlgorithmKind::None), "none");
    assert_eq!(format!("{}", CompressionAlgorithmKind::Zlib), "zlib");
}

#[cfg(feature = "zstd")]
#[test]
fn kind_display_zstd() {
    assert_eq!(format!("{}", CompressionAlgorithmKind::Zstd), "zstd");
}

#[cfg(feature = "lz4")]
#[test]
fn kind_display_lz4() {
    assert_eq!(format!("{}", CompressionAlgorithmKind::Lz4), "lz4");
}

#[cfg(feature = "zstd")]
#[test]
fn kind_all_contains_zstd() {
    let all = CompressionAlgorithmKind::all();
    assert!(all.contains(&CompressionAlgorithmKind::Zstd));
}

#[cfg(feature = "lz4")]
#[test]
fn kind_all_contains_lz4() {
    let all = CompressionAlgorithmKind::all();
    assert!(all.contains(&CompressionAlgorithmKind::Lz4));
}

#[cfg(feature = "zstd")]
#[test]
fn selector_for_algorithm_zstd() {
    let strategy = CompressionStrategySelector::for_algorithm(
        CompressionAlgorithmKind::Zstd,
        CompressionLevel::Default,
    )
    .unwrap();
    assert_eq!(strategy.algorithm_name(), "zstd");
}

#[cfg(feature = "lz4")]
#[test]
fn selector_for_algorithm_lz4() {
    let strategy = CompressionStrategySelector::for_algorithm(
        CompressionAlgorithmKind::Lz4,
        CompressionLevel::Default,
    )
    .unwrap();
    assert_eq!(strategy.algorithm_name(), "lz4");
}

#[test]
fn negotiate_local_preference_order_wins() {
    // When local lists Zlib before None, and remote has both,
    // Zlib should be selected (not None).
    let local = vec![
        CompressionAlgorithmKind::Zlib,
        CompressionAlgorithmKind::None,
    ];
    let remote = vec![
        CompressionAlgorithmKind::None,
        CompressionAlgorithmKind::Zlib,
    ];
    let strategy =
        CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);
    assert_eq!(strategy.algorithm_name(), "zlib");
}

#[test]
fn selector_for_protocol_version_roundtrip() {
    for version in [0, 28, 29, 30, 31, 32, 35, 36, 255] {
        let strategy = CompressionStrategySelector::for_protocol_version(version);
        let mut compressed = Vec::new();
        let mut decompressed = Vec::new();

        strategy.compress(TEST_DATA, &mut compressed).unwrap();
        strategy.decompress(&compressed, &mut decompressed).unwrap();
        assert_eq!(
            &decompressed, TEST_DATA,
            "roundtrip failed for protocol version {version}"
        );
    }
}

#[test]
fn selector_for_algorithm_roundtrip_zlib_fast() {
    let strategy = CompressionStrategySelector::for_algorithm(
        CompressionAlgorithmKind::Zlib,
        CompressionLevel::Fast,
    )
    .unwrap();
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    strategy.compress(TEST_DATA, &mut compressed).unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(&decompressed, TEST_DATA);
}

#[test]
fn selector_for_algorithm_roundtrip_zlib_best() {
    let strategy = CompressionStrategySelector::for_algorithm(
        CompressionAlgorithmKind::Zlib,
        CompressionLevel::Best,
    )
    .unwrap();
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    strategy
        .compress(COMPRESSIBLE_DATA, &mut compressed)
        .unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(&decompressed, COMPRESSIBLE_DATA);
}

#[test]
fn selector_for_algorithm_roundtrip_none() {
    let strategy = CompressionStrategySelector::for_algorithm(
        CompressionAlgorithmKind::None,
        CompressionLevel::None,
    )
    .unwrap();
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    strategy.compress(TEST_DATA, &mut compressed).unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(&decompressed, TEST_DATA);
}

#[cfg(feature = "zstd")]
#[test]
fn selector_for_algorithm_roundtrip_zstd() {
    let strategy = CompressionStrategySelector::for_algorithm(
        CompressionAlgorithmKind::Zstd,
        CompressionLevel::Fast,
    )
    .unwrap();
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    strategy.compress(TEST_DATA, &mut compressed).unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(&decompressed, TEST_DATA);
}

#[cfg(feature = "lz4")]
#[test]
fn selector_for_algorithm_roundtrip_lz4() {
    let strategy = CompressionStrategySelector::for_algorithm(
        CompressionAlgorithmKind::Lz4,
        CompressionLevel::Default,
    )
    .unwrap();
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    strategy.compress(TEST_DATA, &mut compressed).unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(&decompressed, TEST_DATA);
}

#[test]
fn selector_zlib_with_level() {
    let strategy = CompressionStrategySelector::zlib(CompressionLevel::Fast);
    assert_eq!(strategy.algorithm_name(), "zlib");

    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();
    strategy.compress(TEST_DATA, &mut compressed).unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(&decompressed, TEST_DATA);
}

#[cfg(feature = "zstd")]
#[test]
fn selector_zstd_with_level() {
    let strategy = CompressionStrategySelector::zstd(CompressionLevel::Best);
    assert_eq!(strategy.algorithm_name(), "zstd");

    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();
    strategy.compress(TEST_DATA, &mut compressed).unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(&decompressed, TEST_DATA);
}

#[cfg(feature = "lz4")]
#[test]
fn selector_lz4_with_level() {
    let strategy = CompressionStrategySelector::lz4(CompressionLevel::Fast);
    assert_eq!(strategy.algorithm_name(), "lz4");

    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();
    strategy.compress(TEST_DATA, &mut compressed).unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(&decompressed, TEST_DATA);
}

#[test]
fn negotiate_passes_level_to_strategy() {
    let local = vec![CompressionAlgorithmKind::Zlib];
    let remote = vec![CompressionAlgorithmKind::Zlib];

    let strategy = CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Best);
    assert_eq!(strategy.algorithm_name(), "zlib");

    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();
    strategy
        .compress(COMPRESSIBLE_DATA, &mut compressed)
        .unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(&decompressed, COMPRESSIBLE_DATA);
}

#[test]
fn negotiate_none_kind_roundtrip() {
    let local = vec![CompressionAlgorithmKind::None];
    let remote = vec![CompressionAlgorithmKind::None];

    let strategy =
        CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);

    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();
    strategy.compress(TEST_DATA, &mut compressed).unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(&decompressed, TEST_DATA);
}

#[test]
fn kind_for_protocol_version_boundary_29_is_zlib() {
    // Pre-30 always defaults to Zlib (upstream: compat.c:556-563).
    assert_eq!(
        CompressionAlgorithmKind::for_protocol_version(29),
        CompressionAlgorithmKind::Zlib
    );
}

#[test]
fn kind_clone_copy_eq_hash() {
    let a = CompressionAlgorithmKind::Zlib;
    let b = a;
    assert_eq!(a, b);

    let mut set = std::collections::HashSet::new();
    set.insert(a);
    assert!(set.contains(&b));
    assert!(!set.contains(&CompressionAlgorithmKind::None));
}

#[test]
fn kind_debug_format() {
    let debug = format!("{:?}", CompressionAlgorithmKind::Zlib);
    assert_eq!(debug, "Zlib");
    let debug = format!("{:?}", CompressionAlgorithmKind::None);
    assert_eq!(debug, "None");
}
