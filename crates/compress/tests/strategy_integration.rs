//! Integration tests for compression strategy pattern.
//!
//! These tests verify the Strategy pattern implementation for runtime
//! compression algorithm selection, including protocol version defaults,
//! negotiation, and cross-algorithm compatibility.

use compress::strategy::{
    CompressionAlgorithmKind, CompressionStrategy, CompressionStrategySelector,
    NoCompressionStrategy, ZlibStrategy,
};
use compress::zlib::CompressionLevel;

#[cfg(feature = "zstd")]
use compress::strategy::ZstdStrategy;

#[cfg(feature = "lz4")]
use compress::strategy::Lz4Strategy;

// ============================================================================
// Test Data
// ============================================================================

const EMPTY_DATA: &[u8] = b"";
const SMALL_DATA: &[u8] = b"Hello, World!";
const MEDIUM_DATA: &[u8] = b"The quick brown fox jumps over the lazy dog. \
                              This is a medium-sized test string that should \
                              compress reasonably well with most algorithms.";
const COMPRESSIBLE_DATA: &[u8] = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
                                    bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\
                                    cccccccccccccccccccccccccccccccccccccc\
                                    dddddddddddddddddddddddddddddddddddddd";
const INCOMPRESSIBLE_DATA: &[u8] = b"\x12\x34\x56\x78\x9a\xbc\xde\xf0\
                                      \x23\x45\x67\x89\xab\xcd\xef\x01";

// ============================================================================
// CompressionAlgorithmKind Tests
// ============================================================================

#[test]
fn algorithm_kind_names_are_canonical() {
    assert_eq!(CompressionAlgorithmKind::None.name(), "none");
    assert_eq!(CompressionAlgorithmKind::Zlib.name(), "zlib");
    #[cfg(feature = "zstd")]
    assert_eq!(CompressionAlgorithmKind::Zstd.name(), "zstd");
    #[cfg(feature = "lz4")]
    assert_eq!(CompressionAlgorithmKind::Lz4.name(), "lz4");
}

#[test]
fn algorithm_kind_from_name_case_insensitive() {
    assert_eq!(
        CompressionAlgorithmKind::from_name("ZLIB"),
        Some(CompressionAlgorithmKind::Zlib)
    );
    assert_eq!(
        CompressionAlgorithmKind::from_name("zlib"),
        Some(CompressionAlgorithmKind::Zlib)
    );
    assert_eq!(
        CompressionAlgorithmKind::from_name("ZLib"),
        Some(CompressionAlgorithmKind::Zlib)
    );
}

#[test]
fn algorithm_kind_from_name_accepts_aliases() {
    assert_eq!(
        CompressionAlgorithmKind::from_name("deflate"),
        Some(CompressionAlgorithmKind::Zlib)
    );
    assert_eq!(
        CompressionAlgorithmKind::from_name("zlibx"),
        Some(CompressionAlgorithmKind::Zlib)
    );
    #[cfg(feature = "zstd")]
    assert_eq!(
        CompressionAlgorithmKind::from_name("zstandard"),
        Some(CompressionAlgorithmKind::Zstd)
    );
}

#[test]
fn algorithm_kind_from_name_rejects_invalid() {
    assert_eq!(CompressionAlgorithmKind::from_name("brotli"), None);
    assert_eq!(CompressionAlgorithmKind::from_name("gzip"), None);
    assert_eq!(CompressionAlgorithmKind::from_name(""), None);
}

#[test]
fn algorithm_kind_availability() {
    // Always available
    assert!(CompressionAlgorithmKind::None.is_available());
    assert!(CompressionAlgorithmKind::Zlib.is_available());

    // Feature-dependent
    #[cfg(feature = "zstd")]
    assert!(CompressionAlgorithmKind::Zstd.is_available());
    #[cfg(feature = "lz4")]
    assert!(CompressionAlgorithmKind::Lz4.is_available());
}

#[test]
fn algorithm_kind_all_returns_available_algorithms() {
    let all = CompressionAlgorithmKind::all();
    assert!(!all.is_empty());
    assert!(all.contains(&CompressionAlgorithmKind::None));
    assert!(all.contains(&CompressionAlgorithmKind::Zlib));

    // Verify all returned algorithms are actually available
    for algo in &all {
        assert!(algo.is_available());
    }
}

