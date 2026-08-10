#[test]
fn format_bandwidth_rate_prefers_largest_whole_unit() {
    let cases = [
        (NonZeroU64::new(512).unwrap(), "512 bytes/s"),
        (NonZeroU64::new(1024).unwrap(), "1 KiB/s"),
        (NonZeroU64::new(8 * 1024).unwrap(), "8 KiB/s"),
        (NonZeroU64::new(1024 * 1024).unwrap(), "1 MiB/s"),
        (NonZeroU64::new(1024 * 1024 * 1024).unwrap(), "1 GiB/s"),
        (NonZeroU64::new(1024u64.pow(4)).unwrap(), "1 TiB/s"),
        (NonZeroU64::new(2 * 1024u64.pow(5)).unwrap(), "2 PiB/s"),
    ];

    for (input, expected) in cases {
        assert_eq!(format_bandwidth_rate(input), expected);
    }
}

