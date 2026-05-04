use super::*;

#[test]
fn parse_client_info_extracts_capabilities_from_separate_args() {
    let args = vec!["-e".to_owned(), "fxCIvu".to_owned()];
    let info = parse_client_info(&args);
    assert_eq!(info, "fxCIvu");
}

#[test]
fn parse_client_info_extracts_capabilities_from_combined_args() {
    let args = vec!["-efxCIvu".to_owned()];
    let info = parse_client_info(&args);
    assert_eq!(info, "fxCIvu");
}

#[test]
fn parse_client_info_handles_version_placeholder() {
    let args = vec!["-e.LsfxCIvu".to_owned()];
    let info = parse_client_info(&args);
    assert_eq!(info, "LsfxCIvu");
}

#[test]
fn parse_client_info_returns_empty_when_not_found() {
    let args = vec!["--server".to_owned(), "--sender".to_owned()];
    let info = parse_client_info(&args);
    assert_eq!(info, "");
}

#[test]
#[cfg(unix)]
fn build_compat_flags_enables_symlink_times_when_client_advertises_l() {
    let flags = build_compat_flags_from_client_info("LfxCIvu", true);
    assert!(
        flags.contains(CompatibilityFlags::SYMLINK_TIMES),
        "SYMLINK_TIMES should be enabled when client advertises 'L' on Unix"
    );
}

#[test]
fn build_compat_flags_skips_symlink_times_when_client_missing_l() {
    let flags = build_compat_flags_from_client_info("fxCIvu", true);
    assert!(
        !flags.contains(CompatibilityFlags::SYMLINK_TIMES),
        "SYMLINK_TIMES should not be enabled when client doesn't advertise 'L'"
    );
}

#[test]
fn build_compat_flags_enables_safe_file_list_when_client_advertises_f() {
    let flags = build_compat_flags_from_client_info("fxCIvu", true);
    assert!(
        flags.contains(CompatibilityFlags::SAFE_FILE_LIST),
        "SAFE_FILE_LIST should be enabled when client advertises 'f'"
    );
}

#[test]
fn build_compat_flags_enables_checksum_seed_fix_when_client_advertises_c() {
    let flags = build_compat_flags_from_client_info("fxCIvu", true);
    assert!(
        flags.contains(CompatibilityFlags::CHECKSUM_SEED_FIX),
        "CHECKSUM_SEED_FIX should be enabled when client advertises 'C'"
    );
}

#[test]
fn build_compat_flags_respects_inc_recurse_gate() {
    let flags_allowed = build_compat_flags_from_client_info("ifxCIvu", true);
    assert!(
        flags_allowed.contains(CompatibilityFlags::INC_RECURSE),
        "INC_RECURSE should be enabled when allowed and client advertises 'i'"
    );

    let flags_forbidden = build_compat_flags_from_client_info("ifxCIvu", false);
    assert!(
        !flags_forbidden.contains(CompatibilityFlags::INC_RECURSE),
        "INC_RECURSE should not be enabled when not allowed even if client advertises 'i'"
    );
}

#[test]
fn build_compat_flags_enables_id0_names_when_client_advertises_u() {
    let flags = build_compat_flags_from_client_info("ufxCIv", true);
    assert!(
        flags.contains(CompatibilityFlags::ID0_NAMES),
        "ID0_NAMES should be enabled when client advertises 'u'"
    );
}

#[test]
fn build_compat_flags_skips_id0_names_when_client_missing_u() {
    let flags = build_compat_flags_from_client_info("fxCIv", true);
    assert!(
        !flags.contains(CompatibilityFlags::ID0_NAMES),
        "ID0_NAMES should not be enabled when client doesn't advertise 'u'"
    );
}

#[test]
fn build_compat_flags_enables_inplace_partial_dir_when_client_advertises_i_cap() {
    let flags = build_compat_flags_from_client_info("fxCIvu", true);
    assert!(
        flags.contains(CompatibilityFlags::INPLACE_PARTIAL_DIR),
        "INPLACE_PARTIAL_DIR should be enabled when client advertises 'I'"
    );
}

#[test]
fn build_compat_flags_skips_inplace_partial_dir_when_client_missing_i_cap() {
    let flags = build_compat_flags_from_client_info("fxCvu", true);
    assert!(
        !flags.contains(CompatibilityFlags::INPLACE_PARTIAL_DIR),
        "INPLACE_PARTIAL_DIR should not be enabled when client doesn't advertise 'I'"
    );
}

#[test]
fn build_compat_flags_enables_avoid_xattr_optimization_when_client_advertises_x() {
    let flags = build_compat_flags_from_client_info("xfCIvu", true);
    assert!(
        flags.contains(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION),
        "AVOID_XATTR_OPTIMIZATION should be enabled when client advertises 'x'"
    );
}

#[test]
fn build_compat_flags_skips_avoid_xattr_optimization_when_client_missing_x() {
    let flags = build_compat_flags_from_client_info("fCIvu", true);
    assert!(
        !flags.contains(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION),
        "AVOID_XATTR_OPTIMIZATION should not be enabled when client doesn't advertise 'x'"
    );
}

#[test]
fn setup_protocol_below_30_returns_none_for_algorithms_and_compat() {
    // Protocol 29 should skip all negotiation and compat exchange
    let protocol = ProtocolVersion::try_from(29).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let config = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: None,
        is_server: true,
        is_daemon_mode: false,
        do_compression: false,
        compress_choice: None,
        checksum_seed: None,
        allow_inc_recurse: false,
    };

    let result =
        setup_protocol(&mut stdout, &mut stdin, &config).expect("protocol 29 setup should succeed");

    assert!(
        result.negotiated_algorithms.is_none(),
        "Protocol 29 should not negotiate algorithms"
    );
    assert!(
        result.compat_flags.is_none(),
        "Protocol 29 should not exchange compat flags"
    );
    // Protocol 29 still does seed exchange (server writes 4 bytes)
    assert_eq!(
        stdout.len(),
        4,
        "Protocol 29 server should write 4-byte checksum seed"
    );
}

#[test]
fn setup_protocol_skip_compat_exchange_skips_flags() {
    // With skip_compat_exchange=true, even protocol 30+ should skip compat flags
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let config = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: true, // SKIP
        client_args: None,
        is_server: true,
        is_daemon_mode: false,
        do_compression: false,
        compress_choice: None,
        checksum_seed: None,
        allow_inc_recurse: false,
    };

    let result = setup_protocol(&mut stdout, &mut stdin, &config)
        .expect("setup with skip_compat_exchange should succeed");

    assert!(
        result.compat_flags.is_none(),
        "skip_compat_exchange=true should skip compat flags"
    );
    assert!(
        result.negotiated_algorithms.is_none(),
        "skip_compat_exchange=true should skip algorithm negotiation"
    );
    // Only the 4-byte seed should be written
    assert_eq!(
        stdout.len(),
        4,
        "Only checksum seed should be written when skip_compat_exchange=true"
    );
}

