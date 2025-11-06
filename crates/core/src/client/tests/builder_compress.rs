use std::num::NonZeroU32;

#[test]
fn builder_enabling_compress_sets_default_level() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .compress(true)
        .build();

    assert!(config.compress());
    assert!(config.compression_setting().is_enabled());
    assert_eq!(
        config.compression_setting().level_or_default(),
        CompressionLevel::Default
    );
}

#[test]
fn builder_sets_min_file_size_limit() {
    let config = ClientConfig::builder().min_file_size(Some(1_024)).build();

    assert_eq!(config.min_file_size(), Some(1_024));

    let cleared = ClientConfig::builder()
        .min_file_size(Some(2048))
        .min_file_size(None)
        .build();

    assert_eq!(cleared.min_file_size(), None);
}

#[test]
fn builder_sets_max_file_size_limit() {
    let config = ClientConfig::builder().max_file_size(Some(8_192)).build();

    assert_eq!(config.max_file_size(), Some(8_192));

    let cleared = ClientConfig::builder()
        .max_file_size(Some(4_096))
        .max_file_size(None)
        .build();

    assert_eq!(cleared.max_file_size(), None);
}

#[test]
fn builder_sets_block_size_override() {
    let override_size = NonZeroU32::new(16_384).unwrap();
    let config = ClientConfig::builder()
        .block_size_override(Some(override_size))
        .build();

    assert_eq!(config.block_size_override(), Some(override_size));

    let cleared = ClientConfig::builder()
        .block_size_override(Some(override_size))
        .block_size_override(None)
        .build();
    assert_eq!(cleared.block_size_override(), None);
}

#[test]
fn builder_disabling_compress_clears_override() {
    let level = NonZeroU8::new(5).unwrap();
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .compression_level(Some(CompressionLevel::precise(level)))
        .compress(false)
        .build();

    assert!(!config.compress());
    assert!(config.compression_setting().is_disabled());
    assert_eq!(config.compression_level(), None);
}

#[test]
fn builder_enables_delete() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete(true)
        .build();

    assert!(config.delete());
    assert_eq!(config.delete_mode(), DeleteMode::During);
}

#[test]
fn builder_enables_delete_after() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete_after(true)
        .build();

    assert!(config.delete());
    assert!(config.delete_after());
    assert_eq!(config.delete_mode(), DeleteMode::After);
}

#[test]
fn builder_enables_delete_delay() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete_delay(true)
        .build();

    assert!(config.delete());
    assert!(config.delete_delay());
    assert_eq!(config.delete_mode(), DeleteMode::Delay);
}

#[test]
fn builder_enables_delete_before() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete_before(true)
        .build();

    assert!(config.delete());
    assert!(config.delete_before());
    assert_eq!(config.delete_mode(), DeleteMode::Before);
}

#[test]
fn builder_enables_delete_excluded() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete_excluded(true)
        .build();

    assert!(config.delete_excluded());
    assert!(!ClientConfig::default().delete_excluded());
}

#[test]
fn builder_sets_max_delete_limit() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .max_delete(Some(4))
        .build();

    assert_eq!(config.max_delete(), Some(4));
    assert_eq!(ClientConfig::default().max_delete(), None);
}

#[test]
fn builder_enables_checksum() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .checksum(true)
        .build();

    assert!(config.checksum());
}

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

#[test]
fn builder_sets_bandwidth_limit() {
    let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(4096).unwrap());
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .bandwidth_limit(Some(limit))
        .build();

    assert_eq!(config.bandwidth_limit(), Some(limit));
}

#[test]
fn bandwidth_limit_to_limiter_preserves_rate_and_burst() {
    let rate = NonZeroU64::new(8 * 1024 * 1024).expect("non-zero rate");
    let burst = NonZeroU64::new(256 * 1024).expect("non-zero burst");
    let limit = BandwidthLimit::from_rate_and_burst(rate, Some(burst));

    let limiter = limit.to_limiter();

    assert_eq!(limiter.limit_bytes(), rate);
    assert_eq!(limiter.burst_bytes(), Some(burst));
}

#[test]
fn bandwidth_limit_into_limiter_transfers_configuration() {
    let rate = NonZeroU64::new(4 * 1024 * 1024).expect("non-zero rate");
    let limit = BandwidthLimit::from_rate_and_burst(rate, None);

    let limiter = limit.into_limiter();

    assert_eq!(limiter.limit_bytes(), rate);
    assert_eq!(limiter.burst_bytes(), None);
}

