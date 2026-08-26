#[test]
fn execute_with_bandwidth_limit_records_sleep() {
    let mut recorder = bandwidth::recorded_sleep_session();
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
    // Windows CI runners have 15.6ms timer resolution + scheduling jitter;
    // nightly builds may show additional variance from debug assertions
    let tolerance = Duration::from_millis(750);
    assert!(
        diff <= tolerance,
        "expected sleep duration near {expected:?}, got {total:?} (diff {diff:?})"
    );
    let summary_sleep = summary.bandwidth_sleep();
    let summary_diff = summary_sleep.abs_diff(total);
    assert!(
        summary_diff <= tolerance,
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

    // Zero, not three. MEASURED against rsync 3.4.4 on this exact fixture
    // (`-a --append --ignore-times --stats`, source "abcdef", dest "abc"):
    // `Literal data: 3 bytes`, `Matched data: 0 bytes`.
    //
    // The 3-byte prefix the append skipped is neither literal nor matched.
    // Asserting 3 here encoded the old derivation `file_size - literal_bytes`,
    // which treats every non-literal byte as matched; upstream only ever
    // increments `stats.matched_data` inside `matched()` (match.c:121), which
    // append mode never reaches (match.c:389-390).
    assert_eq!(summary.matched_bytes(), 0);
}

#[test]
fn execute_with_append_verify_appends_then_redoes_on_mismatch() {
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

    // MEASURED against rsync 3.4.4 on this exact fixture
    // (`-a --append-verify --ignore-times --stats`, source "abcdef", seed
    // "abx"): 2 transfers, `Literal data: 9 bytes`, `Matched data: 0 bytes`.
    //
    // Nine literal bytes, not six: pass one appends the 3-byte tail, then the
    // failed whole-file re-checksum retains it and the redo re-sends all 6
    // bytes as literal, because the 6-byte basis is one short block that does
    // not match. Asserting 6 would be asserting that the append never happened.
    assert_eq!(summary.bytes_copied(), 9);

    // Zero matched bytes, matching upstream. The 3-byte prefix pass one
    // appended past is neither literal nor matched: upstream never calls
    // `matched()` in append mode (match.c:389-390 sets
    // `last_match = s->flength; s->count = 0;` so the hash loop is skipped),
    // and `stats.matched_data` grows nowhere else (match.c:121). Pass two is an
    // ordinary delta whose single 6-byte basis block does not match, so it
    // contributes nothing either.
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
    let mut recorder = bandwidth::recorded_sleep_session();
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
    // A local copy records data as sent, not received (it emulates the sender).
    assert_eq!(summary.bytes_received(), 0);
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
    // A local copy records the data as sent, not received: it emulates the
    // protocol sender, which writes the data and reads back only small replies.
    assert_eq!(summary.bytes_received(), 0);
    assert_eq!(summary.matched_bytes(), 0);
}

#[test]
fn execute_with_compression_limits_post_compress_bandwidth() {
    let mut recorder = bandwidth::recorded_sleep_session();
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
        .with_compression_algorithm(CompressionAlgorithm::Zlib)
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

/// A delta-against-basis local copy must pace the bandwidth limiter on the
/// emitted wire-token volume - the small literal payload for the unmatched
/// runs - and NOT on the full source size. When the destination is a near
/// copy of the source, almost every block is satisfied by a basis match, so
/// only the divergent tail is written as literal data; the throttle must track
/// that literal volume, keeping a mostly-matching large file quick under a
/// tight limit.
///
/// This is why it matters: upstream `sleep_for_bwlimit(n)` accounts exactly the
/// bytes just written to the multiplexed socket by `send_token()` - the literal
/// data plus tiny block-reference tokens - never the whole basis-matched file.
/// Pacing on the full literal-read or source size would over-throttle a delta
/// transfer whose actual wire volume is small.
///
/// upstream: io.c:861 `sleep_for_bwlimit(n)`; token.c:`send_token()` writes the
/// literal length + bytes and the negative block-reference tokens that `n`
/// counts.
#[test]
fn delta_bandwidth_paces_wire_tokens_not_source_size() {
    let mut recorder = bandwidth::recorded_sleep_session();
    recorder.clear();

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    // 512 KiB source; the basis (destination) shares the first 480 KiB
    // byte-for-byte and diverges only in the final 32 KiB. The delta matcher
    // reuses the shared prefix as block matches and emits only the ~32 KiB
    // tail as literal (wire-token) data.
    const TOTAL: usize = 512 * 1024;
    const DIVERGENT_TAIL: usize = 32 * 1024;
    let mut source_payload = vec![0u8; TOTAL];
    for (i, byte) in source_payload.iter_mut().enumerate() {
        // Deterministic, non-repeating fill so blocks carry distinct checksums.
        *byte = (i as u32).wrapping_mul(2_654_435_761).to_le_bytes()[i % 4];
    }
    let mut basis_payload = source_payload.clone();
    for byte in basis_payload[TOTAL - DIVERGENT_TAIL..].iter_mut() {
        *byte ^= 0xFF;
    }

    fs::write(&source, &source_payload).expect("write source");
    fs::write(&destination, &basis_payload).expect("write basis");
    // Backdate the basis so the size+mtime quick-check does not skip the copy.
    set_file_mtime(&destination, FileTime::from_unix_time(1_000_000_000, 0))
        .expect("backdate basis");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // 64 KiB/s: the ~32 KiB literal tail throttles for ~0.5s, whereas pacing on
    // the full 512 KiB would stall for ~8s - a decisive separation.
    let limit = NonZeroU64::new(64 * 1024).expect("limit");
    let options = LocalCopyOptions::default()
        .whole_file(false)
        .bandwidth_limit(Some(limit));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("delta copy succeeds");

    // The destination must be reconstructed byte-for-byte from basis + literals.
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        source_payload,
        "delta copy must reconstruct the source exactly"
    );

    // Delta engaged: most of the file came from basis block matches, only the
    // divergent tail became literal wire data. `bytes_copied` is the literal
    // volume the limiter was fed (see `record_file`), so it must sit well below
    // the full file size - proving this was not a whole-file re-send.
    assert!(
        summary.matched_bytes() > 0,
        "expected block matches against the basis, got matched_bytes=0"
    );
    let literal = summary.bytes_copied();
    assert!(
        literal < TOTAL as u64,
        "expected literal volume below the full file size, got {literal} of {TOTAL}"
    );

    let sleeps = recorder.take();
    assert!(!sleeps.is_empty(), "bandwidth limiter recorded no sleeps");
    let total_sleep_secs: f64 = sleeps.iter().map(|duration| duration.as_secs_f64()).sum();

    let expected_literal_secs = literal as f64 / limit.get() as f64;
    let expected_fullsize_secs = TOTAL as f64 / limit.get() as f64;

    // Tolerance mirrors the compression pacing test: proportional slack plus a
    // fixed floor for timer resolution and scheduling jitter.
    let tolerance = expected_literal_secs * 0.25 + 0.25;
    assert!(
        (total_sleep_secs - expected_literal_secs).abs() <= tolerance,
        "sleep {total_sleep_secs:.3}s deviates from literal expectation {expected_literal_secs:.3}s (tol {tolerance:.3}s)"
    );
    // The decisive fidelity check: the throttle aligns with the small literal
    // volume, unmistakably closer to it than to the full source size.
    assert!(
        (total_sleep_secs - expected_literal_secs).abs()
            < (total_sleep_secs - expected_fullsize_secs).abs(),
        "sleep {total_sleep_secs:.3}s should track literal bytes ({expected_literal_secs:.3}s) not full size ({expected_fullsize_secs:.3}s)"
    );
    // The summary's own throttle accounting must match the recorded sleeps.
    let summary_secs = summary.bandwidth_sleep().as_secs_f64();
    assert!(
        (summary_secs - total_sleep_secs).abs() <= tolerance,
        "summary tracked {summary_secs:.3}s while recordings totalled {total_sleep_secs:.3}s"
    );
}