#[test]
fn algorithm_kind_default_levels() {
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
fn algorithm_kind_for_protocol_version_follows_rsync_defaults() {
    // Protocol < 36 should use Zlib
    for version in 27..36 {
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(version),
            CompressionAlgorithmKind::Zlib,
            "Protocol version {} should default to Zlib",
            version
        );
    }

    // Protocol >= 36 should use Zstd (if available)
    #[cfg(feature = "zstd")]
    {
        for version in 36..40 {
            assert_eq!(
                CompressionAlgorithmKind::for_protocol_version(version),
                CompressionAlgorithmKind::Zstd,
                "Protocol version {} should default to Zstd",
                version
            );
        }
    }

    #[cfg(not(feature = "zstd"))]
    {
        for version in 36..40 {
            assert_eq!(
                CompressionAlgorithmKind::for_protocol_version(version),
                CompressionAlgorithmKind::Zlib,
                "Protocol version {} should fall back to Zlib when Zstd unavailable",
                version
            );
        }
    }
}

// ============================================================================
// NoCompressionStrategy Tests
// ============================================================================

#[test]
fn no_compression_passthrough() {
    let strategy = NoCompressionStrategy::new();
    let mut output = Vec::new();

    let bytes = strategy.compress(MEDIUM_DATA, &mut output).unwrap();
    assert_eq!(bytes, MEDIUM_DATA.len());
    assert_eq!(&output, MEDIUM_DATA);
}

#[test]
fn no_compression_roundtrip() {
    let strategy = NoCompressionStrategy::new();
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    strategy.compress(MEDIUM_DATA, &mut compressed).unwrap();
    strategy
        .decompress(&compressed, &mut decompressed)
        .unwrap();

    assert_eq!(&decompressed, MEDIUM_DATA);
}

#[test]
fn no_compression_empty_input() {
    let strategy = NoCompressionStrategy::new();
    let mut output = Vec::new();

    let bytes = strategy.compress(EMPTY_DATA, &mut output).unwrap();
    assert_eq!(bytes, 0);
    assert!(output.is_empty());
}

// ============================================================================
// ZlibStrategy Tests
// ============================================================================

#[test]
fn zlib_strategy_compress_and_decompress() {
    let strategy = ZlibStrategy::new(CompressionLevel::Default);
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    let comp_bytes = strategy.compress(MEDIUM_DATA, &mut compressed).unwrap();
    assert!(comp_bytes > 0);
    assert!(comp_bytes < MEDIUM_DATA.len()); // Should compress

    let decomp_bytes = strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(decomp_bytes, MEDIUM_DATA.len());
    assert_eq!(&decompressed, MEDIUM_DATA);
}

#[test]
fn zlib_strategy_different_compression_levels() {
    let data = COMPRESSIBLE_DATA;

    let none = ZlibStrategy::new(CompressionLevel::None);
    let fast = ZlibStrategy::new(CompressionLevel::Fast);
    let default = ZlibStrategy::new(CompressionLevel::Default);
    let best = ZlibStrategy::new(CompressionLevel::Best);

    let mut none_out = Vec::new();
    let mut fast_out = Vec::new();
    let mut default_out = Vec::new();
    let mut best_out = Vec::new();

    none.compress(data, &mut none_out).unwrap();
    fast.compress(data, &mut fast_out).unwrap();
    default.compress(data, &mut default_out).unwrap();
    best.compress(data, &mut best_out).unwrap();

    // None should produce larger output than compressed variants
    assert!(none_out.len() >= fast_out.len());
    assert!(none_out.len() >= default_out.len());
    assert!(none_out.len() >= best_out.len());

    // For small data, compression levels might produce similar or slightly different sizes
    // due to overhead. Just verify all produce valid compressed output that decompresses correctly.
    // Note: Higher compression doesn't always mean smaller output for small data.

    // All should decompress correctly
    for (strategy, compressed) in [
        (&none, &none_out),
        (&fast, &fast_out),
        (&default, &default_out),
        (&best, &best_out),
    ] {
        let mut decompressed = Vec::new();
        strategy.decompress(compressed, &mut decompressed).unwrap();
        assert_eq!(&decompressed, data);
    }
}

