use super::*;
use std::fs::File;
use std::io::Write;
use tempfile::NamedTempFile;

use crate::constants::MAX_MAP_SIZE;

fn create_test_file(size: usize) -> NamedTempFile {
    let mut file = NamedTempFile::new().unwrap();
    let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    file.write_all(&data).unwrap();
    file.flush().unwrap();
    file
}

#[test]
fn open_file() {
    let temp = create_test_file(1000);
    let map = MapFile::open(temp.path()).unwrap();
    assert_eq!(map.file_size(), 1000);
}

#[test]
fn map_ptr_returns_correct_data() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open(temp.path()).unwrap();

    let data = map.map_ptr(0, 10).unwrap();
    assert_eq!(data, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
}

#[test]
fn map_ptr_mid_file() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open(temp.path()).unwrap();

    let data = map.map_ptr(500, 10).unwrap();
    let expected: Vec<u8> = (500..510).map(|i| (i % 256) as u8).collect();
    assert_eq!(data, &expected[..]);
}

#[test]
fn map_ptr_sequential_reads_use_cache() {
    let temp = create_test_file(10000);
    let mut map = MapFile::open(temp.path()).unwrap();

    let _ = map.map_ptr(0, 100).unwrap();

    for offset in (100..5000).step_by(100) {
        let data = map.map_ptr(offset as u64, 100).unwrap();
        let expected: Vec<u8> = (offset..offset + 100).map(|i| (i % 256) as u8).collect();
        assert_eq!(data, &expected[..]);
    }
}

#[test]
fn map_ptr_zero_length() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open(temp.path()).unwrap();

    let data = map.map_ptr(500, 0).unwrap();
    assert!(data.is_empty());
}

#[test]
fn map_ptr_past_eof_fails() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open(temp.path()).unwrap();

    let result = map.map_ptr(900, 200);
    assert!(result.is_err());
}

#[test]
fn map_ptr_window_slides_forward() {
    let temp = create_test_file(MAX_MAP_SIZE * 3);
    let mut map = MapFile::open(temp.path()).unwrap();

    let data1 = map.map_ptr(0, 100).unwrap();
    assert_eq!(data1[0], 0);

    let offset = (MAX_MAP_SIZE * 2) as u64;
    let data2 = map.map_ptr(offset, 100).unwrap();
    let expected_start = (offset % 256) as u8;
    assert_eq!(data2[0], expected_start);
}

#[test]
fn buffered_map_with_custom_window() {
    let temp = create_test_file(10000);
    let map = BufferedMap::open_with_window(temp.path(), 1024).unwrap();
    assert_eq!(map.window_size(), 1024);
}

#[test]
fn from_file_works() {
    let temp = create_test_file(1000);
    let file = File::open(temp.path()).unwrap();
    let mut map = MapFile::from_file(file).unwrap();

    let data = map.map_ptr(0, 10).unwrap();
    assert_eq!(data, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
}

#[test]
fn alignment_respected() {
    let temp = create_test_file(5000);
    let mut map = BufferedMap::open_with_window(temp.path(), 4096).unwrap();

    let _ = map.map_ptr(1500, 100).unwrap();

    assert_eq!(map.window_start, crate::constants::align_down(1500));
    assert_eq!(map.window_start, 1024);
}

#[cfg(unix)]
#[test]
fn mmap_strategy_open_and_read() {
    let temp = create_test_file(1000);
    let mut strategy = MmapStrategy::open(temp.path()).unwrap();

    assert_eq!(strategy.file_size(), 1000);

    let data = strategy.map_ptr(0, 10).unwrap();
    assert_eq!(data, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
}

#[cfg(unix)]
#[test]
fn mmap_strategy_mid_file_read() {
    let temp = create_test_file(1000);
    let mut strategy = MmapStrategy::open(temp.path()).unwrap();

    let data = strategy.map_ptr(500, 10).unwrap();
    let expected: Vec<u8> = (500..510).map(|i| (i % 256) as u8).collect();
    assert_eq!(data, &expected[..]);
}

#[cfg(unix)]
#[test]
fn mmap_strategy_zero_length_read() {
    let temp = create_test_file(1000);
    let mut strategy = MmapStrategy::open(temp.path()).unwrap();

    let data = strategy.map_ptr(500, 0).unwrap();
    assert!(data.is_empty());
}

#[cfg(unix)]
#[test]
fn mmap_strategy_past_eof_fails() {
    let temp = create_test_file(1000);
    let mut strategy = MmapStrategy::open(temp.path()).unwrap();

    let result = strategy.map_ptr(900, 200);
    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn mmap_strategy_window_size_is_file_size() {
    let temp = create_test_file(5000);
    let strategy = MmapStrategy::open(temp.path()).unwrap();

    assert_eq!(strategy.window_size(), 5000);
}

#[cfg(unix)]
#[test]
fn mmap_strategy_as_slice() {
    let temp = create_test_file(100);
    let strategy = MmapStrategy::open(temp.path()).unwrap();

    let slice = strategy.as_slice();
    assert_eq!(slice.len(), 100);
    assert_eq!(slice[0], 0);
    assert_eq!(slice[99], 99);
}

#[cfg(unix)]
#[test]
fn map_file_open_mmap() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    assert_eq!(map.file_size(), 1000);

    let data = map.map_ptr(0, 10).unwrap();
    assert_eq!(data, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_uses_buffered_for_small_files() {
    let temp = create_test_file(100);
    let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), 1000).unwrap();

    assert!(strategy.is_buffered());
    assert!(!strategy.is_mmap());
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_uses_mmap_for_large_files() {
    let temp = create_test_file(2000);
    let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), 1000).unwrap();

    assert!(strategy.is_mmap());
    assert!(!strategy.is_buffered());
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_threshold_boundary() {
    let temp = create_test_file(1000);
    let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), 1000).unwrap();

    assert!(strategy.is_mmap());
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_reads_correctly_buffered() {
    let temp = create_test_file(100);
    let mut strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), 1000).unwrap();

    let data = strategy.map_ptr(50, 10).unwrap();
    let expected: Vec<u8> = (50..60).collect();
    assert_eq!(data, &expected[..]);
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_reads_correctly_mmap() {
    let temp = create_test_file(2000);
    let mut strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), 1000).unwrap();

    let data = strategy.map_ptr(500, 10).unwrap();
    let expected: Vec<u8> = (500..510).map(|i| (i % 256) as u8).collect();
    assert_eq!(data, &expected[..]);
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_file_size() {
    let temp = create_test_file(5000);
    let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

    assert_eq!(strategy.file_size(), 5000);
}