#[test]
fn setup_protocol_server_writes_compat_flags_and_seed() {
    // Server mode (is_server=true) should WRITE compat flags, not read them
    let protocol = ProtocolVersion::try_from(31).unwrap();
    // Server doesn't read stdin during its turn (compat exchange is unidirectional)
    // Provide algorithm list for negotiation (empty list = use defaults)
    let mut stdin = &b"\x03md5"[..]; // vstring: "md5" checksum list
    let mut stdout = Vec::new();

    let client_args = ["-efxCIvu".to_owned()];
    let config = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: Some(&client_args), // client_args with 'v' capability
        is_server: true,
        is_daemon_mode: true, // server advertises, client reads
        do_compression: false,
        compress_choice: None,
        checksum_seed: None,
        allow_inc_recurse: false,
    };

    let result =
        setup_protocol(&mut stdout, &mut stdin, &config).expect("server setup should succeed");

    assert!(
        result.compat_flags.is_some(),
        "Server should have compat flags"
    );
    let flags = result.compat_flags.unwrap();
    assert!(
        flags.contains(CompatibilityFlags::CHECKSUM_SEED_FIX),
        "Server should have CHECKSUM_SEED_FIX from client 'C' capability"
    );
    assert!(
        flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
        "Server should have VARINT_FLIST_FLAGS from client 'v' capability"
    );

    // stdout should contain: varint compat flags + algorithm lists + 4-byte seed
    assert!(
        stdout.len() >= 5, // At least 1 byte varint + 4 bytes seed
        "Server should write compat flags varint and seed"
    );
}

#[test]
fn setup_protocol_client_reads_compat_flags_from_server() {
    // Client mode (is_server=false) should READ compat flags from server
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Prepare server response: varint compat flags + checksum seed
    // compat flags = 0x21 (INC_RECURSE | CHECKSUM_SEED_FIX) - NO VARINT_FLIST_FLAGS
    // When VARINT_FLIST_FLAGS is not set, do_negotiation=false and no algorithm
    // lists are exchanged.
    let mut server_response: Vec<u8> = vec![0x21]; // compat flags varint

    // Server sends checksum seed (4 bytes little-endian)
    let test_seed: i32 = 0x12345678;
    server_response.extend_from_slice(&test_seed.to_le_bytes());

    let mut stdin = &server_response[..];
    let mut stdout = Vec::new();

    let config = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: None, // not needed for client mode
        is_server: false,  // CLIENT mode
        is_daemon_mode: true,
        do_compression: false,
        compress_choice: None,
        checksum_seed: None,
        allow_inc_recurse: false,
    };

    let result =
        setup_protocol(&mut stdout, &mut stdin, &config).expect("client setup should succeed");

    assert!(
        result.compat_flags.is_some(),
        "Client should have compat flags"
    );
    let flags = result.compat_flags.unwrap();
    assert!(
        flags.contains(CompatibilityFlags::CHECKSUM_SEED_FIX),
        "Client should read CHECKSUM_SEED_FIX from server"
    );
    assert!(
        !flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
        "Server sent flags without VARINT_FLIST_FLAGS"
    );

    assert_eq!(
        result.checksum_seed, test_seed,
        "Client should read the correct checksum seed"
    );
}

#[test]
fn setup_protocol_server_generates_deterministic_seeds() {
    // Same process within the same second produces the same seed
    // (seed = time_secs ^ (pid << 6)), so rapid calls are deterministic
    let protocol = ProtocolVersion::try_from(29).unwrap();
    let mut stdin = &b""[..];

    let config = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: None,
        is_server: true,
        is_daemon_mode: false,
        do_compression: false,
        compress_choice: None,
        checksum_seed: None,
        allow_inc_recurse: false,
    };

    let mut stdout1 = Vec::new();
    let result1 =
        setup_protocol(&mut stdout1, &mut stdin, &config).expect("first setup should succeed");

    let mut stdout2 = Vec::new();
    let result2 =
        setup_protocol(&mut stdout2, &mut stdin, &config).expect("second setup should succeed");

    assert_eq!(
        result1.checksum_seed, result2.checksum_seed,
        "Same process in same second should have same seed (deterministic)"
    );
}

#[test]
fn setup_protocol_ssh_mode_bidirectional_exchange() {
    // SSH mode (is_daemon_mode=false) has bidirectional capability exchange
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Prepare stdin with what we expect to read from peer:
    // - Compat flags varint with VARINT_FLIST_FLAGS to trigger negotiation
    // - Checksum algorithm list (empty = use defaults)
    // - Checksum seed
    //
    // VARINT_FLIST_FLAGS = 0x80 = 128, INC_RECURSE = 0x01, CHECKSUM_SEED_FIX = 0x20
    // Combined: 0xA1 = 161 (requires 2-byte rsync varint encoding)
    // Rsync varint encoding of 161: [0x80, 0xA1] (marker byte, then value byte)
    let mut peer_data: Vec<u8> = vec![
        0x80, 0xA1, // varint for 161 (VARINT_FLIST_FLAGS | INC_RECURSE | CHECKSUM_SEED_FIX)
        0x03, b'm', b'd', b'5', // vstring "md5" - valid checksum list
    ];
    peer_data.extend_from_slice(&0x12345678_i32.to_le_bytes()); // seed

    let mut stdin = &peer_data[..];
    let mut stdout = Vec::new();

    let config = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: None,
        is_server: false, // CLIENT
        is_daemon_mode: false,
        do_compression: false,
        compress_choice: None,
        checksum_seed: None,
        allow_inc_recurse: false,
    };

    let result = setup_protocol(&mut stdout, &mut stdin, &config)
        .expect("SSH mode client setup should succeed");

    // Should have read compat flags from peer
    assert!(result.compat_flags.is_some());
    let flags = result.compat_flags.unwrap();
    assert!(
        flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
        "Should have VARINT_FLIST_FLAGS from server"
    );

    // SSH mode client should write algorithm preferences
    // (unlike daemon mode where client reads silently)
    // Client writes its checksum list in SSH bidirectional mode
    assert!(
        !stdout.is_empty(),
        "SSH mode client should write algorithm preferences"
    );
}

