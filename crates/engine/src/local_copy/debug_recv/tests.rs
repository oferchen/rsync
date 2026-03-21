use std::time::Duration;

use super::*;

#[test]
fn test_tracer_new() {
    let tracer = RecvTracer::new();
    assert_eq!(tracer.files_received(), 0);
    assert_eq!(tracer.bytes_received(), 0);
    assert_eq!(tracer.matched_bytes(), 0);
    assert_eq!(tracer.literal_bytes(), 0);
    assert_eq!(tracer.basis_selections(), 0);
    assert_eq!(tracer.checksum_matches(), 0);
    assert_eq!(tracer.checksum_mismatches(), 0);
    assert_eq!(tracer.current_file_elapsed(), Duration::ZERO);
    assert_eq!(tracer.session_elapsed(), Duration::ZERO);
}

#[test]
fn test_tracer_default() {
    let tracer = RecvTracer::default();
    assert_eq!(tracer.files_received(), 0);
    assert_eq!(tracer.bytes_received(), 0);
}

#[test]
fn test_start_file_initializes_timing() {
    let mut tracer = RecvTracer::new();
    tracer.start_file("test.txt", 1024, 0);

    std::thread::sleep(Duration::from_millis(1));
    assert!(tracer.current_file_elapsed() > Duration::ZERO);
    assert!(tracer.session_elapsed() > Duration::ZERO);
}

#[test]
fn test_record_match_accumulates() {
    let mut tracer = RecvTracer::new();
    tracer.record_match(0, 0, 1024);
    tracer.record_match(1, 1024, 2048);
    tracer.record_match(2, 3072, 512);

    assert_eq!(tracer.matched_bytes(), 3584);
}

#[test]
fn test_record_literal_accumulates() {
    let mut tracer = RecvTracer::new();
    tracer.record_literal(0, 256);
    tracer.record_literal(256, 512);
    tracer.record_literal(768, 128);

    assert_eq!(tracer.literal_bytes(), 896);
}

#[test]
fn test_record_basis_increments() {
    let mut tracer = RecvTracer::new();
    tracer.record_basis("file1.txt", "/basis/file1.txt", 1024);
    tracer.record_basis("file2.txt", "/basis/file2.txt", 2048);

    assert_eq!(tracer.basis_selections(), 2);
}

#[test]
fn test_record_checksum_verify_matches() {
    let mut tracer = RecvTracer::new();
    tracer.record_checksum_verify(true);
    tracer.record_checksum_verify(true);

    assert_eq!(tracer.checksum_matches(), 2);
    assert_eq!(tracer.checksum_mismatches(), 0);
}

#[test]
fn test_record_checksum_verify_mismatches() {
    let mut tracer = RecvTracer::new();
    tracer.record_checksum_verify(false);
    tracer.record_checksum_verify(false);

    assert_eq!(tracer.checksum_matches(), 0);
    assert_eq!(tracer.checksum_mismatches(), 2);
}

#[test]
fn test_record_checksum_verify_mixed() {
    let mut tracer = RecvTracer::new();
    tracer.record_checksum_verify(true);
    tracer.record_checksum_verify(false);
    tracer.record_checksum_verify(true);

    assert_eq!(tracer.checksum_matches(), 2);
    assert_eq!(tracer.checksum_mismatches(), 1);
}

#[test]
fn test_end_file_increments_counts() {
    let mut tracer = RecvTracer::new();
    tracer.start_file("file1.txt", 1024, 0);
    tracer.end_file("file1.txt", 512);

    assert_eq!(tracer.files_received(), 1);
    assert_eq!(tracer.bytes_received(), 512);
}

#[test]
fn test_multiple_files() {
    let mut tracer = RecvTracer::new();

    tracer.start_file("file1.txt", 2048, 0);
    tracer.record_basis("file1.txt", "/basis/file1.txt", 1024);
    tracer.record_match(0, 0, 1024);
    tracer.record_literal(1024, 256);
    tracer.record_checksum_verify(true);
    tracer.end_file("file1.txt", 1280);

    tracer.start_file("file2.txt", 4096, 1);
    tracer.record_basis("file2.txt", "/basis/file2.txt", 2048);
    tracer.record_match(0, 0, 2048);
    tracer.record_literal(2048, 512);
    tracer.record_checksum_verify(true);
    tracer.end_file("file2.txt", 2560);

    assert_eq!(tracer.files_received(), 2);
    assert_eq!(tracer.bytes_received(), 3840);
    assert_eq!(tracer.matched_bytes(), 3072);
    assert_eq!(tracer.literal_bytes(), 768);
    assert_eq!(tracer.basis_selections(), 2);
    assert_eq!(tracer.checksum_matches(), 2);
}

#[test]
fn test_reset_clears_state() {
    let mut tracer = RecvTracer::new();
    tracer.start_file("test.txt", 1024, 0);
    tracer.record_basis("test.txt", "/basis/test.txt", 512);
    tracer.record_match(0, 0, 512);
    tracer.record_literal(512, 256);
    tracer.record_checksum_verify(true);
    tracer.end_file("test.txt", 768);

    tracer.reset();

    assert_eq!(tracer.files_received(), 0);
    assert_eq!(tracer.bytes_received(), 0);
    assert_eq!(tracer.matched_bytes(), 0);
    assert_eq!(tracer.literal_bytes(), 0);
    assert_eq!(tracer.basis_selections(), 0);
    assert_eq!(tracer.checksum_matches(), 0);
    assert_eq!(tracer.checksum_mismatches(), 0);
    assert_eq!(tracer.current_file_elapsed(), Duration::ZERO);
    assert_eq!(tracer.session_elapsed(), Duration::ZERO);
}