#[cfg(unix)]
#[test]
fn map_file_open_adaptive() {
    let temp = create_test_file(100);
    let map = MapFile::open_adaptive(temp.path()).unwrap();

    assert_eq!(map.file_size(), 100);
    assert!(map.is_buffered());
}

#[cfg(unix)]
#[test]
fn map_file_open_adaptive_with_threshold() {
    let temp = create_test_file(100);
    let map = MapFile::open_adaptive_with_threshold(temp.path(), 50).unwrap();

    assert!(map.is_mmap());
}

#[cfg(unix)]
#[test]
fn map_file_adaptive_data_access() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open_adaptive(temp.path()).unwrap();

    let data = map.map_ptr(0, 10).unwrap();
    assert_eq!(data, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
}

#[cfg(unix)]
#[test]
fn adaptive_default_threshold() {
    let temp = create_test_file(500_000);
    let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

    assert!(strategy.is_buffered());
}

#[test]
fn empty_file_open_buffered() {
    let temp = create_test_file(0);
    let map = MapFile::open(temp.path()).unwrap();
    assert_eq!(map.file_size(), 0);
    assert_eq!(map.window_size(), MAX_MAP_SIZE);
}

#[cfg(unix)]
#[test]
fn empty_file_open_mmap() {
    let temp = create_test_file(0);
    let map = MapFile::open_mmap(temp.path()).unwrap();
    assert_eq!(map.file_size(), 0);
    assert_eq!(map.window_size(), 0);
}

#[cfg(unix)]
#[test]
fn empty_file_open_adaptive() {
    let temp = create_test_file(0);
    let map = MapFile::open_adaptive(temp.path()).unwrap();
    assert_eq!(map.file_size(), 0);
    assert!(map.is_buffered());
}

#[test]
fn empty_file_map_ptr_zero_length_succeeds() {
    let temp = create_test_file(0);
    let mut map = MapFile::open(temp.path()).unwrap();

    let data = map.map_ptr(0, 0).unwrap();
    assert!(data.is_empty());
}