#[test]
fn setup_protocol_client_args_affects_compat_flags() {
    // Different client args should result in different compat flags
    let protocol = ProtocolVersion::try_from(31).unwrap();

    // Test with minimal capabilities
    let mut stdin = &b"\x03md5"[..]; // vstring: "md5" checksum list
    let mut stdout = Vec::new();

    let client_args_minimal = ["-ev".to_owned()];
    let config_minimal = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: Some(&client_args_minimal), // Only 'v' capability
        is_server: true,
        is_daemon_mode: true,
        do_compression: false,
        compress_choice: None,
        checksum_seed: None,
        allow_inc_recurse: false,
    };

    let result_minimal = setup_protocol(&mut stdout, &mut stdin, &config_minimal)
        .expect("minimal caps setup should succeed");

    let flags_minimal = result_minimal.compat_flags.unwrap();
    assert!(
        flags_minimal.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
        "Should have VARINT_FLIST_FLAGS from 'v'"
    );
    assert!(
        !flags_minimal.contains(CompatibilityFlags::CHECKSUM_SEED_FIX),
        "Should NOT have CHECKSUM_SEED_FIX without 'C'"
    );

    // Test with full capabilities
    let mut stdin = &b"\x03md5"[..];
    let mut stdout = Vec::new();

    let client_args_full = ["-e.LsfxCIvu".to_owned()];
    let config_full = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: Some(&client_args_full), // Full capabilities
        is_server: true,
        is_daemon_mode: true,
        do_compression: false,
        compress_choice: None,
        checksum_seed: None,
        allow_inc_recurse: false,
    };

    let result_full = setup_protocol(&mut stdout, &mut stdin, &config_full)
        .expect("full caps setup should succeed");

    let flags_full = result_full.compat_flags.unwrap();
    assert!(
        flags_full.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
        "Should have VARINT_FLIST_FLAGS from 'v'"
    );
    assert!(
        flags_full.contains(CompatibilityFlags::CHECKSUM_SEED_FIX),
        "Should have CHECKSUM_SEED_FIX from 'C'"
    );
    assert!(
        flags_full.contains(CompatibilityFlags::SAFE_FILE_LIST),
        "Should have SAFE_FILE_LIST from 'f'"
    );
    assert!(
        flags_full.contains(CompatibilityFlags::INPLACE_PARTIAL_DIR),
        "Should have INPLACE_PARTIAL_DIR from 'I'"
    );
}

#[test]
fn setup_protocol_protocol_30_minimum_for_compat_exchange() {
    // Protocol 30 is the minimum for compat exchange
    let protocol_30 = ProtocolVersion::try_from(30).unwrap();
    let mut stdin = &b"\x03md5"[..]; // vstring: "md5" checksum list
    let mut stdout = Vec::new();

    let client_args = ["-efxCIvu".to_owned()];
    let config = ProtocolSetupConfig {
        protocol: protocol_30,
        skip_compat_exchange: false,
        client_args: Some(&client_args),
        is_server: true,
        is_daemon_mode: true,
        do_compression: false,
        compress_choice: None,
        checksum_seed: None,
        allow_inc_recurse: false,
    };

    let result =
        setup_protocol(&mut stdout, &mut stdin, &config).expect("protocol 30 setup should succeed");

    assert!(
        result.compat_flags.is_some(),
        "Protocol 30 should exchange compat flags"
    );
    assert!(
        result.negotiated_algorithms.is_some(),
        "Protocol 30 should negotiate algorithms"
    );
}

#[test]
fn protocol_setup_config_builder_new_sets_defaults() {
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let config = ProtocolSetupConfig::new(protocol, true);

    assert_eq!(config.protocol.as_u8(), 31);
    assert!(config.is_server);
    assert!(!config.skip_compat_exchange);
    assert!(config.client_args.is_none());
    assert!(!config.is_daemon_mode);
    assert!(!config.do_compression);
    assert!(config.compress_choice.is_none());
    assert!(config.checksum_seed.is_none());
}

#[test]
fn protocol_setup_config_builder_chain_methods() {
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let client_args = vec!["-efxCIvu".to_owned()];

    let config = ProtocolSetupConfig::new(protocol, true)
        .with_skip_compat_exchange(true)
        .with_client_args(Some(&client_args))
        .with_daemon_mode(true)
        .with_compression(true)
        .with_checksum_seed(Some(12345));

    assert!(config.skip_compat_exchange);
    assert!(config.client_args.is_some());
    assert!(config.is_daemon_mode);
    assert!(config.do_compression);
    assert_eq!(config.checksum_seed, Some(12345));
}

#[test]
fn protocol_setup_config_builder_partial_configuration() {
    let protocol = ProtocolVersion::try_from(30).unwrap();

    // Only set some optional fields
    let config = ProtocolSetupConfig::new(protocol, false)
        .with_daemon_mode(true)
        .with_compression(false);

    assert!(!config.is_server);
    assert!(config.is_daemon_mode);
    assert!(!config.do_compression);
    // Other fields should still be at defaults
    assert!(!config.skip_compat_exchange);
    assert!(config.client_args.is_none());
    assert!(config.checksum_seed.is_none());
}

#[test]
fn protocol_setup_config_builder_can_override_values() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    let config = ProtocolSetupConfig::new(protocol, true)
        .with_compression(true)
        .with_compression(false); // Override previous value

    assert!(!config.do_compression, "Last value should win");
}

#[test]
fn protocol_setup_config_builder_works_in_real_setup() {
    // Verify builder works in actual setup_protocol call
    let protocol = ProtocolVersion::try_from(29).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let config = ProtocolSetupConfig::new(protocol, true)
        .with_compression(false)
        .with_daemon_mode(false);

    let result = setup_protocol(&mut stdout, &mut stdin, &config)
        .expect("setup should succeed with builder config");

    assert!(
        result.negotiated_algorithms.is_none(),
        "Protocol 29 should not negotiate"
    );
}

#[test]
fn setup_protocol_server_uses_fixed_checksum_seed() {
    // --checksum-seed=12345 should use exactly 12345 as the seed
    let protocol = ProtocolVersion::try_from(29).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let config = ProtocolSetupConfig::new(protocol, true).with_checksum_seed(Some(12345));

    let result = setup_protocol(&mut stdout, &mut stdin, &config)
        .expect("setup with fixed seed should succeed");

    assert_eq!(
        result.checksum_seed, 12345_i32,
        "Fixed seed value should be used as-is"
    );

    // Verify the seed was written to stdout as 4-byte LE
    assert_eq!(stdout.len(), 4, "Should write 4-byte seed");
    let written_seed = i32::from_le_bytes(stdout[..4].try_into().unwrap());
    assert_eq!(written_seed, 12345, "Written seed should match fixed value");
}

