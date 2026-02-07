// Comprehensive tests for connection timeout (contimeout) support in the local
// copy engine.
//
// These tests cover:
// 1. Default contimeout value is None
// 2. Setting various durations (1s, 30s, 0s, large values)
// 3. Round-trip: set contimeout, verify accessor returns it
// 4. Builder support
// 5. Combination with regular timeout
// 6. Transfers work with contimeout set

// =============================================================================
// Default Value Tests
// =============================================================================

#[test]
fn contimeout_defaults_to_none() {
    let opts = LocalCopyOptions::new();
    assert!(opts.contimeout().is_none());
}

#[test]
fn contimeout_default_trait_returns_none() {
    let opts = LocalCopyOptions::default();
    assert!(opts.contimeout().is_none());
}

// =============================================================================
// Setting Various Durations
// =============================================================================

#[test]
fn contimeout_one_second() {
    let opts = LocalCopyOptions::new().with_contimeout(Some(Duration::from_secs(1)));
    assert_eq!(opts.contimeout(), Some(Duration::from_secs(1)));
}

#[test]
fn contimeout_thirty_seconds() {
    let opts = LocalCopyOptions::new().with_contimeout(Some(Duration::from_secs(30)));
    assert_eq!(opts.contimeout(), Some(Duration::from_secs(30)));
}

#[test]
fn contimeout_zero_seconds() {
    let opts = LocalCopyOptions::new().with_contimeout(Some(Duration::from_secs(0)));
    assert_eq!(opts.contimeout(), Some(Duration::ZERO));
}

#[test]
fn contimeout_large_value() {
    let duration = Duration::from_secs(86400); // 24 hours
    let opts = LocalCopyOptions::new().with_contimeout(Some(duration));
    assert_eq!(opts.contimeout(), Some(duration));
}

#[test]
fn contimeout_very_large_value() {
    let duration = Duration::from_secs(604800); // 7 days
    let opts = LocalCopyOptions::new().with_contimeout(Some(duration));
    assert_eq!(opts.contimeout(), Some(duration));
}

#[test]
fn contimeout_subsecond_precision() {
    let duration = Duration::from_millis(500);
    let opts = LocalCopyOptions::new().with_contimeout(Some(duration));
    assert_eq!(opts.contimeout(), Some(duration));
}

// =============================================================================
// Round-Trip Tests
// =============================================================================

#[test]
fn contimeout_round_trip_set_and_get() {
    let duration = Duration::from_secs(45);
    let opts = LocalCopyOptions::new().with_contimeout(Some(duration));
    assert_eq!(opts.contimeout(), Some(duration));
}

#[test]
fn contimeout_round_trip_set_then_clear() {
    let opts = LocalCopyOptions::new()
        .with_contimeout(Some(Duration::from_secs(30)))
        .with_contimeout(None);
    assert!(opts.contimeout().is_none());
}

#[test]
fn contimeout_last_write_wins() {
    let opts = LocalCopyOptions::new()
        .with_contimeout(Some(Duration::from_secs(10)))
        .with_contimeout(Some(Duration::from_secs(30)))
        .with_contimeout(Some(Duration::from_secs(60)));
    assert_eq!(opts.contimeout(), Some(Duration::from_secs(60)));
}

// =============================================================================
// Builder Support Tests
// =============================================================================

#[test]
fn builder_contimeout_defaults_to_none() {
    let opts = LocalCopyOptions::builder().build().expect("valid options");
    assert!(opts.contimeout().is_none());
}

#[test]
fn builder_contimeout_sets_value() {
    let duration = Duration::from_secs(15);
    let opts = LocalCopyOptions::builder()
        .contimeout(Some(duration))
        .build()
        .expect("valid options");
    assert_eq!(opts.contimeout(), Some(duration));
}

#[test]
fn builder_contimeout_none_clears_value() {
    let opts = LocalCopyOptions::builder()
        .contimeout(Some(Duration::from_secs(30)))
        .contimeout(None)
        .build()
        .expect("valid options");
    assert!(opts.contimeout().is_none());
}

#[test]
fn builder_contimeout_zero() {
    let opts = LocalCopyOptions::builder()
        .contimeout(Some(Duration::ZERO))
        .build()
        .expect("valid options");
    assert_eq!(opts.contimeout(), Some(Duration::ZERO));
}

#[test]
fn builder_contimeout_large_value() {
    let duration = Duration::from_secs(86400);
    let opts = LocalCopyOptions::builder()
        .contimeout(Some(duration))
        .build()
        .expect("valid options");
    assert_eq!(opts.contimeout(), Some(duration));
}