#[test]
fn empty_file_map_ptr_nonzero_length_fails() {
    let temp = create_test_file(0);
    let mut map = MapFile::open(temp.path()).unwrap();

    let result = map.map_ptr(0, 1);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[cfg(unix)]
#[test]
fn empty_file_mmap_zero_length_succeeds() {
    let temp = create_test_file(0);
    let mut strategy = MmapStrategy::open(temp.path()).unwrap();

    let data = strategy.map_ptr(0, 0).unwrap();
    assert!(data.is_empty());
}

#[cfg(unix)]
#[test]
fn empty_file_mmap_nonzero_length_fails() {
    let temp = create_test_file(0);
    let mut strategy = MmapStrategy::open(temp.path()).unwrap();

    let result = strategy.map_ptr(0, 1);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn large_file_exceeds_single_window_buffered() {
    let size = MAX_MAP_SIZE * 2 + 1000;
    let temp = create_test_file(size);
    let mut map = MapFile::open(temp.path()).unwrap();

    assert_eq!(map.file_size(), size as u64);

    let data1 = map.map_ptr(0, 100).unwrap();
    assert_eq!(data1[0], 0);

    let mid_offset = (MAX_MAP_SIZE + 1000) as u64;
    let data2 = map.map_ptr(mid_offset, 100).unwrap();
    assert_eq!(data2[0], (mid_offset % 256) as u8);

    let end_offset = (size - 100) as u64;
    let data3 = map.map_ptr(end_offset, 100).unwrap();
    assert_eq!(data3[0], (end_offset % 256) as u8);
}

#[cfg(unix)]
#[test]
fn large_file_mmap_strategy() {
    let size = MMAP_THRESHOLD as usize + 1024;
    let temp = create_test_file(size);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    assert_eq!(map.file_size(), size as u64);

    let data1 = map.map_ptr(0, 100).unwrap();
    assert_eq!(data1[0], 0);

    let mid_offset = (size / 2) as u64;
    let data2 = map.map_ptr(mid_offset, 100).unwrap();
    assert_eq!(data2[0], (mid_offset % 256) as u8);

    let end_offset = (size - 100) as u64;
    let data3 = map.map_ptr(end_offset, 100).unwrap();
    assert_eq!(data3[0], (end_offset % 256) as u8);
}

#[cfg(unix)]
#[test]
fn large_file_adaptive_uses_mmap() {
    let size = MMAP_THRESHOLD as usize + 1024;
    let temp = create_test_file(size);
    let map = MapFile::open_adaptive(temp.path()).unwrap();

    assert!(map.is_mmap());
    assert_eq!(map.file_size(), size as u64);
}

#[test]
fn large_file_sequential_access_across_windows() {
    let size = MAX_MAP_SIZE * 4;
    let temp = create_test_file(size);
    let mut map = MapFile::open(temp.path()).unwrap();

    let chunk_size = 1000;
    for offset in (0..size).step_by(MAX_MAP_SIZE / 2) {
        if offset + chunk_size > size {
            break;
        }
        let data = map.map_ptr(offset as u64, chunk_size).unwrap();
        let expected: Vec<u8> = (offset..offset + chunk_size)
            .map(|i| (i % 256) as u8)
            .collect();
        assert_eq!(data, &expected[..], "Mismatch at offset {offset}");
    }
}

#[test]
fn large_file_random_access_pattern() {
    let size = MAX_MAP_SIZE * 3;
    let temp = create_test_file(size);
    let mut map = MapFile::open(temp.path()).unwrap();

    let offsets = [
        0,
        MAX_MAP_SIZE * 2,
        1000,
        MAX_MAP_SIZE + 500,
        MAX_MAP_SIZE * 2 + 1000,
        500,
    ];

    for &offset in &offsets {
        if offset + 100 > size {
            continue;
        }
        let data = map.map_ptr(offset as u64, 100).unwrap();
        assert_eq!(data[0], (offset % 256) as u8, "Mismatch at offset {offset}");
    }
}

#[test]
fn window_slides_forward_correctly() {
    let size = MAX_MAP_SIZE * 2;
    let temp = create_test_file(size);
    let mut map = BufferedMap::open(temp.path()).unwrap();

    let _ = map.map_ptr(0, 100).unwrap();
    assert_eq!(map.window_start, 0);

    let far_offset = (MAX_MAP_SIZE + 1000) as u64;
    let _ = map.map_ptr(far_offset, 100).unwrap();

    assert!(map.window_start > 0);
    assert!(map.window_start <= far_offset);
}

#[test]
fn window_slide_forward_reuses_overlap() {
    let size = 8192;
    let temp = create_test_file(size);
    let mut map = BufferedMap::open_with_window(temp.path(), 4096).unwrap();

    let _ = map.map_ptr(0, 100).unwrap();
    assert_eq!(map.window_start, 0);
    assert!(map.window_len > 0);

    // Read at offset 3000 with len 2000 requires window to cover [3000, 5000)
    // which exceeds current window [0, 4096). The new window should retain
    // overlapping bytes via copy_within instead of re-reading from disk.
    let data = map.map_ptr(3000, 2000).unwrap();
    let expected: Vec<u8> = (3000..5000).map(|i| (i % 256) as u8).collect();
    assert_eq!(data, &expected[..]);
}

#[test]
fn window_can_slide_backward() {
    let size = MAX_MAP_SIZE * 2;
    let temp = create_test_file(size);
    let mut map = BufferedMap::open(temp.path()).unwrap();

    let far_offset = (MAX_MAP_SIZE + 1000) as u64;
    let _ = map.map_ptr(far_offset, 100).unwrap();
    let first_window_start = map.window_start;

    let _ = map.map_ptr(0, 100).unwrap();
    assert!(map.window_start < first_window_start);
    assert_eq!(map.window_start, 0);
}

#[test]
fn window_respects_alignment_boundary() {
    let temp = create_test_file(10000);
    let mut map = BufferedMap::open_with_window(temp.path(), 2048).unwrap();

    let test_offsets = [100, 3000, 5500, 8000];

    for &offset in &test_offsets {
        let _ = map.map_ptr(offset, 100).unwrap();
        assert_eq!(
            map.window_start % crate::constants::ALIGN_BOUNDARY as u64,
            0,
            "Window start not on alignment boundary (start={}, boundary={}, offset={})",
            map.window_start,
            crate::constants::ALIGN_BOUNDARY,
            offset
        );
        assert!(
            offset >= map.window_start && offset < map.window_start + map.window_len as u64,
            "Offset {} not in window [{}, {})",
            offset,
            map.window_start,
            map.window_start + map.window_len as u64
        );
    }
}

#[test]
fn cache_hit_within_window() {
    let temp = create_test_file(10000);
    let mut map = BufferedMap::open(temp.path()).unwrap();

    let _ = map.map_ptr(0, 100).unwrap();
    let initial_window_start = map.window_start;
    let initial_window_len = map.window_len;

    for offset in [100, 200, 500, 1000] {
        let _ = map.map_ptr(offset as u64, 100).unwrap();
        assert_eq!(map.window_start, initial_window_start);
        assert_eq!(map.window_len, initial_window_len);
    }
}

#[test]
fn cache_miss_outside_window() {
    let size = MAX_MAP_SIZE * 2;
    let temp = create_test_file(size);
    let mut map = BufferedMap::open(temp.path()).unwrap();

    let _ = map.map_ptr(0, 100).unwrap();

    let far_offset = (MAX_MAP_SIZE + 1000) as u64;
    let _ = map.map_ptr(far_offset, 100).unwrap();

    assert!(map.window_start > 0);
}

#[test]
fn small_custom_window_size() {
    let temp = create_test_file(10000);
    let mut map = BufferedMap::open_with_window(temp.path(), 2048).unwrap();
    assert_eq!(map.window_size(), 2048);

    let data = map.map_ptr(0, 100).unwrap();
    assert_eq!(data[0], 0);

    let data = map.map_ptr(1000, 100).unwrap();
    assert_eq!(data[0], (1000 % 256) as u8);
}

#[test]
fn large_custom_window_size() {
    let window_size = MAX_MAP_SIZE * 2;
    let file_size = window_size + 10000;
    let temp = create_test_file(file_size);
    let mut map = BufferedMap::open_with_window(temp.path(), window_size).unwrap();
    assert_eq!(map.window_size(), window_size);

    let data1 = map.map_ptr(0, 100).unwrap();
    assert_eq!(data1[0], 0);

    let offset = MAX_MAP_SIZE + 1000;
    let data2 = map.map_ptr(offset as u64, 100).unwrap();
    assert_eq!(data2[0], (offset % 256) as u8);
}

#[test]
fn from_file_with_custom_window() {
    let temp = create_test_file(10000);
    let file = File::open(temp.path()).unwrap();
    let map = BufferedMap::from_file_with_window(file, 2048).unwrap();
    assert_eq!(map.window_size(), 2048);
    assert_eq!(map.file_size(), 10000);
}

#[test]
fn file_not_found_error_buffered() {
    let result = MapFile::open("/nonexistent/path/to/file.txt");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

#[cfg(unix)]
#[test]
fn file_not_found_error_mmap() {
    let result = MapFile::<MmapStrategy>::open_mmap("/nonexistent/path/to/file.txt");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

#[cfg(unix)]
#[test]
fn file_not_found_error_adaptive() {
    let result = MapFile::<AdaptiveMapStrategy>::open_adaptive("/nonexistent/path/to/file.txt");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn buffered_map_file_not_found() {
    let result = BufferedMap::open("/nonexistent/path/to/file.txt");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

#[cfg(unix)]
#[test]
fn mmap_strategy_file_not_found() {
    let result = MmapStrategy::open("/nonexistent/path/to/file.txt");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_file_not_found() {
    let result = AdaptiveMapStrategy::open("/nonexistent/path/to/file.txt");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

#[test]
#[cfg(unix)]
fn permission_denied_error_buffered() {
    use std::os::unix::fs::PermissionsExt;
    let dir = test_support::create_tempdir();
    let path = dir.path().join("no_read.txt");
    std::fs::write(&path, b"test data").unwrap();

    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&path, perms).unwrap();

    let result = MapFile::open(&path);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );

    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&path, perms).unwrap();
}

#[test]
#[cfg(unix)]
fn permission_denied_error_mmap() {
    use std::os::unix::fs::PermissionsExt;
    let dir = test_support::create_tempdir();
    let path = dir.path().join("no_read.txt");
    std::fs::write(&path, b"test data").unwrap();

    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&path, perms).unwrap();

    let result = MmapStrategy::open(&path);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );

    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&path, perms).unwrap();
}

#[test]
fn map_ptr_offset_at_eof() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open(temp.path()).unwrap();

    let data = map.map_ptr(1000, 0).unwrap();
    assert!(data.is_empty());
}

#[test]
fn map_ptr_offset_past_eof() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open(temp.path()).unwrap();

    let result = map.map_ptr(1001, 10);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn map_ptr_read_extends_past_eof() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open(temp.path()).unwrap();

    let result = map.map_ptr(950, 100);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[cfg(unix)]
#[test]
fn mmap_map_ptr_offset_past_eof() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    let result = map.map_ptr(1001, 10);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn single_byte_file_buffered() {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&[42]).unwrap();
    file.flush().unwrap();

    let mut map = MapFile::open(file.path()).unwrap();
    assert_eq!(map.file_size(), 1);

    let data = map.map_ptr(0, 1).unwrap();
    assert_eq!(data, &[42]);
}