#[test]
fn setup_protocol_server_fixed_seed_is_deterministic() {
    // Same fixed seed should produce same result every time
    let protocol = ProtocolVersion::try_from(29).unwrap();

    let config = ProtocolSetupConfig::new(protocol, true).with_checksum_seed(Some(42));

    let mut stdout1 = Vec::new();
    let mut stdin1 = &b""[..];
    let result1 =
        setup_protocol(&mut stdout1, &mut stdin1, &config).expect("first setup should succeed");

    let mut stdout2 = Vec::new();
    let mut stdin2 = &b""[..];
    let result2 =
        setup_protocol(&mut stdout2, &mut stdin2, &config).expect("second setup should succeed");

    assert_eq!(
        result1.checksum_seed, result2.checksum_seed,
        "Fixed seed should be deterministic across calls"
    );
    assert_eq!(
        stdout1, stdout2,
        "Wire bytes should be identical for same fixed seed"
    );
}

#[test]
fn setup_protocol_server_seed_zero_uses_time_based_generation() {
    // --checksum-seed=0 is treated like None: generate from time XOR PID
    // This matches upstream rsync's behavior where 0 means "use current time"
    let protocol = ProtocolVersion::try_from(29).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let config = ProtocolSetupConfig::new(protocol, true).with_checksum_seed(Some(0));

    let result =
        setup_protocol(&mut stdout, &mut stdin, &config).expect("setup with seed=0 should succeed");

    // Seed=0 generates time-based seed, which should be non-zero in practice
    // (unless time and PID happen to XOR to 0, extremely unlikely)
    // We just verify it succeeded and produced a 4-byte seed
    assert_eq!(stdout.len(), 4, "Should write 4-byte seed");
    // Note: we cannot assert the exact value since it's time-dependent
    let _ = result.checksum_seed; // Just verify we got a value
}

#[test]
fn setup_protocol_server_max_u32_seed() {
    // --checksum-seed=4294967295 (u32::MAX) should work
    let protocol = ProtocolVersion::try_from(29).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let config = ProtocolSetupConfig::new(protocol, true).with_checksum_seed(Some(u32::MAX));

    let result = setup_protocol(&mut stdout, &mut stdin, &config)
        .expect("setup with max seed should succeed");

    // u32::MAX as i32 is -1
    assert_eq!(
        result.checksum_seed,
        u32::MAX as i32,
        "u32::MAX seed should be transmitted as i32 (-1)"
    );

    let written_seed = i32::from_le_bytes(stdout[..4].try_into().unwrap());
    assert_eq!(
        written_seed, -1_i32,
        "Wire representation of u32::MAX is -1 as i32"
    );
}

#[test]
fn setup_protocol_client_reads_fixed_seed_from_server() {
    // Client reads exact seed bytes sent by server
    let protocol = ProtocolVersion::try_from(29).unwrap();
    let test_seed: i32 = 99999;
    let seed_bytes = test_seed.to_le_bytes();

    let mut stdin = &seed_bytes[..];
    let mut stdout = Vec::new();

    let config = ProtocolSetupConfig::new(protocol, false); // CLIENT mode

    let result =
        setup_protocol(&mut stdout, &mut stdin, &config).expect("client setup should succeed");

    assert_eq!(
        result.checksum_seed, test_seed,
        "Client should read exact seed value from server"
    );
}

#[test]
fn setup_protocol_client_reads_negative_seed_from_server() {
    // Server may send a negative i32 seed (from u32::MAX cast)
    let protocol = ProtocolVersion::try_from(29).unwrap();
    let test_seed: i32 = -1; // This is what u32::MAX becomes
    let seed_bytes = test_seed.to_le_bytes();

    let mut stdin = &seed_bytes[..];
    let mut stdout = Vec::new();

    let config = ProtocolSetupConfig::new(protocol, false);

    let result = setup_protocol(&mut stdout, &mut stdin, &config)
        .expect("client setup with negative seed should succeed");

    assert_eq!(
        result.checksum_seed, -1,
        "Client should correctly read negative seed value"
    );
}

#[test]
fn client_has_pre_release_v_flag_detects_uppercase_v() {
    assert!(
        client_has_pre_release_v_flag("LsfxCIVu"),
        "'V' in capability string should be detected"
    );
}

#[test]
fn client_has_pre_release_v_flag_ignores_lowercase_v() {
    assert!(
        !client_has_pre_release_v_flag("LsfxCIvu"),
        "lowercase 'v' should not trigger pre-release detection"
    );
}

#[test]
fn client_has_pre_release_v_flag_empty_string() {
    assert!(
        !client_has_pre_release_v_flag(""),
        "empty string should not match"
    );
}

#[test]
fn write_compat_flags_uses_single_byte_for_pre_release_v() {
    // When 'V' is present, compat flags should be written as a single byte
    let flags = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::CHECKSUM_SEED_FIX;
    let mut output = Vec::new();

    let final_flags =
        write_compat_flags(&mut output, flags, "LsfxCIVu").expect("write should succeed");

    // Should be exactly 1 byte (write_byte, not write_varint)
    assert_eq!(
        output.len(),
        1,
        "pre-release 'V' should use single-byte encoding"
    );

    // CF_VARINT_FLIST_FLAGS should be implicitly set
    assert!(
        final_flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
        "V flag should implicitly enable VARINT_FLIST_FLAGS"
    );

    // The byte should include all original flags plus VARINT_FLIST_FLAGS
    let expected_bits = flags.bits() | CompatibilityFlags::VARINT_FLIST_FLAGS.bits();
    assert_eq!(
        output[0], expected_bits as u8,
        "written byte should contain original flags plus VARINT_FLIST_FLAGS"
    );
}

#[test]
fn write_compat_flags_uses_varint_for_lowercase_v() {
    // When only 'v' is present (no 'V'), use standard varint encoding
    let flags = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::CHECKSUM_SEED_FIX;
    let mut output = Vec::new();

    let final_flags =
        write_compat_flags(&mut output, flags, "LsfxCIvu").expect("write should succeed");

    // Should NOT implicitly add VARINT_FLIST_FLAGS (that's done by
    // build_compat_flags_from_client_info)
    assert_eq!(
        final_flags, flags,
        "lowercase 'v' should not implicitly modify flags"
    );

    // Verify varint encoding: 0x28 = SAFE_FILE_LIST(0x08) | CHECKSUM_SEED_FIX(0x20)
    // For values < 128, varint is a single byte matching the value
    assert_eq!(
        output,
        vec![0x28],
        "varint encoding of 0x28 should be [0x28]"
    );
}