#[test]
fn builder_unchecked_contimeout_sets_value() {
    let duration = Duration::from_secs(45);
    let opts = LocalCopyOptions::builder()
        .contimeout(Some(duration))
        .build_unchecked();
    assert_eq!(opts.contimeout(), Some(duration));
}

// =============================================================================
// Combination with Regular Timeout Tests
// =============================================================================

#[test]
fn contimeout_independent_of_timeout() {
    let contimeout = Duration::from_secs(10);
    let timeout = Duration::from_secs(60);
    let opts = LocalCopyOptions::new()
        .with_contimeout(Some(contimeout))
        .with_timeout(Some(timeout));
    assert_eq!(opts.contimeout(), Some(contimeout));
    assert_eq!(opts.timeout(), Some(timeout));
}

#[test]
fn clearing_contimeout_preserves_timeout() {
    let timeout = Duration::from_secs(60);
    let opts = LocalCopyOptions::new()
        .with_contimeout(Some(Duration::from_secs(10)))
        .with_timeout(Some(timeout))
        .with_contimeout(None);
    assert!(opts.contimeout().is_none());
    assert_eq!(opts.timeout(), Some(timeout));
}

#[test]
fn clearing_timeout_preserves_contimeout() {
    let contimeout = Duration::from_secs(10);
    let opts = LocalCopyOptions::new()
        .with_contimeout(Some(contimeout))
        .with_timeout(Some(Duration::from_secs(60)))
        .with_timeout(None);
    assert_eq!(opts.contimeout(), Some(contimeout));
    assert!(opts.timeout().is_none());
}

#[test]
fn both_timeouts_and_stop_at_coexist() {
    let contimeout = Duration::from_secs(10);
    let timeout = Duration::from_secs(60);
    let deadline = SystemTime::now();
    let opts = LocalCopyOptions::new()
        .with_contimeout(Some(contimeout))
        .with_timeout(Some(timeout))
        .with_stop_at(Some(deadline));
    assert_eq!(opts.contimeout(), Some(contimeout));
    assert_eq!(opts.timeout(), Some(timeout));
    assert!(opts.stop_at().is_some());
}

#[test]
fn builder_both_timeouts_coexist() {
    let contimeout = Duration::from_secs(5);
    let timeout = Duration::from_secs(120);
    let opts = LocalCopyOptions::builder()
        .contimeout(Some(contimeout))
        .timeout(Some(timeout))
        .build()
        .expect("valid options");
    assert_eq!(opts.contimeout(), Some(contimeout));
    assert_eq!(opts.timeout(), Some(timeout));
}

// =============================================================================
// Transfer with Contimeout Tests
// =============================================================================

#[test]
fn transfer_works_with_contimeout_set() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"hello contimeout");

    let options = LocalCopyOptions::default()
        .with_contimeout(Some(Duration::from_secs(30)));

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execute");

    assert!(ctx.dest_exists("file.txt"));
    assert_eq!(ctx.read_dest("file.txt"), b"hello contimeout");
    assert!(summary.files_copied() >= 1);
}

#[test]
fn transfer_works_with_contimeout_and_timeout() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"dual timeout");

    let options = LocalCopyOptions::default()
        .with_contimeout(Some(Duration::from_secs(10)))
        .with_timeout(Some(Duration::from_secs(60)));

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execute");

    assert!(ctx.dest_exists("file.txt"));
    assert_eq!(ctx.read_dest("file.txt"), b"dual timeout");
    assert!(summary.files_copied() >= 1);
}

#[test]
fn transfer_works_with_zero_contimeout() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"zero contimeout");

    let options = LocalCopyOptions::default()
        .with_contimeout(Some(Duration::ZERO));

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execute");

    assert!(ctx.dest_exists("file.txt"));
    assert_eq!(ctx.read_dest("file.txt"), b"zero contimeout");
    assert!(summary.files_copied() >= 1);
}

// =============================================================================
// Connection Timeout Exit Code Tests
// =============================================================================

#[test]
fn connection_timeout_exit_code_is_35() {
    assert_eq!(super::filter_program::CONNECTION_TIMEOUT_EXIT_CODE, 35);
}

#[test]
fn connection_timeout_exit_code_distinct_from_io_timeout() {
    assert_ne!(
        super::filter_program::CONNECTION_TIMEOUT_EXIT_CODE,
        super::filter_program::TIMEOUT_EXIT_CODE,
    );
}

#[test]
fn connection_timeout_exit_code_matches_upstream_rerr_contimeout() {
    // RERR_CONTIMEOUT = 35 in upstream rsync's errcode.h
    const RERR_CONTIMEOUT: i32 = 35;
    assert_eq!(
        super::filter_program::CONNECTION_TIMEOUT_EXIT_CODE,
        RERR_CONTIMEOUT,
    );
}
