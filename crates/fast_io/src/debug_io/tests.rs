//! Tests for debug_io tracing functions.

use super::*;

#[cfg(feature = "tracing")]
mod tracing_tests {
    use super::*;

    #[test]
    fn test_format_hex_dump_empty() {
        assert_eq!(format::format_hex_dump(&[]), "<empty>");
    }

    #[test]
    fn test_format_hex_dump_ascii() {
        let data = b"Hello";
        let result = format::format_hex_dump(data);
        assert_eq!(result, "48 65 6c 6c 6f |Hello|");
    }

    #[test]
    fn test_format_hex_dump_binary() {
        let data = [0x00, 0x01, 0x02, 0xff];
        let result = format::format_hex_dump(&data);
        assert_eq!(result, "00 01 02 ff |....|");
    }

    #[test]
    fn test_format_hex_dump_mixed() {
        let data = [0x48, 0x69, 0x00, 0x21]; // "Hi" + null + "!"
        let result = format::format_hex_dump(&data);
        assert_eq!(result, "48 69 00 21 |Hi.!|");
    }
}

// These tests verify that the no-op functions compile and can be called
// without the tracing feature enabled
#[test]
fn test_trace_functions_compile() {
    // Level 1
    trace_open("/test/path", 1024);
    trace_close("/test/path");
    trace_create("/test/path", Some(1024));
    trace_create("/test/path", None);

    // Level 2
    trace_read("/test/path", 512, 1024);
    trace_write("/test/path", 256);
    trace_seek("/test/path", 0, 100);
    trace_sync("/test/path");
    trace_mmap("/test/path", 4096);
    trace_munmap("/test/path");

    // Level 3
    trace_buffer_acquire(4096, 3);
    trace_buffer_release(4096);
    trace_buffer_pool_create(8, 4096);
    trace_buffer_state("test", 0, 100);
    trace_io_uring_submit("read", 5, 0, 1024);
    trace_io_uring_complete(1024, 0x42);
    trace_mmap_advise("/test/path", "sequential", 0, 0);

    // Level 4
    trace_bytes_read(&[1, 2, 3], 0);
    trace_bytes_written(&[1, 2, 3], 0);
    trace_data_pattern("test pattern", &[1, 2, 3]);
}