#[test]
fn write_compat_flags_v_flag_single_byte_without_varint_readable() {
    // When 'V' is present but the total flags value stays below 0x80
    // (i.e. VARINT_FLIST_FLAGS is not yet set by 'v'), the single-byte
    // encoding IS compatible with read_varint because the 0x80 bit is off.
    // However, write_compat_flags always sets CF_VARINT_FLIST_FLAGS (0x80)
    // for 'V', making the high bit on. A pre-release 'V' client reads
    // with read_byte(), not read_varint(), so this is by design.
    // upstream: "read_varint() is compatible with the older write_byte()
    // when the 0x80 bit isn't on."
    let flags = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::CHECKSUM_SEED_FIX;
    let mut output = Vec::new();

    let written_flags =
        write_compat_flags(&mut output, flags, "LsfxCIVu").expect("write should succeed");

    // The value has CF_VARINT_FLIST_FLAGS set (0x80 bit on), so
    // read_varint would interpret this as a multi-byte varint and fail.
    // This is correct because pre-release 'V' clients use read_byte().
    assert_eq!(output.len(), 1, "should be single byte");
    assert_eq!(
        output[0],
        written_flags.bits() as u8,
        "byte should match the flag bits"
    );
    assert!(
        output[0] & 0x80 != 0,
        "high bit is set due to CF_VARINT_FLIST_FLAGS"
    );
}

#[test]
fn write_compat_flags_v_flag_low_value_readable_by_read_varint() {
    // When write_compat_flags produces a value < 0x80, read_varint can
    // decode it since the high bit is off and a varint with no extra
    // bytes is identical to a plain byte.
    // Use empty flags so the only bit is VARINT_FLIST_FLAGS from 'V'.
    // CF_VARINT_FLIST_FLAGS itself is 0x80 so this will always set the
    // high bit. We verify this invariant.
    let flags = CompatibilityFlags::EMPTY;
    let mut output = Vec::new();

    let written_flags = write_compat_flags(&mut output, flags, "V").expect("write should succeed");

    // Even with EMPTY input, 'V' sets CF_VARINT_FLIST_FLAGS (0x80)
    assert_eq!(written_flags.bits(), 0x80);
    assert_eq!(output, vec![0x80]);
}

#[test]
fn setup_protocol_server_v_flag_uses_single_byte_encoding() {
    // Server with 'V' capability client should use single-byte compat flags encoding
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdin = &b"\x03md5"[..]; // vstring: "md5" checksum list
    let mut stdout = Vec::new();

    // Client has 'V' (pre-release) instead of 'v'
    let client_args = ["-e.LsfxCIVu".to_owned()];
    let config = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: Some(&client_args),
        is_server: true,
        is_daemon_mode: true,
        do_compression: false,
        compress_choice: None,
        checksum_seed: None,
        allow_inc_recurse: false,
    };

    let result =
        setup_protocol(&mut stdout, &mut stdin, &config).expect("server setup should succeed");

    let flags = result.compat_flags.unwrap();
    assert!(
        flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
        "'V' should implicitly set VARINT_FLIST_FLAGS"
    );

    // First byte of output should be the compat flags (single byte encoding)
    // Remaining bytes are algorithm negotiation data + checksum seed
    assert!(
        !stdout.is_empty(),
        "Server should have written compat flags"
    );

    // The first byte is the compat flags in single-byte format
    let compat_byte = stdout[0];
    assert_eq!(
        compat_byte & CompatibilityFlags::VARINT_FLIST_FLAGS.bits() as u8,
        CompatibilityFlags::VARINT_FLIST_FLAGS.bits() as u8,
        "First byte should contain VARINT_FLIST_FLAGS"
    );
}

#[test]
fn setup_protocol_server_v_flag_enables_negotiation() {
    // 'V' should trigger algorithm negotiation just like 'v' does
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdin = &b"\x03md5"[..]; // vstring: "md5" checksum list
    let mut stdout = Vec::new();

    let client_args = ["-e.LsfxCIVu".to_owned()];
    let config = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: Some(&client_args),
        is_server: true,
        is_daemon_mode: true,
        do_compression: false,
        compress_choice: None,
        checksum_seed: None,
        allow_inc_recurse: false,
    };

    let result =
        setup_protocol(&mut stdout, &mut stdin, &config).expect("server setup should succeed");

    assert!(
        result.negotiated_algorithms.is_some(),
        "'V' should enable algorithm negotiation"
    );
}

#[test]
fn setup_protocol_server_v_flag_with_both_v_and_uppercase_v() {
    // If both 'v' and 'V' are present, 'V' takes precedence for encoding
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdin = &b"\x03md5"[..];
    let mut stdout_v = Vec::new();
    let mut stdout_both = Vec::new();

    // Only uppercase V
    let client_args_v = ["-e.LsfxCIVu".to_owned()];
    let config_v = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: Some(&client_args_v),
        is_server: true,
        is_daemon_mode: true,
        do_compression: false,
        compress_choice: None,
        checksum_seed: Some(42),
        allow_inc_recurse: false,
    };

    let result_v =
        setup_protocol(&mut stdout_v, &mut stdin, &config_v).expect("V-only setup should succeed");

    // Both v and V
    let mut stdin = &b"\x03md5"[..];
    let client_args_both = ["-e.LsfxCIvVu".to_owned()];
    let config_both = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: Some(&client_args_both),
        is_server: true,
        is_daemon_mode: true,
        do_compression: false,
        compress_choice: None,
        checksum_seed: Some(42),
        allow_inc_recurse: false,
    };

    let result_both = setup_protocol(&mut stdout_both, &mut stdin, &config_both)
        .expect("vV setup should succeed");

    // Both should have VARINT_FLIST_FLAGS set
    assert!(
        result_v
            .compat_flags
            .unwrap()
            .contains(CompatibilityFlags::VARINT_FLIST_FLAGS)
    );
    assert!(
        result_both
            .compat_flags
            .unwrap()
            .contains(CompatibilityFlags::VARINT_FLIST_FLAGS)
    );

    // Both should use single-byte encoding since 'V' is present in both
    assert_eq!(
        stdout_v[0], stdout_both[0],
        "Both should produce the same compat flags byte"
    );
}

#[test]
fn build_capability_string_without_inc_recurse() {
    let s = build_capability_string(false);
    assert!(s.starts_with("-e."), "must start with -e. prefix");
    assert!(!s.contains('i'), "must not contain 'i' without inc_recurse");
    // All non-conditional, iconv-independent platform chars must be present
    for ch in ['f', 'x', 'C', 'I', 'v', 'u'] {
        assert!(s.contains(ch), "missing capability char '{ch}'");
    }
}

