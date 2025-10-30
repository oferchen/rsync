#[test]
fn builder_records_connect_program_and_address_mode() {
    let connect_program = std::ffi::OsString::from("/usr/bin/ssh");
    let config = super::ClientConfig::builder()
        .connect_program(Some(connect_program.clone()))
        .address_mode(super::AddressMode::Ipv6)
        .build();

    assert_eq!(config.address_mode(), super::AddressMode::Ipv6);
    assert_eq!(
        config
            .connect_program()
            .map(|value| value.to_os_string()),
        Some(connect_program),
    );
}

#[test]
fn builder_records_bandwidth_limit_and_timeouts() {
    let rate = std::num::NonZeroU64::new(2_048).expect("non-zero rate");
    let burst = std::num::NonZeroU64::new(4_096).expect("non-zero burst");
    let limit = super::BandwidthLimit::from_rate_and_burst(rate, Some(burst));
    let transfer_timeout = super::TransferTimeout::Seconds(
        std::num::NonZeroU64::new(30).expect("transfer timeout"),
    );
    let connect_timeout = super::TransferTimeout::Seconds(
        std::num::NonZeroU64::new(5).expect("connect timeout"),
    );

    let config = super::ClientConfig::builder()
        .bandwidth_limit(Some(limit))
        .timeout(transfer_timeout)
        .connect_timeout(connect_timeout)
        .build();

    assert_eq!(config.bandwidth_limit(), Some(limit));
    assert_eq!(config.timeout(), transfer_timeout);
    assert_eq!(config.connect_timeout(), connect_timeout);
}
