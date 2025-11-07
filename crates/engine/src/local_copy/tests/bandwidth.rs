
#[test]
fn execute_with_bandwidth_limit_records_sleep() {
    let mut recorder = oc_rsync_bandwidth::recorded_sleep_session();
    recorder.clear();

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");
    fs::write(&source, vec![0xAA; 4 * 1024]).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().bandwidth_limit(Some(NonZeroU64::new(1024).unwrap()));
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest").len(), 4 * 1024);

    let recorded = recorder.take();
    assert!(
        !recorded.is_empty(),
        "expected bandwidth limiter to schedule sleeps"
    );
    let total = recorded
        .into_iter()
        .fold(Duration::ZERO, |acc, duration| acc + duration);
    let expected = Duration::from_secs(4);
    let diff = total.abs_diff(expected);
    assert!(
        diff <= Duration::from_millis(50),
        "expected sleep duration near {expected:?}, got {total:?}"
    );
    let summary_sleep = summary.bandwidth_sleep();
    let summary_diff = summary_sleep.abs_diff(total);
    assert!(
        summary_diff <= Duration::from_millis(50),
        "summary recorded {summary_sleep:?} of throttling while sleeps totalled {total:?}"
    );
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn execute_with_append_appends_missing_bytes() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"abcdef").expect("write source");
    fs::write(&destination, b"abc").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append(true),
        )
        .expect("append succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), b"abcdef");
    assert_eq!(summary.bytes_copied(), 3);
    assert_eq!(summary.matched_bytes(), 3);
}

#[test]
fn execute_with_append_verify_rewrites_on_mismatch() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"abcdef").expect("write source");
    fs::write(&destination, b"abx").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().append_verify(true),
        )
        .expect("append verify succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), b"abcdef");
    assert_eq!(summary.bytes_copied(), 6);
    assert_eq!(summary.matched_bytes(), 0);
}

#[test]
fn bandwidth_limiter_limits_chunk_size_for_slow_rates() {
    let limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    assert_eq!(limiter.recommended_read_size(COPY_BUFFER_SIZE), 512);
    assert_eq!(limiter.recommended_read_size(256), 256);
}

#[test]
fn bandwidth_limiter_preserves_buffer_for_fast_rates() {
    let limiter = BandwidthLimiter::new(NonZeroU64::new(8 * 1024 * 1024).unwrap());
    assert_eq!(
        limiter.recommended_read_size(COPY_BUFFER_SIZE),
        COPY_BUFFER_SIZE
    );
}

#[test]
fn execute_without_bandwidth_limit_does_not_sleep() {
    let mut recorder = oc_rsync_bandwidth::recorded_sleep_session();
    recorder.clear();

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"no limit").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan.execute().expect("copy succeeds");

    assert_eq!(fs::read(destination).expect("read dest"), b"no limit");
    assert!(
        recorder.take().is_empty(),
        "unexpected sleep durations recorded"
    );
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bandwidth_sleep(), Duration::ZERO);
}

#[test]
fn execute_with_compression_records_compressed_bytes() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");
    let content = vec![b'A'; 16 * 1024];
    fs::write(&source, &content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().compress(true),
        )
        .expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), content);
    assert!(summary.compression_used());
    let compressed = summary.compressed_bytes();
    assert!(compressed > 0);
    assert!(compressed <= summary.bytes_copied());
    assert_eq!(summary.bytes_sent(), summary.bytes_received());
    assert_eq!(summary.bytes_sent(), compressed);
    assert_eq!(summary.bandwidth_sleep(), Duration::ZERO);
}

#[test]
fn execute_records_transmitted_bytes_for_uncompressed_copy() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let payload = b"payload";
    fs::write(&source, payload).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");

    assert_eq!(fs::read(destination).expect("read dest"), payload);
    let expected = payload.len() as u64;
    assert_eq!(summary.bytes_copied(), expected);
    assert_eq!(summary.bytes_sent(), expected);
    assert_eq!(summary.bytes_received(), expected);
    assert_eq!(summary.matched_bytes(), 0);
}

#[test]
fn execute_with_compression_limits_post_compress_bandwidth() {
    let mut recorder = oc_rsync_bandwidth::recorded_sleep_session();
    recorder.clear();

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");
    let mut content = Vec::new();
    for _ in 0..4096 {
        content.extend_from_slice(b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. \n");
    }
    fs::write(&source, &content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let limit = NonZeroU64::new(2 * 1024).expect("limit");
    let options = LocalCopyOptions::default()
        .compress(true)
        .bandwidth_limit(Some(limit));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), content);
    assert!(summary.compression_used());

    let compressed = summary.compressed_bytes();
    assert!(compressed > 0);
    let transferred = summary.bytes_copied();

    let sleeps = recorder.take();
    assert!(
        !sleeps.is_empty(),
        "bandwidth limiter did not record sleeps"
    );
    let total_sleep_secs: f64 = sleeps.iter().map(|duration| duration.as_secs_f64()).sum();

    let summary_sleep = summary.bandwidth_sleep();
    assert!(summary_sleep > Duration::ZERO);
    let summary_secs = summary_sleep.as_secs_f64();

    let expected_compressed = compressed as f64 / limit.get() as f64;
    let expected_uncompressed = transferred as f64 / limit.get() as f64;

    let tolerance = expected_compressed * 0.2 + 0.2;
    assert!(
        (total_sleep_secs - expected_compressed).abs() <= tolerance,
        "sleep {total_sleep_secs:?}s deviates too far from compressed expectation {expected_compressed:?}s",
    );
    assert!(
        (summary_secs - total_sleep_secs).abs() <= tolerance,
        "summary tracked {summary_secs:?}s while recordings totalled {total_sleep_secs:?}s",
    );
    assert!(
        (total_sleep_secs - expected_compressed).abs()
            < (total_sleep_secs - expected_uncompressed).abs(),
        "sleep {total_sleep_secs:?}s should align with compressed bytes ({expected_compressed:?}s) rather than uncompressed ({expected_uncompressed:?}s)",
    );
}