#[cfg(unix)]
#[test]
fn single_byte_file_mmap() {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&[42]).unwrap();
    file.flush().unwrap();

    let mut map = MapFile::open_mmap(file.path()).unwrap();
    assert_eq!(map.file_size(), 1);

    let data = map.map_ptr(0, 1).unwrap();
    assert_eq!(data, &[42]);
}

#[test]
fn read_exactly_file_size() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open(temp.path()).unwrap();

    let data = map.map_ptr(0, 1000).unwrap();
    assert_eq!(data.len(), 1000);

    let expected: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
    assert_eq!(data, &expected[..]);
}

#[test]
fn read_last_byte() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open(temp.path()).unwrap();

    let data = map.map_ptr(999, 1).unwrap();
    assert_eq!(data, &[(999 % 256) as u8]);
}

#[test]
fn many_sequential_small_reads() {
    let temp = create_test_file(10000);
    let mut map = MapFile::open(temp.path()).unwrap();

    for offset in (0..9900).step_by(10) {
        let data = map.map_ptr(offset as u64, 10).unwrap();
        let expected: Vec<u8> = (offset..offset + 10).map(|i| (i % 256) as u8).collect();
        assert_eq!(data, &expected[..]);
    }
}

#[test]
fn overlapping_reads() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open(temp.path()).unwrap();

    let data1 = map.map_ptr(0, 100).unwrap().to_vec();
    let data2 = map.map_ptr(50, 100).unwrap().to_vec();
    let data3 = map.map_ptr(25, 50).unwrap().to_vec();

    assert_eq!(&data1[50..100], &data2[0..50]);
    assert_eq!(&data1[25..75], &data3[..]);
}

#[test]
fn binary_data_with_null_bytes() {
    let mut file = NamedTempFile::new().unwrap();
    let data = vec![0u8, 1, 2, 0, 0, 3, 0, 4, 0, 0, 0, 5];
    file.write_all(&data).unwrap();
    file.flush().unwrap();

    let mut map = MapFile::open(file.path()).unwrap();
    let read_data = map.map_ptr(0, data.len()).unwrap();
    assert_eq!(read_data, &data[..]);
}

#[test]
fn all_byte_values() {
    let mut file = NamedTempFile::new().unwrap();
    let data: Vec<u8> = (0..=255).collect();
    file.write_all(&data).unwrap();
    file.flush().unwrap();

    let mut map = MapFile::open(file.path()).unwrap();
    let read_data = map.map_ptr(0, 256).unwrap();
    assert_eq!(read_data, &data[..]);
}

#[cfg(unix)]
#[test]
fn binary_data_mmap() {
    let mut file = NamedTempFile::new().unwrap();
    let data: Vec<u8> = (0..=255).collect();
    file.write_all(&data).unwrap();
    file.flush().unwrap();

    let mut map = MapFile::open_mmap(file.path()).unwrap();
    let read_data = map.map_ptr(0, 256).unwrap();
    assert_eq!(read_data, &data[..]);
}

#[test]
fn map_strategy_file_size_buffered() {
    let temp = create_test_file(5000);
    let map = BufferedMap::open(temp.path()).unwrap();
    assert_eq!(map.file_size(), 5000);
}

#[test]
fn map_strategy_window_size_buffered() {
    let temp = create_test_file(1000);
    let map = BufferedMap::open(temp.path()).unwrap();
    assert_eq!(map.window_size(), MAX_MAP_SIZE);
}