#[cfg(feature = "iconv")]
#[test]
fn build_capability_string_includes_symlink_iconv_when_iconv_compiled_in() {
    // upstream: compat.c:716-718 - CF_SYMLINK_ICONV is gated on
    // `#ifdef ICONV_OPTION`. With the iconv feature enabled we must
    // advertise 's' so the peer runs `sender_symlink_iconv` against our
    // file-list reader's transcoding hook.
    let s = build_capability_string(false);
    assert!(s.contains('s'), "must contain 's' when iconv is enabled");
}

#[cfg(not(feature = "iconv"))]
#[test]
fn build_capability_string_omits_symlink_iconv_when_iconv_disabled() {
    // upstream: compat.c:716-718 - without ICONV_OPTION the build neither
    // sets nor accepts CF_SYMLINK_ICONV, so the capability string must
    // not contain 's' either.
    let s = build_capability_string(false);
    assert!(
        !s.contains('s'),
        "must not contain 's' when iconv feature is disabled: {s}"
    );
}

#[test]
fn build_capability_string_with_inc_recurse() {
    let s = build_capability_string(true);
    assert!(s.starts_with("-e."), "must start with -e. prefix");
    assert!(s.contains('i'), "must contain 'i' with inc_recurse");
}

#[test]
fn build_capability_string_matches_mapping_order() {
    let s = build_capability_string(true);
    let chars: String = s.strip_prefix("-e.").unwrap().to_owned();
    // Verify order matches CAPABILITY_MAPPINGS table, applying the same
    // build-time filters the production builder uses.
    let expected: String = CAPABILITY_MAPPINGS
        .iter()
        .filter(|m| m.platform_ok)
        .filter(|m| !m.requires_iconv || cfg!(feature = "iconv"))
        .map(|m| m.char)
        .collect();
    assert_eq!(chars, expected);
}

/// Mock negotiator that returns fixed values without wire I/O.
///
/// Used to verify that `setup_protocol_with` delegates correctly to the
/// trait methods rather than calling concrete protocol functions.
struct MockNegotiator {
    fixed_seed: i32,
    fixed_checksum: protocol::ChecksumAlgorithm,
    fixed_compression: protocol::CompressionAlgorithm,
    flags_to_write: CompatibilityFlags,
    flags_to_read: CompatibilityFlags,
}

impl MockNegotiator {
    fn new() -> Self {
        Self {
            fixed_seed: 777,
            fixed_checksum: protocol::ChecksumAlgorithm::MD5,
            fixed_compression: protocol::CompressionAlgorithm::None,
            flags_to_write: CompatibilityFlags::CHECKSUM_SEED_FIX
                | CompatibilityFlags::VARINT_FLIST_FLAGS,
            flags_to_read: CompatibilityFlags::CHECKSUM_SEED_FIX
                | CompatibilityFlags::VARINT_FLIST_FLAGS,
        }
    }
}

impl CompatFlagsExchanger for MockNegotiator {
    fn write_compat_flags(
        &self,
        _writer: &mut dyn std::io::Write,
        _flags: CompatibilityFlags,
        _client_info: &str,
    ) -> std::io::Result<CompatibilityFlags> {
        Ok(self.flags_to_write)
    }

    fn read_compat_flags(
        &self,
        _reader: &mut dyn std::io::Read,
    ) -> std::io::Result<CompatibilityFlags> {
        Ok(self.flags_to_read)
    }

    fn build_flags_from_client_info(
        &self,
        _client_info: &str,
        _allow_inc_recurse: bool,
    ) -> CompatibilityFlags {
        self.flags_to_write
    }

    fn parse_client_info<'a>(&self, _client_args: &'a [String]) -> std::borrow::Cow<'a, str> {
        std::borrow::Cow::Borrowed("fxCIvu")
    }

    fn has_pre_release_v_flag(&self, _client_info: &str) -> bool {
        false
    }
}

impl CapabilityNegotiator for MockNegotiator {
    fn negotiate(
        &self,
        _protocol: protocol::ProtocolVersion,
        _stdin: &mut dyn std::io::Read,
        _stdout: &mut dyn std::io::Write,
        config: &protocol::NegotiationConfig,
    ) -> std::io::Result<protocol::NegotiationResult> {
        // Honour the override when provided, matching the real negotiator
        let compression = config
            .compression_override
            .unwrap_or(self.fixed_compression);
        Ok(protocol::NegotiationResult {
            checksum: self.fixed_checksum,
            compression,
        })
    }
}

impl ChecksumSeedExchanger for MockNegotiator {
    fn write_seed(
        &self,
        _writer: &mut dyn std::io::Write,
        _fixed_seed: Option<u32>,
    ) -> std::io::Result<i32> {
        Ok(self.fixed_seed)
    }

    fn read_seed(&self, _reader: &mut dyn std::io::Read) -> std::io::Result<i32> {
        Ok(self.fixed_seed)
    }
}

#[test]
fn setup_protocol_with_mock_negotiator_server_mode() {
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let client_args = ["-efxCIvu".to_owned()];
    let config = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: Some(&client_args),
        is_server: true,
        is_daemon_mode: true,
        do_compression: false,
        compress_choice: None,
        checksum_seed: None,
        allow_inc_recurse: false,
    };

    let mock = MockNegotiator::new();
    let result = setup_protocol_with(&mut stdout, &mut stdin, &config, &mock)
        .expect("mock setup should succeed");

    assert_eq!(result.checksum_seed, 777, "should use mock's fixed seed");
    assert!(
        result.compat_flags.is_some(),
        "protocol 31 should exchange compat flags"
    );
    assert_eq!(
        result.compat_flags.unwrap(),
        CompatibilityFlags::CHECKSUM_SEED_FIX | CompatibilityFlags::VARINT_FLIST_FLAGS,
        "should return mock's flags"
    );
    assert!(
        result.negotiated_algorithms.is_some(),
        "should have negotiated algorithms from mock"
    );
    let algos = result.negotiated_algorithms.unwrap();
    assert_eq!(
        algos.checksum,
        protocol::ChecksumAlgorithm::MD5,
        "should use mock's checksum"
    );
}

#[test]
fn setup_protocol_with_mock_negotiator_client_mode() {
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let config = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: None,
        is_server: false,
        is_daemon_mode: false,
        do_compression: false,
        compress_choice: None,
        checksum_seed: None,
        allow_inc_recurse: false,
    };

    let mock = MockNegotiator::new();
    let result = setup_protocol_with(&mut stdout, &mut stdin, &config, &mock)
        .expect("mock client setup should succeed");

    assert_eq!(result.checksum_seed, 777);
    assert!(result.compat_flags.is_some());
    assert!(result.negotiated_algorithms.is_some());
}