#[test]
fn zlib_strategy_empty_input() {
    let strategy = ZlibStrategy::with_default_level();
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    strategy.compress(EMPTY_DATA, &mut compressed).unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();

    assert_eq!(&decompressed, EMPTY_DATA);
}

#[test]
fn zlib_strategy_small_data() {
    let strategy = ZlibStrategy::new(CompressionLevel::Default);
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    strategy.compress(SMALL_DATA, &mut compressed).unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();

    assert_eq!(&decompressed, SMALL_DATA);
}

#[test]
fn zlib_strategy_incompressible_data() {
    let strategy = ZlibStrategy::new(CompressionLevel::Best);
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    strategy
        .compress(INCOMPRESSIBLE_DATA, &mut compressed)
        .unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();

    assert_eq!(&decompressed, INCOMPRESSIBLE_DATA);
    // Incompressible data might expand due to compression overhead
    assert!(compressed.len() >= INCOMPRESSIBLE_DATA.len() - 5); // Allow some tolerance
}

// ============================================================================
// ZstdStrategy Tests
// ============================================================================

#[cfg(feature = "zstd")]
#[test]
fn zstd_strategy_compress_and_decompress() {
    let strategy = ZstdStrategy::new(CompressionLevel::Default);
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    let comp_bytes = strategy.compress(MEDIUM_DATA, &mut compressed).unwrap();
    assert!(comp_bytes > 0);

    let decomp_bytes = strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(decomp_bytes, MEDIUM_DATA.len());
    assert_eq!(&decompressed, MEDIUM_DATA);
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_strategy_different_levels() {
    let data = COMPRESSIBLE_DATA;

    let fast = ZstdStrategy::new(CompressionLevel::Fast);
    let default = ZstdStrategy::new(CompressionLevel::Default);
    let best = ZstdStrategy::new(CompressionLevel::Best);

    let mut fast_out = Vec::new();
    let mut default_out = Vec::new();
    let mut best_out = Vec::new();

    fast.compress(data, &mut fast_out).unwrap();
    default.compress(data, &mut default_out).unwrap();
    best.compress(data, &mut best_out).unwrap();

    // For small data, different compression levels might produce similar sizes
    // Just verify all produce valid compressed output that decompresses correctly.

    // All should decompress correctly
    for (strategy, compressed) in [(&fast, &fast_out), (&default, &default_out), (&best, &best_out)]
    {
        let mut decompressed = Vec::new();
        strategy.decompress(compressed, &mut decompressed).unwrap();
        assert_eq!(&decompressed, data);
    }
}

#[cfg(feature = "zstd")]
#[test]
fn zstd_strategy_empty_input() {
    let strategy = ZstdStrategy::with_default_level();
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    strategy.compress(EMPTY_DATA, &mut compressed).unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();

    assert_eq!(&decompressed, EMPTY_DATA);
}

// ============================================================================
// Lz4Strategy Tests
// ============================================================================

#[cfg(feature = "lz4")]
#[test]
fn lz4_strategy_compress_and_decompress() {
    let strategy = Lz4Strategy::new(CompressionLevel::Default);
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    let comp_bytes = strategy.compress(MEDIUM_DATA, &mut compressed).unwrap();
    assert!(comp_bytes > 0);

    let decomp_bytes = strategy.decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(decomp_bytes, MEDIUM_DATA.len());
    assert_eq!(&decompressed, MEDIUM_DATA);
}

#[cfg(feature = "lz4")]
#[test]
fn lz4_strategy_empty_input() {
    let strategy = Lz4Strategy::with_default_level();
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();

    strategy.compress(EMPTY_DATA, &mut compressed).unwrap();
    strategy.decompress(&compressed, &mut decompressed).unwrap();

    assert_eq!(&decompressed, EMPTY_DATA);
}

// ============================================================================
// CompressionStrategySelector Tests
// ============================================================================

#[test]
fn selector_for_protocol_version_creates_correct_strategy() {
    // Protocol 30 should use Zlib
    let strategy = CompressionStrategySelector::for_protocol_version(30);
    assert_eq!(strategy.algorithm_name(), "zlib");

    // Protocol 35 should use Zlib
    let strategy = CompressionStrategySelector::for_protocol_version(35);
    assert_eq!(strategy.algorithm_name(), "zlib");

    // Protocol 36 should use Zstd (if available)
    let strategy = CompressionStrategySelector::for_protocol_version(36);
    #[cfg(feature = "zstd")]
    assert_eq!(strategy.algorithm_name(), "zstd");
    #[cfg(not(feature = "zstd"))]
    assert_eq!(strategy.algorithm_name(), "zlib");
}

#[test]
fn selector_for_algorithm_creates_correct_strategy() {
    // None
    let strategy =
        CompressionStrategySelector::for_algorithm(CompressionAlgorithmKind::None, CompressionLevel::None)
            .unwrap();
    assert_eq!(strategy.algorithm_kind(), CompressionAlgorithmKind::None);

    // Zlib
    let strategy = CompressionStrategySelector::for_algorithm(
        CompressionAlgorithmKind::Zlib,
        CompressionLevel::Best,
    )
    .unwrap();
    assert_eq!(strategy.algorithm_kind(), CompressionAlgorithmKind::Zlib);

    // Zstd
    #[cfg(feature = "zstd")]
    {
        let strategy = CompressionStrategySelector::for_algorithm(
            CompressionAlgorithmKind::Zstd,
            CompressionLevel::Default,
        )
        .unwrap();
        assert_eq!(strategy.algorithm_kind(), CompressionAlgorithmKind::Zstd);
    }

    // Lz4
    #[cfg(feature = "lz4")]
    {
        let strategy = CompressionStrategySelector::for_algorithm(
            CompressionAlgorithmKind::Lz4,
            CompressionLevel::Fast,
        )
        .unwrap();
        assert_eq!(strategy.algorithm_kind(), CompressionAlgorithmKind::Lz4);
    }
}

#[test]
fn selector_for_unavailable_algorithm_returns_error() {
    #[cfg(not(feature = "zstd"))]
    {
        let result = CompressionStrategySelector::for_algorithm(
            CompressionAlgorithmKind::Zstd,
            CompressionLevel::Default,
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
    }

    #[cfg(not(feature = "lz4"))]
    {
        let result = CompressionStrategySelector::for_algorithm(
            CompressionAlgorithmKind::Lz4,
            CompressionLevel::Default,
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
    }
}

#[test]
fn selector_negotiate_finds_first_common_algorithm() {
    let local = vec![
        CompressionAlgorithmKind::Zstd,
        CompressionAlgorithmKind::Zlib,
        CompressionAlgorithmKind::Lz4,
    ];
    let remote = vec![
        CompressionAlgorithmKind::Lz4,
        CompressionAlgorithmKind::Zlib,
    ];

    let strategy = CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);

    // Should select Zlib (first common algorithm in local preference order)
    assert_eq!(strategy.algorithm_name(), "zlib");
}

#[test]
fn selector_negotiate_respects_local_preference_order() {
    let local = vec![
        CompressionAlgorithmKind::Zlib,
        CompressionAlgorithmKind::Zstd,
    ];
    let remote = vec![
        CompressionAlgorithmKind::Zstd,
        CompressionAlgorithmKind::Zlib,
    ];

    let strategy = CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);

    // Should select Zlib (first in local list that remote supports)
    assert_eq!(strategy.algorithm_name(), "zlib");
}

#[test]
fn selector_negotiate_no_common_algorithm_returns_none() {
    let local = vec![CompressionAlgorithmKind::Zstd];
    let remote = vec![CompressionAlgorithmKind::Lz4];

    let strategy = CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);

    // Should fall back to no compression
    assert_eq!(strategy.algorithm_name(), "none");
}