#[test]
fn map_file_with_strategy_buffered() {
    let temp = create_test_file(1000);
    let strategy = BufferedMap::open(temp.path()).unwrap();
    let mut map = MapFile::with_strategy(strategy);

    assert_eq!(map.file_size(), 1000);
    let data = map.map_ptr(0, 10).unwrap();
    assert_eq!(data, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
}

#[cfg(unix)]
#[test]
fn map_file_with_strategy_mmap() {
    let temp = create_test_file(1000);
    let strategy = MmapStrategy::open(temp.path()).unwrap();
    let mut map = MapFile::with_strategy(strategy);

    assert_eq!(map.file_size(), 1000);
    let data = map.map_ptr(0, 10).unwrap();
    assert_eq!(data, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
}

#[test]
fn stress_alternating_start_end_reads_buffered() {
    let size = MAX_MAP_SIZE * 2;
    let temp = create_test_file(size);
    let mut map = MapFile::open(temp.path()).unwrap();

    for _ in 0..10 {
        let data_start = map.map_ptr(0, 100).unwrap();
        assert_eq!(data_start[0], 0);

        let end_offset = (size - 100) as u64;
        let data_end = map.map_ptr(end_offset, 100).unwrap();
        assert_eq!(data_end[0], (end_offset % 256) as u8);
    }
}

#[cfg(unix)]
#[test]
fn stress_alternating_start_end_reads_mmap() {
    let size = MAX_MAP_SIZE * 2;
    let temp = create_test_file(size);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    for _ in 0..10 {
        let data_start = map.map_ptr(0, 100).unwrap();
        assert_eq!(data_start[0], 0);

        let end_offset = (size - 100) as u64;
        let data_end = map.map_ptr(end_offset, 100).unwrap();
        assert_eq!(data_end[0], (end_offset % 256) as u8);
    }
}

#[test]
fn stress_many_window_reloads() {
    let size = MAX_MAP_SIZE * 4;
    let temp = create_test_file(size);
    let mut map = BufferedMap::open_with_window(temp.path(), 2048).unwrap();

    for i in 0..100 {
        let offset = ((i * 500) % (size - 100)) as u64;
        let data = map.map_ptr(offset, 100).unwrap();
        assert_eq!(data[0], (offset % 256) as u8);
    }
}

#[cfg(unix)]
#[test]
fn threshold_boundary_one_below() {
    let threshold = 1000u64;
    let temp = create_test_file((threshold - 1) as usize);
    let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), threshold).unwrap();

    assert!(strategy.is_buffered());
}

#[cfg(unix)]
#[test]
fn threshold_boundary_exactly_at() {
    let threshold = 1000u64;
    let temp = create_test_file(threshold as usize);
    let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), threshold).unwrap();

    assert!(strategy.is_mmap());
}

#[cfg(unix)]
#[test]
fn threshold_boundary_one_above() {
    let threshold = 1000u64;
    let temp = create_test_file((threshold + 1) as usize);
    let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), threshold).unwrap();

    assert!(strategy.is_mmap());
}

#[test]
fn default_threshold_value() {
    assert_eq!(MMAP_THRESHOLD, 1024 * 1024);
}

#[cfg(unix)]
#[test]
fn large_file_2mb_mmap_sequential_access() {
    let size = 2 * 1024 * 1024;
    let temp = create_test_file(size);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    assert_eq!(map.file_size(), size as u64);
    assert_eq!(map.window_size(), size);

    let chunk_size = 4096;
    for offset in (0..size).step_by(chunk_size * 10) {
        if offset + chunk_size > size {
            break;
        }
        let data = map.map_ptr(offset as u64, chunk_size).unwrap();
        assert_eq!(data.len(), chunk_size);
        assert_eq!(data[0], (offset % 256) as u8);
    }
}

#[cfg(unix)]
#[test]
fn large_file_5mb_mmap_random_access() {
    let size = 5 * 1024 * 1024;
    let temp = create_test_file(size);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    let test_offsets = [0, 1024, size / 4, size / 2, 3 * size / 4, size - 1024];

    for &offset in &test_offsets {
        let data = map.map_ptr(offset as u64, 1024).unwrap();
        assert_eq!(data[0], (offset % 256) as u8);
    }
}

#[cfg(unix)]
#[test]
fn large_file_8mb_adaptive_uses_mmap() {
    let size = 8 * 1024 * 1024;
    let temp = create_test_file(size);
    let map = MapFile::open_adaptive(temp.path()).unwrap();

    assert!(map.is_mmap());
    assert_eq!(map.file_size(), size as u64);
}

#[cfg(unix)]
#[test]
fn large_file_buffered_vs_mmap_correctness() {
    let size = 3 * 1024 * 1024;
    let temp = create_test_file(size);

    let mut buffered = MapFile::open(temp.path()).unwrap();
    let mut mmap = MapFile::open_mmap(temp.path()).unwrap();

    for offset in (0..size).step_by(512 * 1024) {
        if offset + 1024 > size {
            break;
        }
        let buf_data = buffered.map_ptr(offset as u64, 1024).unwrap().to_vec();
        let mmap_data = mmap.map_ptr(offset as u64, 1024).unwrap();

        assert_eq!(buf_data, mmap_data, "Data mismatch at offset {offset}");
    }
}

#[cfg(unix)]
#[test]
fn large_file_mmap_read_last_page() {
    let size = 4 * 1024 * 1024;
    let temp = create_test_file(size);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    let last_page_offset = size - 4096;
    let data = map.map_ptr(last_page_offset as u64, 4096).unwrap();
    assert_eq!(data.len(), 4096);
    assert_eq!(data[0], (last_page_offset % 256) as u8);
}

#[cfg(unix)]
#[test]
fn large_file_mmap_strided_access() {
    let size = 2 * 1024 * 1024;
    let temp = create_test_file(size);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    let block_size = 8192;
    let stride = 64 * 1024;

    for i in 0..((size / stride) - 1) {
        let offset = i * stride;
        if offset + block_size > size {
            break;
        }
        let data = map.map_ptr(offset as u64, block_size).unwrap();
        assert_eq!(data.len(), block_size);
        assert_eq!(data[0], (offset % 256) as u8);
    }
}

#[cfg(unix)]
#[test]
fn mmap_send_trait() {
    fn assert_send<T: Send>() {}
    assert_send::<MmapStrategy>();
}

#[test]
fn buffered_send_trait() {
    fn assert_send<T: Send>() {}
    assert_send::<BufferedMap>();
}