#[test]
fn setup_protocol_with_mock_skips_compat_for_old_protocol() {
    let protocol = ProtocolVersion::try_from(29).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let config = ProtocolSetupConfig::new(protocol, true);

    let mock = MockNegotiator::new();
    let result = setup_protocol_with(&mut stdout, &mut stdin, &config, &mock)
        .expect("protocol 29 mock setup should succeed");

    assert!(
        result.compat_flags.is_none(),
        "protocol 29 should skip compat flags"
    );
    assert!(
        result.negotiated_algorithms.is_none(),
        "protocol 29 should skip negotiation"
    );
    assert_eq!(result.checksum_seed, 777);
}

#[test]
fn rsync_negotiator_implements_protocol_negotiator() {
    // Verify the default RsyncNegotiator satisfies the trait bound.
    fn assert_negotiator(_n: &dyn ProtocolNegotiator) {}
    assert_negotiator(&RsyncNegotiator);
}

#[test]
fn setup_protocol_delegates_to_default_negotiator() {
    // Verify setup_protocol (no _with) produces the same result as
    // setup_protocol_with using RsyncNegotiator.
    let protocol = ProtocolVersion::try_from(29).unwrap();
    let config = ProtocolSetupConfig::new(protocol, true).with_checksum_seed(Some(42));

    let mut stdout1 = Vec::new();
    let mut stdin1 = &b""[..];
    let result1 =
        setup_protocol(&mut stdout1, &mut stdin1, &config).expect("setup_protocol should succeed");

    let mut stdout2 = Vec::new();
    let mut stdin2 = &b""[..];
    let result2 = setup_protocol_with(&mut stdout2, &mut stdin2, &config, &RsyncNegotiator)
        .expect("setup_protocol_with should succeed");

    assert_eq!(result1.checksum_seed, result2.checksum_seed);
    assert_eq!(stdout1, stdout2);
}

/// When the client capability string includes 'x' (CF_AVOID_XATTR_OPTIM),
/// the server advertises xattr awareness. Clients use this flag to detect
/// that the remote peer supports xattrs.
///
/// upstream: compat.c:722 - server sets CF_AVOID_XATTR_OPTIM when compiled
/// with SUPPORT_XATTRS
#[test]
fn server_advertises_xattr_support_via_avoid_xattr_optim_flag() {
    let flags = build_compat_flags_from_client_info("LsfxCIvu", true);
    assert!(
        flags.contains(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION),
        "server should advertise CF_AVOID_XATTR_OPTIM when client sends 'x'"
    );
}

/// When the client capability string does NOT include 'x', the server
/// does not advertise xattr support. A client requesting --xattrs should
/// detect this and gracefully disable xattr preservation.
///
/// upstream: compat.c:722 - CF_AVOID_XATTR_OPTIM only set when client
/// advertises 'x' and server has SUPPORT_XATTRS
#[test]
fn server_omits_xattr_flag_when_client_lacks_x_capability() {
    let flags = build_compat_flags_from_client_info("LsfCIvu", true);
    assert!(
        !flags.contains(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION),
        "server should NOT advertise CF_AVOID_XATTR_OPTIM when client omits 'x'"
    );
}

/// The client reads compat flags from the server. When the server's flags
/// lack CF_AVOID_XATTR_OPTIM, the client should detect that the remote
/// does not support xattrs.
///
/// upstream: compat.c:780-785 - client detects missing xattr support
#[test]
fn client_detects_missing_xattr_support_from_server_flags() {
    // Server flags without CF_AVOID_XATTR_OPTIM
    let server_flags = CompatibilityFlags::CHECKSUM_SEED_FIX
        | CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::VARINT_FLIST_FLAGS;

    assert!(
        !server_flags.contains(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION),
        "server flags should lack CF_AVOID_XATTR_OPTIM"
    );

    // Server flags with CF_AVOID_XATTR_OPTIM
    let server_flags_with_xattr = server_flags | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION;
    assert!(
        server_flags_with_xattr.contains(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION),
        "server flags should have CF_AVOID_XATTR_OPTIM when added"
    );
}

/// Verify that when the client does NOT request ACLs or xattrs (i.e., flags
/// are false), no capability negotiation issues arise - the transfer proceeds
/// without any degradation warnings.
#[test]
fn no_degradation_when_client_does_not_request_acls_or_xattrs() {
    use crate::flags::ParsedServerFlags;

    let mut flags = ParsedServerFlags::parse("-rlt").unwrap();
    assert!(!flags.acls, "ACLs should not be set for -rlt");
    assert!(!flags.xattrs, "xattrs should not be set for -rlt");

    let cleared = flags.clear_unsupported_features();
    assert!(
        cleared.is_empty(),
        "nothing should be cleared when neither ACL nor xattr is requested"
    );
}

/// Protocol version < 30 must reject --acls for non-local transfers.
/// This is enforced at the protocol restriction layer.
///
/// upstream: compat.c:655-661 - ACLs require protocol 30+
#[test]
fn protocol_restriction_rejects_acls_below_protocol_30() {
    let flags = ProtocolRestrictionFlags {
        preserve_acls: true,
        local_server: false,
        ..Default::default()
    };
    let err =
        apply_protocol_restrictions(ProtocolVersion::try_from(29).unwrap(), &flags).unwrap_err();
    assert!(
        err.to_string().contains("--acls requires protocol 30"),
        "should reject ACLs below protocol 30: {err}"
    );
}

/// Protocol version < 30 must reject --xattrs for non-local transfers.
/// This is enforced at the protocol restriction layer.
///
/// upstream: compat.c:662-668 - xattrs require protocol 30+
#[test]
fn protocol_restriction_rejects_xattrs_below_protocol_30() {
    let flags = ProtocolRestrictionFlags {
        preserve_xattrs: true,
        local_server: false,
        ..Default::default()
    };
    let err =
        apply_protocol_restrictions(ProtocolVersion::try_from(29).unwrap(), &flags).unwrap_err();
    assert!(
        err.to_string().contains("--xattrs requires protocol 30"),
        "should reject xattrs below protocol 30: {err}"
    );
}

/// Protocol 30+ allows both ACLs and xattrs without error.
///
/// upstream: compat.c:652-668 - restrictions only apply below version 30
#[test]
fn protocol_30_allows_acls_and_xattrs() {
    let flags = ProtocolRestrictionFlags {
        preserve_acls: true,
        preserve_xattrs: true,
        local_server: false,
        ..Default::default()
    };
    assert!(
        apply_protocol_restrictions(ProtocolVersion::try_from(30).unwrap(), &flags,).is_ok(),
        "protocol 30 should allow both ACLs and xattrs"
    );
}