#[test]
fn bandwidth_limit_fallback_argument_returns_bytes_per_second() {
    let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(2048).unwrap());
    assert_eq!(limit.fallback_argument(), OsString::from("2048"));
}

#[test]
fn bandwidth_limit_fallback_argument_includes_burst_when_specified() {
    let rate = NonZeroU64::new(8 * 1024).unwrap();
    let burst = NonZeroU64::new(32 * 1024).unwrap();
    let limit = BandwidthLimit::from_rate_and_burst(rate, Some(burst));

    assert_eq!(limit.fallback_argument(), OsString::from("8192:32768"));
}

#[test]
fn bandwidth_limit_fallback_argument_preserves_explicit_zero_burst() {
    let rate = NonZeroU64::new(4 * 1024).unwrap();
    let components =
        bandwidth::BandwidthLimitComponents::new_with_specified(Some(rate), None, true);

    let limit = BandwidthLimit::from_components(components).expect("limit produced");

    assert!(limit.burst_specified());
    assert_eq!(limit.burst_bytes(), None);
    assert_eq!(limit.fallback_argument(), OsString::from("4096:0"));

    let round_trip = limit.components();
    assert!(round_trip.burst_specified());
    assert_eq!(round_trip.burst(), None);
}

#[test]
fn bandwidth_limit_fallback_unlimited_argument_returns_zero() {
    assert_eq!(
        BandwidthLimit::fallback_unlimited_argument(),
        OsString::from("0"),
    );
}

#[test]
fn builder_sets_compression_level() {
    let level = NonZeroU8::new(7).unwrap();
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .compress(true)
        .compression_level(Some(CompressionLevel::precise(level)))
        .build();

    assert!(config.compress());
    assert_eq!(
        config.compression_level(),
        Some(CompressionLevel::precise(level))
    );
    assert_eq!(ClientConfig::default().compression_level(), None);
}

#[test]
fn builder_enables_update() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .update(true)
        .build();

    assert!(config.update());
    assert!(!ClientConfig::default().update());
}

#[test]
fn builder_sets_timeout() {
    let timeout = TransferTimeout::Seconds(NonZeroU64::new(30).unwrap());
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .timeout(timeout)
        .build();

    assert_eq!(config.timeout(), timeout);
    assert_eq!(ClientConfig::default().timeout(), TransferTimeout::Default);
}

#[test]
fn builder_sets_connect_timeout() {
    let timeout = TransferTimeout::Seconds(NonZeroU64::new(12).unwrap());
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .connect_timeout(timeout)
        .build();

    assert_eq!(config.connect_timeout(), timeout);
    assert_eq!(
        ClientConfig::default().connect_timeout(),
        TransferTimeout::Default
    );
}

#[test]
fn builder_records_modify_window() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .modify_window(Some(5))
        .build();

    assert_eq!(config.modify_window(), Some(5));
    assert_eq!(config.modify_window_duration(), Duration::from_secs(5));
    assert_eq!(
        ClientConfig::default().modify_window_duration(),
        Duration::ZERO
    );
}

#[test]
fn builder_collects_reference_directories() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .compare_destination(PathBuf::from("compare"))
        .copy_destination(PathBuf::from("copy"))
        .link_destination(PathBuf::from("link"))
        .build();

    let references = config.reference_directories();
    assert_eq!(references.len(), 3);
    assert_eq!(references[0].kind(), ReferenceDirectoryKind::Compare);
    assert_eq!(references[1].kind(), ReferenceDirectoryKind::Copy);
    assert_eq!(references[2].kind(), ReferenceDirectoryKind::Link);
    assert_eq!(references[0].path(), PathBuf::from("compare").as_path());
}

#[test]
fn local_copy_options_apply_explicit_timeout() {
    let timeout = TransferTimeout::Seconds(NonZeroU64::new(5).unwrap());
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .timeout(timeout)
        .build();

    let options = build_local_copy_options(&config, None);
    assert_eq!(options.timeout(), Some(Duration::from_secs(5)));
}