#[cfg(unix)]
#[test]
fn adaptive_send_trait() {
    fn assert_send<T: Send>() {}
    assert_send::<AdaptiveMapStrategy>();
}

#[cfg(unix)]
#[test]
fn multiple_readers_same_file_mmap() {
    let size = 1024 * 1024;
    let temp = create_test_file(size);

    let mut reader1 = MapFile::open_mmap(temp.path()).unwrap();
    let mut reader2 = MapFile::open_mmap(temp.path()).unwrap();

    let data1 = reader1.map_ptr(0, 1024).unwrap().to_vec();
    let data2 = reader2.map_ptr(0, 1024).unwrap();
    assert_eq!(data1, data2);

    let offset1 = 0;
    let offset2 = size / 2;
    let d1 = reader1.map_ptr(offset1, 1024).unwrap();
    let d2 = reader2.map_ptr(offset2 as u64, 1024).unwrap();
    assert_eq!(d1[0], (offset1 % 256) as u8);
    assert_eq!(d2[0], (offset2 % 256) as u8);
}

#[test]
fn multiple_readers_same_file_buffered() {
    let size = 1024 * 1024;
    let temp = create_test_file(size);

    let mut reader1 = MapFile::open(temp.path()).unwrap();
    let mut reader2 = MapFile::open(temp.path()).unwrap();

    let data1 = reader1.map_ptr(0, 1024).unwrap().to_vec();
    let data2 = reader2.map_ptr(0, 1024).unwrap();
    assert_eq!(data1, data2);

    let d1 = reader1.map_ptr(0, 1024).unwrap().to_vec();
    let d2 = reader2.map_ptr((size / 2) as u64, 1024).unwrap();
    assert_eq!(d1[0], 0);
    assert_eq!(d2[0], ((size / 2) % 256) as u8);
}

#[cfg(unix)]
#[test]
fn mmap_borrowed_slice_lifetime() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    let data1 = map.map_ptr(0, 100).unwrap().to_vec();
    let data2 = map.map_ptr(100, 100).unwrap().to_vec();

    assert_eq!(data1[0], 0);
    assert_eq!(data2[0], 100);
}

#[test]
fn buffered_borrowed_slice_lifetime() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open(temp.path()).unwrap();

    let data1 = map.map_ptr(0, 100).unwrap().to_vec();
    let data2 = map.map_ptr(100, 100).unwrap().to_vec();

    assert_eq!(data1[0], 0);
    assert_eq!(data2[0], 100);
}

#[cfg(unix)]
#[test]
fn mmap_slice_bounds_checking() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    assert!(map.map_ptr(0, 1000).is_ok());
    assert!(map.map_ptr(999, 1).is_ok());

    assert!(map.map_ptr(1000, 1).is_err());
    assert!(map.map_ptr(500, 501).is_err());
    assert!(map.map_ptr(u64::MAX, 1).is_err());
}

#[test]
fn buffered_slice_bounds_checking() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open(temp.path()).unwrap();

    assert!(map.map_ptr(0, 1000).is_ok());
    assert!(map.map_ptr(999, 1).is_ok());

    assert!(map.map_ptr(1000, 1).is_err());
    assert!(map.map_ptr(500, 501).is_err());
}

#[cfg(unix)]
#[test]
fn mmap_no_window_sliding_overhead() {
    let size = 2 * 1024 * 1024;
    let temp = create_test_file(size);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    for _ in 0..10 {
        let _ = map.map_ptr(0, 100).unwrap();
        let _ = map.map_ptr((size - 100) as u64, 100).unwrap();
        let _ = map.map_ptr((size / 2) as u64, 100).unwrap();
    }
}

#[test]
fn buffered_sequential_access_efficiency() {
    let size = MAX_MAP_SIZE * 2;
    let temp = create_test_file(size);
    let mut map = MapFile::open(temp.path()).unwrap();

    for offset in (0..size).step_by(1024) {
        if offset + 100 > size {
            break;
        }
        let data = map.map_ptr(offset as u64, 100).unwrap();
        assert_eq!(data[0], (offset % 256) as u8);
    }
}

#[cfg(unix)]
#[test]
fn adaptive_switches_at_threshold() {
    let below = MMAP_THRESHOLD - 1;
    let at = MMAP_THRESHOLD;
    let above = MMAP_THRESHOLD + 1;

    let temp_below = create_test_file(below as usize);
    let temp_at = create_test_file(at as usize);
    let temp_above = create_test_file(above as usize);

    let map_below = MapFile::open_adaptive(temp_below.path()).unwrap();
    let map_at = MapFile::open_adaptive(temp_at.path()).unwrap();
    let map_above = MapFile::open_adaptive(temp_above.path()).unwrap();

    assert!(map_below.is_buffered());
    assert!(map_at.is_mmap());
    assert!(map_above.is_mmap());
}

#[cfg(unix)]
#[test]
fn sparse_access_pattern_mmap() {
    let size = 4 * 1024 * 1024;
    let temp = create_test_file(size);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    let offsets = [
        0,
        64 * 1024,
        512 * 1024,
        1024 * 1024,
        2 * 1024 * 1024,
        3 * 1024 * 1024,
        size - 1024,
    ];

    for &offset in &offsets {
        let data = map.map_ptr(offset as u64, 512).unwrap();
        assert_eq!(data[0], (offset % 256) as u8);
    }
}

#[test]
fn reverse_sequential_access_buffered() {
    let size = MAX_MAP_SIZE * 2;
    let temp = create_test_file(size);
    let mut map = MapFile::open(temp.path()).unwrap();

    let step = 4096;
    for offset in (step..size).step_by(step).rev() {
        if offset < 100 {
            break;
        }
        let data = map.map_ptr((offset - 100) as u64, 100).unwrap();
        assert_eq!(data[0], ((offset - 100) % 256) as u8);
    }
}