#[test]
fn test_summary_returns_elapsed() {
    let mut tracer = RecvTracer::new();
    tracer.start_file("file.txt", 1024, 0);
    std::thread::sleep(Duration::from_millis(5));
    tracer.end_file("file.txt", 1024);

    let elapsed = tracer.summary();
    assert!(elapsed >= Duration::from_millis(5));
}

#[test]
fn test_zero_size_file() {
    let mut tracer = RecvTracer::new();
    tracer.start_file("empty.txt", 0, 0);
    tracer.end_file("empty.txt", 0);

    assert_eq!(tracer.files_received(), 1);
    assert_eq!(tracer.bytes_received(), 0);
}

#[test]
fn test_saturating_add_bytes_received() {
    let mut tracer = RecvTracer::new();
    tracer.bytes_received = u64::MAX - 100;
    tracer.start_file("huge.bin", 1024, 0);
    tracer.end_file("huge.bin", 200);

    assert_eq!(tracer.bytes_received(), u64::MAX);
}

#[test]
fn test_saturating_add_matched_bytes() {
    let mut tracer = RecvTracer::new();
    tracer.matched_bytes = u64::MAX - 50;
    tracer.record_match(0, 0, 100);

    assert_eq!(tracer.matched_bytes(), u64::MAX);
}

#[test]
fn test_saturating_add_literal_bytes() {
    let mut tracer = RecvTracer::new();
    tracer.literal_bytes = u64::MAX - 50;
    tracer.record_literal(0, 100);

    assert_eq!(tracer.literal_bytes(), u64::MAX);
}

#[test]
fn test_trace_functions_do_not_panic() {
    trace_recv_file_start("test.txt", 1024, 0);
    trace_recv_file_end("test.txt", 512, Duration::from_millis(100));
    trace_basis_file_selected("test.txt", "/basis/test.txt", 1024);
    trace_delta_apply_start("test.txt", 2048, 1024);
    trace_delta_apply_match(5, 4096, 512);
    trace_delta_apply_literal(4608, 128);
    trace_delta_apply_end("test.txt", 2048, Duration::from_millis(50));
    trace_checksum_verify("test.txt", &[0x12, 0x34], &[0x12, 0x34], true);
    trace_recv_summary(10, 10240, Duration::from_secs(1));
}

#[test]
fn test_end_file_without_start_file() {
    let mut tracer = RecvTracer::new();
    tracer.end_file("test.txt", 1024);

    assert_eq!(tracer.files_received(), 1);
    assert_eq!(tracer.bytes_received(), 1024);
    assert_eq!(tracer.current_file_elapsed(), Duration::ZERO);
}

#[test]
fn test_summary_without_files() {
    let mut tracer = RecvTracer::new();
    let elapsed = tracer.summary();

    assert_eq!(tracer.files_received(), 0);
    assert_eq!(tracer.bytes_received(), 0);
    assert_eq!(elapsed, Duration::ZERO);
}

#[test]
fn test_multiple_start_file_calls() {
    let mut tracer = RecvTracer::new();
    tracer.start_file("file1.txt", 1024, 0);
    let first_start = tracer.current_file_start;

    std::thread::sleep(Duration::from_millis(1));
    tracer.start_file("file2.txt", 2048, 1);
    let second_start = tracer.current_file_start;

    assert_ne!(first_start, second_start);
    assert!(tracer.session_elapsed() > Duration::ZERO);
}

#[test]
fn test_large_match_literal_counts() {
    let mut tracer = RecvTracer::new();
    tracer.start_file("largefile.bin", 1_000_000, 0);

    for i in 0..1000 {
        tracer.record_match(i, (i as u64) * 1024, 1024);
    }

    for i in 0..500 {
        tracer.record_literal((i as u64) * 2048, 512);
    }

    tracer.end_file("largefile.bin", 1_280_000);

    assert_eq!(tracer.matched_bytes(), 1_024_000);
    assert_eq!(tracer.literal_bytes(), 256_000);
    assert_eq!(tracer.bytes_received(), 1_280_000);
    assert_eq!(tracer.files_received(), 1);
}

#[test]
fn test_empty_transfer() {
    let mut tracer = RecvTracer::new();
    let elapsed = tracer.summary();

    assert_eq!(tracer.files_received(), 0);
    assert_eq!(tracer.bytes_received(), 0);
    assert_eq!(tracer.matched_bytes(), 0);
    assert_eq!(tracer.literal_bytes(), 0);
    assert_eq!(elapsed, Duration::ZERO);
}

#[test]
fn test_file_without_basis() {
    let mut tracer = RecvTracer::new();
    tracer.start_file("newfile.txt", 1024, 0);
    tracer.record_literal(0, 1024);
    tracer.record_checksum_verify(true);
    tracer.end_file("newfile.txt", 1024);

    assert_eq!(tracer.files_received(), 1);
    assert_eq!(tracer.bytes_received(), 1024);
    assert_eq!(tracer.matched_bytes(), 0);
    assert_eq!(tracer.literal_bytes(), 1024);
    assert_eq!(tracer.basis_selections(), 0);
}

#[cfg(feature = "tracing")]
#[test]
fn test_tracing_feature_enabled() {
    let mut tracer = RecvTracer::new();
    tracer.start_file("traced.txt", 1024, 0);
    tracer.record_basis("traced.txt", "/basis/traced.txt", 512);
    tracer.record_match(0, 0, 512);
    tracer.record_literal(512, 256);
    tracer.record_checksum_verify(true);
    tracer.end_file("traced.txt", 768);

    assert_eq!(tracer.files_received(), 1);
    assert_eq!(tracer.bytes_received(), 768);
    assert_eq!(tracer.matched_bytes(), 512);
    assert_eq!(tracer.literal_bytes(), 256);
    assert_eq!(tracer.checksum_matches(), 1);
}