#[test]
fn local_copy_options_apply_modify_window() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .modify_window(Some(3))
        .build();

    let options = build_local_copy_options(&config, None);
    assert_eq!(options.modify_window(), Duration::from_secs(3));

    let default_config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();
    assert_eq!(
        build_local_copy_options(&default_config, None).modify_window(),
        Duration::ZERO
    );
}

#[test]
fn local_copy_options_omit_timeout_when_unset() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    let options = build_local_copy_options(&config, None);
    assert!(options.timeout().is_none());
}

#[test]
fn local_copy_options_delay_updates_enable_partial_transfers() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delay_updates(true)
        .build();

    let enabled_options = build_local_copy_options(&enabled, None);
    assert!(enabled_options.delay_updates_enabled());
    assert!(enabled_options.partial_enabled());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    let disabled_options = build_local_copy_options(&disabled, None);
    assert!(!disabled_options.delay_updates_enabled());
    assert!(!disabled_options.partial_enabled());
}

#[test]
fn local_copy_options_honour_temp_directory_setting() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .temp_directory(Some(PathBuf::from(".staging")))
        .build();

    let options = build_local_copy_options(&config, None);
    assert_eq!(options.temp_directory_path(), Some(Path::new(".staging")));

    let default_config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(
        build_local_copy_options(&default_config, None)
            .temp_directory_path()
            .is_none()
    );
}

#[test]
fn resolve_connect_timeout_prefers_explicit_setting() {
    let explicit = TransferTimeout::Seconds(NonZeroU64::new(5).unwrap());
    let resolved =
        resolve_connect_timeout(explicit, TransferTimeout::Default, Duration::from_secs(30));
    assert_eq!(resolved, Some(Duration::from_secs(5)));
}

#[test]
fn resolve_connect_timeout_uses_transfer_timeout_when_default() {
    let transfer = TransferTimeout::Seconds(NonZeroU64::new(8).unwrap());
    let resolved =
        resolve_connect_timeout(TransferTimeout::Default, transfer, Duration::from_secs(30));
    assert_eq!(resolved, Some(Duration::from_secs(8)));
}

#[test]
fn resolve_connect_timeout_disables_when_requested() {
    let resolved = resolve_connect_timeout(
        TransferTimeout::Disabled,
        TransferTimeout::Seconds(NonZeroU64::new(9).unwrap()),
        Duration::from_secs(30),
    );
    assert!(resolved.is_none());

    let resolved_default = resolve_connect_timeout(
        TransferTimeout::Default,
        TransferTimeout::Disabled,
        Duration::from_secs(30),
    );
    assert!(resolved_default.is_none());
}

#[test]
fn connect_direct_applies_io_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let daemon_addr = DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("daemon addr");

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1];
            let _ = stream.read(&mut buf);
        }
    });

    let timeout = Some(Duration::from_secs(4));
    let mut stream = connect_direct(
        &daemon_addr,
        Some(Duration::from_secs(10)),
        timeout,
        AddressMode::Default,
        None,
    )
    .expect("connect directly");

    assert_eq!(stream.read_timeout().expect("read timeout"), timeout);
    assert_eq!(stream.write_timeout().expect("write timeout"), timeout);

    // Wake the accept loop and close cleanly.
    let _ = stream.write_all(&[0]);
    handle.join().expect("listener thread");
}

#[test]
fn connect_via_proxy_applies_io_timeout() {
    let proxy_listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
    let proxy_addr = proxy_listener.local_addr().expect("proxy addr");
    let proxy = ProxyConfig {
        host: proxy_addr.ip().to_string(),
        port: proxy_addr.port(),
        credentials: None,
    };

    let target = DaemonAddress::new(String::from("daemon.example"), 873).expect("daemon addr");

    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = proxy_listener.accept() {
            let mut reader = BufReader::new(stream);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).expect("read request") == 0 {
                    return;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
            }

            let mut stream = reader.into_inner();
            stream
                .write_all(b"HTTP/1.0 200 Connection established\r\n\r\n")
                .expect("respond to connect");
            let _ = stream.flush();
        }
    });

    let timeout = Some(Duration::from_secs(6));
    let stream = connect_via_proxy(&target, &proxy, Some(Duration::from_secs(9)), timeout, None)
        .expect("proxy connect");

    assert_eq!(stream.read_timeout().expect("read timeout"), timeout);
    assert_eq!(stream.write_timeout().expect("write timeout"), timeout);

    drop(stream);
    handle.join().expect("proxy thread");
}