#[cfg(unix)]
#[test]
fn zigzag_access_pattern() {
    let size = 2 * 1024 * 1024;
    let temp = create_test_file(size);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    for i in 1..10 {
        let near_offset = 0;
        let far_offset = (i * 200 * 1024).min(size - 100);

        let data1 = map.map_ptr(near_offset, 100).unwrap().to_vec();
        let data2 = map.map_ptr(far_offset as u64, 100).unwrap();

        assert_eq!(data1[0], 0);
        assert_eq!(data2[0], (far_offset % 256) as u8);
    }
}

#[cfg(unix)]
#[test]
fn large_file_exact_page_boundaries() {
    let size = 4 * 1024 * 1024;
    let temp = create_test_file(size);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    let page_size = 4096;
    for page in 0..(size / page_size) {
        let offset = page * page_size;
        if offset + 100 > size {
            break;
        }
        let data = map.map_ptr(offset as u64, 100).unwrap();
        assert_eq!(data[0], (offset % 256) as u8);
    }
}

#[cfg(unix)]
#[test]
fn large_file_unaligned_access_mmap() {
    let size = 2 * 1024 * 1024;
    let temp = create_test_file(size);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    let offsets = [1, 3, 7, 13, 127, 1023, 4095, 8191];
    for &offset in &offsets {
        let data = map.map_ptr(offset, 100).unwrap();
        assert_eq!(data[0], (offset % 256) as u8);
    }
}

#[cfg(unix)]
#[test]
fn large_file_single_byte_reads_across_file() {
    let size = 1024 * 1024;
    let temp = create_test_file(size);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    for offset in (0..size).step_by(16 * 1024) {
        let data = map.map_ptr(offset as u64, 1).unwrap();
        assert_eq!(data[0], (offset % 256) as u8);
    }
}

#[cfg(unix)]
#[test]
fn large_file_maximum_single_read_mmap() {
    let size = 2 * 1024 * 1024;
    let temp = create_test_file(size);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    let data = map.map_ptr(0, size).unwrap();
    assert_eq!(data.len(), size);
    assert_eq!(data[0], 0);
    assert_eq!(data[size - 1], ((size - 1) % 256) as u8);
}

#[cfg(unix)]
#[test]
fn map_file_strategy_type_safety() {
    let temp = create_test_file(1000);

    let _buffered: MapFile<BufferedMap> = MapFile::open(temp.path()).unwrap();
    let _mmap: MapFile<MmapStrategy> = MapFile::open_mmap(temp.path()).unwrap();
    let _adaptive: MapFile<AdaptiveMapStrategy> = MapFile::open_adaptive(temp.path()).unwrap();
}

#[test]
fn custom_strategy_with_map_file() {
    let temp = create_test_file(1000);

    let strategy = BufferedMap::open_with_window(temp.path(), 512).unwrap();
    let mut map = MapFile::with_strategy(strategy);

    assert_eq!(map.window_size(), 512);
    let data = map.map_ptr(0, 100).unwrap();
    assert_eq!(data[0], 0);
}

#[cfg(unix)]
#[test]
fn mmap_after_error_recovery() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open_mmap(temp.path()).unwrap();

    let err = map.map_ptr(2000, 100);
    assert!(err.is_err());

    let data = map.map_ptr(0, 100).unwrap();
    assert_eq!(data[0], 0);
}

#[test]
fn buffered_after_error_recovery() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open(temp.path()).unwrap();

    let err = map.map_ptr(2000, 100);
    assert!(err.is_err());

    let data = map.map_ptr(0, 100).unwrap();
    assert_eq!(data[0], 0);
}