#[test]
fn selector_negotiate_empty_lists() {
    let local: Vec<CompressionAlgorithmKind> = vec![];
    let remote: Vec<CompressionAlgorithmKind> = vec![];

    let strategy = CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);

    // Should fall back to no compression
    assert_eq!(strategy.algorithm_name(), "none");
}

#[test]
fn selector_concrete_factories_work() {
    // None
    let strategy = CompressionStrategySelector::none();
    assert_eq!(strategy.algorithm_name(), "none");

    // Zlib
    let strategy = CompressionStrategySelector::zlib_default();
    assert_eq!(strategy.algorithm_name(), "zlib");

    let strategy = CompressionStrategySelector::zlib(CompressionLevel::Best);
    assert_eq!(strategy.algorithm_name(), "zlib");

    // Zstd
    #[cfg(feature = "zstd")]
    {
        let strategy = CompressionStrategySelector::zstd_default();
        assert_eq!(strategy.algorithm_name(), "zstd");

        let strategy = CompressionStrategySelector::zstd(CompressionLevel::Fast);
        assert_eq!(strategy.algorithm_name(), "zstd");
    }

    // Lz4
    #[cfg(feature = "lz4")]
    {
        let strategy = CompressionStrategySelector::lz4_default();
        assert_eq!(strategy.algorithm_name(), "lz4");

        let strategy = CompressionStrategySelector::lz4(CompressionLevel::Default);
        assert_eq!(strategy.algorithm_name(), "lz4");
    }
}