/// Local server (no remote connection) bypasses protocol version checks
/// for ACLs and xattrs. This mirrors upstream which only checks these
/// restrictions for remote connections.
///
/// upstream: compat.c:655,662 - `&& !local_server` guards
#[test]
fn local_server_allows_acls_and_xattrs_below_protocol_30() {
    let flags = ProtocolRestrictionFlags {
        preserve_acls: true,
        preserve_xattrs: true,
        local_server: true,
        ..Default::default()
    };
    assert!(
        apply_protocol_restrictions(ProtocolVersion::try_from(28).unwrap(), &flags,).is_ok(),
        "local server should allow ACLs and xattrs regardless of protocol version"
    );
}

/// When the server supports ACLs/xattrs (features enabled) but the client
/// does NOT include -A/-X in its flag string, the parsed flags should be
/// false. The transfer proceeds without metadata preservation.
#[test]
fn server_does_not_force_acl_xattr_when_client_omits_flags() {
    use crate::flags::ParsedServerFlags;

    let flags = ParsedServerFlags::parse("-logDtpre.iLsfxCIvu").unwrap();
    assert!(
        !flags.acls,
        "ACLs should not be set when client omits 'A' from flag string"
    );
    assert!(
        !flags.xattrs,
        "xattrs should not be set when client omits 'X' from flag string"
    );
}

/// When the client includes both -A and -X in its flag string, the server
/// should parse both and set the corresponding flags to true.
#[test]
fn server_parses_client_acl_and_xattr_flags() {
    use crate::flags::ParsedServerFlags;

    let flags = ParsedServerFlags::parse("-logDtprAXe.iLsfxCIvu").unwrap();
    assert!(flags.acls, "ACLs should be set when client sends 'A'");
    assert!(flags.xattrs, "xattrs should be set when client sends 'X'");
}

/// The capability string from build_capability_string always includes 'x'
/// (CF_AVOID_XATTR_OPTIM), since our build always compiles with xattr wire
/// protocol support. This is how remote peers detect xattr awareness.
///
/// upstream: options.c:3003-3050 - maybe_add_e_option() builds -e.xxx
#[test]
fn capability_string_always_includes_xattr_marker() {
    use super::build_capability_string;

    let cap = build_capability_string(true);
    assert!(
        cap.contains('x'),
        "capability string should include 'x' for xattr support: {cap}"
    );
}

/// The capability string does not contain A or X directly - those are
/// transfer flags in the compact flag string, not capability characters.
/// Capabilities use lowercase letters for different purposes.
#[test]
fn capability_string_does_not_contain_acl_xattr_transfer_flags() {
    use super::build_capability_string;

    let cap = build_capability_string(true);
    assert!(
        !cap.contains('A'),
        "capability string should not contain 'A' (ACL transfer flag): {cap}"
    );
    assert!(
        !cap.contains('X'),
        "capability string should not contain 'X' (xattr transfer flag): {cap}"
    );
}

// -- compress-choice negotiation tests --

#[test]
fn compress_choice_override_skips_vstring_and_uses_algorithm() {
    // upstream: compat.c:543 - when compress_choice is set, compression
    // vstring exchange is skipped and the algorithm is used directly.
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let client_args = ["-efxCIvu".to_owned()];
    let config = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: Some(&client_args),
        is_server: true,
        is_daemon_mode: true,
        do_compression: true,
        compress_choice: Some(protocol::CompressionAlgorithm::Zstd),
        checksum_seed: Some(42),
        allow_inc_recurse: false,
    };

    let mock = MockNegotiator::new();
    let result = setup_protocol_with(&mut stdout, &mut stdin, &config, &mock)
        .expect("compress-choice override setup should succeed");

    let algos = result.negotiated_algorithms.unwrap();
    assert_eq!(
        algos.compression,
        protocol::CompressionAlgorithm::Zstd,
        "compress_choice=zstd should override negotiated compression"
    );
}

#[test]
fn compress_choice_none_allows_normal_negotiation() {
    // When compress_choice is None and do_compression=true, normal vstring
    // negotiation should occur (send_compression=true).
    let protocol = ProtocolVersion::try_from(31).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let client_args = ["-efxCIvu".to_owned()];
    let config = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: Some(&client_args),
        is_server: true,
        is_daemon_mode: true,
        do_compression: true,
        compress_choice: None,
        checksum_seed: Some(42),
        allow_inc_recurse: false,
    };

    let mock = MockNegotiator::new();
    let result = setup_protocol_with(&mut stdout, &mut stdin, &config, &mock)
        .expect("normal negotiation should succeed");

    let algos = result.negotiated_algorithms.unwrap();
    // Mock returns CompressionAlgorithm::None when no override is given
    assert_eq!(
        algos.compression,
        protocol::CompressionAlgorithm::None,
        "without compress_choice, mock returns its default (None)"
    );
}

#[test]
fn compress_choice_zlib_override_on_legacy_protocol() {
    // upstream: compat.c:194-195 - legacy protocols always use zlib unless
    // the user explicitly chose an algorithm.
    let protocol = ProtocolVersion::try_from(29).unwrap();
    let mut stdin = &b""[..];
    let mut stdout = Vec::new();

    let config = ProtocolSetupConfig {
        protocol,
        skip_compat_exchange: false,
        client_args: None,
        is_server: true,
        is_daemon_mode: false,
        do_compression: true,
        compress_choice: Some(protocol::CompressionAlgorithm::ZlibX),
        checksum_seed: Some(42),
        allow_inc_recurse: false,
    };

    let result = setup_protocol(&mut stdout, &mut stdin, &config)
        .expect("legacy protocol with compress_choice should succeed");

    // For protocol < 30, negotiation is skipped but compress_choice is
    // honoured in the legacy path.
    let algos = result.negotiated_algorithms.unwrap();
    assert_eq!(
        algos.compression,
        protocol::CompressionAlgorithm::ZlibX,
        "compress_choice=zlibx should be used on legacy protocol"
    );
}

#[test]
fn with_compress_choice_builder_method() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    let config = ProtocolSetupConfig::new(protocol, true)
        .with_compress_choice(Some(protocol::CompressionAlgorithm::Zstd))
        .with_compression(true);

    assert_eq!(
        config.compress_choice,
        Some(protocol::CompressionAlgorithm::Zstd)
    );
    assert!(config.do_compression);
}

#[test]
fn with_compress_choice_none_clears_override() {
    let protocol = ProtocolVersion::try_from(31).unwrap();

    let config = ProtocolSetupConfig::new(protocol, true)
        .with_compress_choice(Some(protocol::CompressionAlgorithm::Zstd))
        .with_compress_choice(None);

    assert!(
        config.compress_choice.is_none(),
        "second with_compress_choice(None) should clear the override"
    );
}