#[cfg(unix)]
#[test]
fn adaptive_after_error_recovery() {
    let temp = create_test_file(1000);
    let mut map = MapFile::open_adaptive(temp.path()).unwrap();

    let err = map.map_ptr(2000, 100);
    assert!(err.is_err());

    let data = map.map_ptr(0, 100).unwrap();
    assert_eq!(data[0], 0);
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_small_file_uses_buffered() {
    let small_size = 512 * 1024;
    let temp = create_test_file(small_size);
    let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

    assert!(strategy.is_buffered());
    assert!(!strategy.is_mmap());
    assert_eq!(strategy.file_size(), small_size as u64);
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_large_file_uses_mmap() {
    let large_size = 2 * 1024 * 1024;
    let temp = create_test_file(large_size);
    let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

    assert!(strategy.is_mmap());
    assert!(!strategy.is_buffered());
    assert_eq!(strategy.file_size(), large_size as u64);
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_boundary_below_threshold() {
    let size = (MMAP_THRESHOLD - 1) as usize;
    let temp = create_test_file(size);
    let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

    assert!(
        strategy.is_buffered(),
        "File at {size} bytes (1 below threshold) should use buffered"
    );
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_boundary_exactly_at_threshold() {
    let size = MMAP_THRESHOLD as usize;
    let temp = create_test_file(size);
    let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

    assert!(
        strategy.is_mmap(),
        "File at {size} bytes (exactly at threshold) should use mmap"
    );
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_boundary_above_threshold() {
    let size = (MMAP_THRESHOLD + 1) as usize;
    let temp = create_test_file(size);
    let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

    assert!(
        strategy.is_mmap(),
        "File at {size} bytes (1 above threshold) should use mmap"
    );
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_empty_file_uses_buffered() {
    let temp = create_test_file(0);
    let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

    assert!(strategy.is_buffered());
    assert_eq!(strategy.file_size(), 0);
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_tiny_file_uses_buffered() {
    let temp = create_test_file(1);
    let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

    assert!(strategy.is_buffered());
    assert_eq!(strategy.file_size(), 1);
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_custom_threshold_zero() {
    let temp = create_test_file(100);
    let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), 0).unwrap();

    assert!(strategy.is_mmap());
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_custom_threshold_max() {
    let temp = create_test_file(1000);
    let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), u64::MAX).unwrap();

    assert!(strategy.is_buffered());
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_window_size_differs() {
    let small_temp = create_test_file(1000);
    let large_temp = create_test_file((MMAP_THRESHOLD + 1024) as usize);

    let small_strategy = AdaptiveMapStrategy::open(small_temp.path()).unwrap();
    let large_strategy = AdaptiveMapStrategy::open(large_temp.path()).unwrap();

    assert_eq!(small_strategy.window_size(), MAX_MAP_SIZE);

    assert_eq!(
        large_strategy.window_size(),
        (MMAP_THRESHOLD + 1024) as usize
    );
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_data_consistency() {
    let size = 10000;
    let temp = create_test_file(size);

    let mut buffered = AdaptiveMapStrategy::open_with_threshold(temp.path(), u64::MAX).unwrap();
    let mut mmap = AdaptiveMapStrategy::open_with_threshold(temp.path(), 0).unwrap();

    assert!(buffered.is_buffered());
    assert!(mmap.is_mmap());

    for offset in (0..size - 100).step_by(500) {
        let buf_data = buffered.map_ptr(offset as u64, 100).unwrap().to_vec();
        let mmap_data = mmap.map_ptr(offset as u64, 100).unwrap();

        assert_eq!(
            buf_data, mmap_data,
            "Data mismatch at offset {offset} between buffered and mmap strategies"
        );
    }
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_map_file_convenience_methods() {
    let small_temp = create_test_file(1000);
    let large_temp = create_test_file((MMAP_THRESHOLD + 1024) as usize);

    let small_map = MapFile::open_adaptive(small_temp.path()).unwrap();
    let large_map = MapFile::open_adaptive(large_temp.path()).unwrap();

    assert!(small_map.is_buffered());
    assert!(!small_map.is_mmap());

    assert!(large_map.is_mmap());
    assert!(!large_map.is_buffered());
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_map_file_with_custom_threshold() {
    let temp = create_test_file(500);

    let map_low = MapFile::open_adaptive_with_threshold(temp.path(), 100).unwrap();
    assert!(map_low.is_mmap());

    let map_high = MapFile::open_adaptive_with_threshold(temp.path(), 1000).unwrap();
    assert!(map_high.is_buffered());
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_multiple_threshold_boundaries() {
    let boundaries = [
        (100, 99, true, false),
        (100, 100, false, true),
        (100, 101, false, true),
        (1, 0, true, false),
        (1, 1, false, true),
    ];

    for (threshold, size, expect_buffered, expect_mmap) in boundaries {
        let temp = create_test_file(size);
        let strategy =
            AdaptiveMapStrategy::open_with_threshold(temp.path(), threshold as u64).unwrap();

        assert_eq!(
            strategy.is_buffered(),
            expect_buffered,
            "threshold={threshold}, size={size}: expected is_buffered={expect_buffered}"
        );
        assert_eq!(
            strategy.is_mmap(),
            expect_mmap,
            "threshold={threshold}, size={size}: expected is_mmap={expect_mmap}"
        );
    }
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_read_after_strategy_check() {
    let temp = create_test_file(1000);

    let mut strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), 500).unwrap();
    assert!(strategy.is_mmap());

    let data = strategy.map_ptr(0, 100).unwrap();
    assert_eq!(data.len(), 100);
    for (i, &byte) in data.iter().enumerate() {
        assert_eq!(byte, i as u8);
    }
}

#[cfg(unix)]
#[test]
fn adaptive_strategy_selection_file_size_preserved() {
    let sizes = [
        0,
        1,
        100,
        1000,
        MMAP_THRESHOLD as usize - 1,
        MMAP_THRESHOLD as usize,
        MMAP_THRESHOLD as usize + 1,
    ];

    for size in sizes {
        let temp = create_test_file(size);
        let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

        assert_eq!(
            strategy.file_size(),
            size as u64,
            "File size mismatch for size={size}"
        );
    }
}

#[cfg(unix)]
#[test]
fn mmap_as_slice_full_file() {
    let size = 1024;
    let temp = create_test_file(size);
    let strategy = MmapStrategy::open(temp.path()).unwrap();

    let slice = strategy.as_slice();
    assert_eq!(slice.len(), size);

    for (i, &byte) in slice.iter().enumerate() {
        assert_eq!(byte, (i % 256) as u8);
    }
}

#[cfg(unix)]
#[test]
fn mmap_as_slice_vs_map_ptr() {
    let size = 1000;
    let temp = create_test_file(size);
    let mut strategy = MmapStrategy::open(temp.path()).unwrap();

    let via_map_ptr = strategy.map_ptr(0, size).unwrap().to_vec();
    let slice = strategy.as_slice();

    assert_eq!(slice, &via_map_ptr[..]);
}

#[cfg(unix)]
#[test]
fn mmap_as_slice_empty_file() {
    let temp = create_test_file(0);
    let strategy = MmapStrategy::open(temp.path()).unwrap();

    let slice = strategy.as_slice();
    assert!(slice.is_empty());
}

/// `open_buffered` forces the `Buffered` variant even for files that the
/// adaptive selector would otherwise place on `mmap` (>= MMAP_THRESHOLD).
/// This is the seam used by `DeltaApplicator` when paired with an io_uring
/// writer (#1906, audit F1).
#[cfg(unix)]
#[test]
fn adaptive_open_buffered_forces_buffered_for_large_file() {
    // Above MMAP_THRESHOLD (1 MiB) - the adaptive default would pick mmap.
    let temp = create_test_file((MMAP_THRESHOLD as usize) + 4096);

    let buffered_only = AdaptiveMapStrategy::open_buffered(temp.path()).unwrap();
    assert!(buffered_only.is_buffered());
    assert!(!buffered_only.is_mmap());

    // Sanity: the default adaptive open _would_ have picked mmap.
    let adaptive_default = AdaptiveMapStrategy::open(temp.path()).unwrap();
    assert!(adaptive_default.is_mmap());
}

/// `MapFile::open_adaptive_buffered` is the wrapper-level entry point that
/// `DeltaApplicator::new` calls when `BasisWriterKind::IoUring` is selected.
#[cfg(unix)]
#[test]
fn map_file_open_adaptive_buffered_is_buffered() {
    let temp = create_test_file((MMAP_THRESHOLD as usize) + 4096);
    let map = MapFile::open_adaptive_buffered(temp.path()).unwrap();
    assert!(map.is_buffered());
    assert!(!map.is_mmap());
}