// ============================================================================
// Cross-Algorithm Compatibility Tests
// ============================================================================

#[test]
fn all_strategies_handle_empty_input() {
    let strategies: Vec<Box<dyn CompressionStrategy>> = vec![
        Box::new(NoCompressionStrategy::new()),
        Box::new(ZlibStrategy::with_default_level()),
        #[cfg(feature = "zstd")]
        Box::new(ZstdStrategy::with_default_level()),
        #[cfg(feature = "lz4")]
        Box::new(Lz4Strategy::with_default_level()),
    ];

    for strategy in &strategies {
        let mut compressed = Vec::new();
        let mut decompressed = Vec::new();

        strategy.compress(EMPTY_DATA, &mut compressed).unwrap();
        strategy.decompress(&compressed, &mut decompressed).unwrap();

        assert_eq!(
            &decompressed,
            EMPTY_DATA,
            "Failed for {}",
            strategy.algorithm_name()
        );
    }
}

#[test]
fn all_strategies_handle_small_data() {
    let strategies: Vec<Box<dyn CompressionStrategy>> = vec![
        Box::new(NoCompressionStrategy::new()),
        Box::new(ZlibStrategy::with_default_level()),
        #[cfg(feature = "zstd")]
        Box::new(ZstdStrategy::with_default_level()),
        #[cfg(feature = "lz4")]
        Box::new(Lz4Strategy::with_default_level()),
    ];

    for strategy in &strategies {
        let mut compressed = Vec::new();
        let mut decompressed = Vec::new();

        strategy.compress(SMALL_DATA, &mut compressed).unwrap();
        strategy.decompress(&compressed, &mut decompressed).unwrap();

        assert_eq!(
            &decompressed,
            SMALL_DATA,
            "Failed for {}",
            strategy.algorithm_name()
        );
    }
}

#[test]
fn all_strategies_produce_deterministic_output() {
    let strategies: Vec<Box<dyn CompressionStrategy>> = vec![
        Box::new(NoCompressionStrategy::new()),
        Box::new(ZlibStrategy::with_default_level()),
        #[cfg(feature = "zstd")]
        Box::new(ZstdStrategy::with_default_level()),
        #[cfg(feature = "lz4")]
        Box::new(Lz4Strategy::with_default_level()),
    ];

    for strategy in &strategies {
        let mut output1 = Vec::new();
        let mut output2 = Vec::new();

        strategy.compress(MEDIUM_DATA, &mut output1).unwrap();
        strategy.compress(MEDIUM_DATA, &mut output2).unwrap();

        assert_eq!(
            output1, output2,
            "Non-deterministic output for {}",
            strategy.algorithm_name()
        );
    }
}

// ============================================================================
// Trait Object Tests
// ============================================================================

#[test]
fn strategies_work_as_trait_objects() {
    let strategies: Vec<Box<dyn CompressionStrategy>> = vec![
        Box::new(NoCompressionStrategy::new()),
        Box::new(ZlibStrategy::new(CompressionLevel::Default)),
        #[cfg(feature = "zstd")]
        Box::new(ZstdStrategy::new(CompressionLevel::Default)),
        #[cfg(feature = "lz4")]
        Box::new(Lz4Strategy::new(CompressionLevel::Default)),
    ];

    for strategy in &strategies {
        let mut compressed = Vec::new();
        let mut decompressed = Vec::new();

        let comp_bytes = strategy.compress(MEDIUM_DATA, &mut compressed).unwrap();
        assert!(comp_bytes > 0);

        let decomp_bytes = strategy.decompress(&compressed, &mut decompressed).unwrap();
        assert_eq!(decomp_bytes, MEDIUM_DATA.len());
        assert_eq!(&decompressed, MEDIUM_DATA);
    }
}

#[test]
fn strategies_are_send_and_sync() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    assert_send::<NoCompressionStrategy>();
    assert_sync::<NoCompressionStrategy>();

    assert_send::<ZlibStrategy>();
    assert_sync::<ZlibStrategy>();

    #[cfg(feature = "zstd")]
    {
        assert_send::<ZstdStrategy>();
        assert_sync::<ZstdStrategy>();
    }

    #[cfg(feature = "lz4")]
    {
        assert_send::<Lz4Strategy>();
        assert_sync::<Lz4Strategy>();
    }
}

// ============================================================================
// Integration with Protocol Simulation Tests
// ============================================================================

#[test]
fn protocol_version_roundtrip_simulation() {
    // Simulate different protocol versions
    for version in 27..40 {
        let strategy = CompressionStrategySelector::for_protocol_version(version);
        let mut compressed = Vec::new();
        let mut decompressed = Vec::new();

        strategy.compress(MEDIUM_DATA, &mut compressed).unwrap();
        strategy.decompress(&compressed, &mut decompressed).unwrap();

        assert_eq!(
            &decompressed,
            MEDIUM_DATA,
            "Failed roundtrip for protocol version {}",
            version
        );
    }
}

#[test]
fn negotiation_simulation() {
    // Simulate client-server negotiation scenarios

    // Scenario 1: Both support all algorithms
    let client_algos = CompressionAlgorithmKind::all();
    let server_algos = CompressionAlgorithmKind::all();
    let strategy = CompressionStrategySelector::negotiate(&client_algos, &server_algos, CompressionLevel::Default);
    assert!(strategy.algorithm_kind().is_available());

    // Scenario 2: Server only supports Zlib
    let server_algos = vec![CompressionAlgorithmKind::Zlib];
    let strategy = CompressionStrategySelector::negotiate(&client_algos, &server_algos, CompressionLevel::Default);
    assert_eq!(strategy.algorithm_kind(), CompressionAlgorithmKind::Zlib);

    // Scenario 3: Client prefers Zstd
    #[cfg(feature = "zstd")]
    {
        let client_algos = vec![
            CompressionAlgorithmKind::Zstd,
            CompressionAlgorithmKind::Zlib,
        ];
        let server_algos = CompressionAlgorithmKind::all();
        let strategy =
            CompressionStrategySelector::negotiate(&client_algos, &server_algos, CompressionLevel::Default);
        assert_eq!(strategy.algorithm_kind(), CompressionAlgorithmKind::Zstd);
    }
}

#[test]
fn large_data_roundtrip() {
    // Generate larger test data
    let large_data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();

    let strategies: Vec<Box<dyn CompressionStrategy>> = vec![
        Box::new(ZlibStrategy::new(CompressionLevel::Default)),
        #[cfg(feature = "zstd")]
        Box::new(ZstdStrategy::new(CompressionLevel::Default)),
        #[cfg(feature = "lz4")]
        Box::new(Lz4Strategy::new(CompressionLevel::Default)),
    ];

    for strategy in &strategies {
        let mut compressed = Vec::new();
        let mut decompressed = Vec::new();

        strategy.compress(&large_data, &mut compressed).unwrap();
        strategy.decompress(&compressed, &mut decompressed).unwrap();

        assert_eq!(
            decompressed, large_data,
            "Large data roundtrip failed for {}",
            strategy.algorithm_name()
        );
    }
}
