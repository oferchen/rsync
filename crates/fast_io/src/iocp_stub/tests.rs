//! Stub-platform tests covering the public surface of [`super`].

use super::*;
use crate::IocpPolicy;
use crate::traits::{FileReader, FileReaderFactory, FileWriter, FileWriterFactory};
use std::io::Write;
use tempfile::{NamedTempFile, tempdir};

#[test]
fn iocp_unavailable_on_stub_platform() {
    assert!(!is_iocp_available());
}

#[test]
fn skip_event_unavailable_on_stub_platform() {
    assert!(!skip_event_optimization_available());
}

#[test]
fn availability_reason_mentions_platform() {
    let reason = iocp_availability_reason();
    assert!(reason.contains("not Windows"));
}

#[test]
fn config_default_values() {
    let config = IocpConfig::default();
    assert!(
        (MIN_CONCURRENT_OPS..=MAX_CONCURRENT_OPS).contains(&config.concurrent_ops),
        "default concurrent_ops {} must sit inside [{}, {}]",
        config.concurrent_ops,
        MIN_CONCURRENT_OPS,
        MAX_CONCURRENT_OPS,
    );
    assert_eq!(config.concurrent_ops, default_concurrent_ops());
    assert_eq!(config.buffer_size, 64 * 1024);
}

#[test]
fn concurrent_ops_for_cpus_matches_windows_formula() {
    assert_eq!(concurrent_ops_for_cpus(8), 32);
    assert_eq!(concurrent_ops_for_cpus(1), MIN_CONCURRENT_OPS);
    assert_eq!(concurrent_ops_for_cpus(16), MAX_CONCURRENT_OPS);
    assert_eq!(concurrent_ops_for_cpus(u32::MAX), MAX_CONCURRENT_OPS);
}

#[test]
fn policy_disabled_writer_uses_std() {
    let mut tmp = NamedTempFile::new().unwrap();
    tmp.write_all(b"").unwrap();
    let file = tmp.reopen().unwrap();

    let writer = writer_from_file(file, 8192, IocpPolicy::Disabled).unwrap();
    assert!(matches!(writer, IocpOrStdWriter::Std(_)));
}

#[test]
fn policy_disabled_reader_uses_std() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("disabled_reader.txt");
    std::fs::write(&path, b"hello").unwrap();

    let reader = reader_from_path(&path, IocpPolicy::Disabled).unwrap();
    assert!(matches!(reader, IocpOrStdReader::Std(_)));
}

#[test]
fn policy_auto_falls_back_to_std_writer() {
    let mut tmp = NamedTempFile::new().unwrap();
    tmp.write_all(b"").unwrap();
    let file = tmp.reopen().unwrap();

    let writer = writer_from_file(file, 8192, IocpPolicy::Auto).unwrap();
    assert!(matches!(writer, IocpOrStdWriter::Std(_)));
}

#[test]
fn policy_auto_falls_back_to_std_reader() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("auto_reader.txt");
    std::fs::write(&path, b"world").unwrap();

    let reader = reader_from_path(&path, IocpPolicy::Auto).unwrap();
    assert!(matches!(reader, IocpOrStdReader::Std(_)));
}

#[test]
fn policy_enabled_writer_returns_error() {
    let tmp = NamedTempFile::new().unwrap();
    let file = tmp.reopen().unwrap();

    let result = writer_from_file(file, 8192, IocpPolicy::Enabled);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(err.to_string().contains("IOCP"));
}

#[test]
fn policy_enabled_reader_returns_error() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("enabled_reader.txt");
    std::fs::write(&path, b"data").unwrap();

    let result = reader_from_path(&path, IocpPolicy::Enabled);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(err.to_string().contains("IOCP"));
}

#[test]
fn writer_parity_disabled_vs_auto() {
    let test_data: Vec<u8> = (0..4096).map(|i| ((i * 7 + 13) % 256) as u8).collect();

    let dir = tempdir().unwrap();
    let path_disabled = dir.path().join("parity_disabled.bin");
    {
        let file = std::fs::File::create(&path_disabled).unwrap();
        let mut writer = writer_from_file(file, 8192, IocpPolicy::Disabled).unwrap();
        writer.write_all(&test_data).unwrap();
        writer.flush().unwrap();
    }

    let path_auto = dir.path().join("parity_auto.bin");
    {
        let file = std::fs::File::create(&path_auto).unwrap();
        let mut writer = writer_from_file(file, 8192, IocpPolicy::Auto).unwrap();
        writer.write_all(&test_data).unwrap();
        writer.flush().unwrap();
    }

    let content_disabled = std::fs::read(&path_disabled).unwrap();
    let content_auto = std::fs::read(&path_auto).unwrap();
    assert_eq!(content_disabled, content_auto);
    assert_eq!(content_disabled, test_data);
}

#[test]
fn reader_parity_disabled_vs_auto() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("parity_read.bin");
    let test_data: Vec<u8> = (0..8192).map(|i| ((i * 11 + 3) % 256) as u8).collect();
    std::fs::write(&path, &test_data).unwrap();

    let mut reader_disabled = reader_from_path(&path, IocpPolicy::Disabled).unwrap();
    let data_disabled = reader_disabled.read_all().unwrap();

    let mut reader_auto = reader_from_path(&path, IocpPolicy::Auto).unwrap();
    let data_auto = reader_auto.read_all().unwrap();

    assert_eq!(data_disabled, data_auto);
    assert_eq!(data_disabled, test_data);
}

#[test]
fn factory_reader_forced_fallback_produces_std() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("factory_fallback.txt");
    std::fs::write(&path, b"factory test").unwrap();

    let factory = IocpReaderFactory::default().force_fallback(true);
    assert!(!factory.will_use_iocp());
    let reader = factory.open(&path).unwrap();
    assert!(matches!(reader, IocpOrStdReader::Std(_)));
}

#[test]
fn factory_writer_forced_fallback_produces_std() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("factory_fallback_write.txt");

    let factory = IocpWriterFactory::default().force_fallback(true);
    assert!(!factory.will_use_iocp());
    let writer = factory.create(&path).unwrap();
    assert!(matches!(writer, IocpOrStdWriter::Std(_)));
}

#[test]
fn write_then_read_roundtrip_via_policy() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("roundtrip.bin");
    let test_data: Vec<u8> = (0..65536).map(|i| ((i * 17 + 5) % 256) as u8).collect();

    {
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = writer_from_file(file, 16384, IocpPolicy::Disabled).unwrap();
        writer.write_all(&test_data).unwrap();
        writer.flush().unwrap();
    }

    let mut reader = reader_from_path(&path, IocpPolicy::Disabled).unwrap();
    let read_back = reader.read_all().unwrap();
    assert_eq!(read_back, test_data);
}

#[test]
fn empty_file_roundtrip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("empty.bin");

    {
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = writer_from_file(file, 8192, IocpPolicy::Disabled).unwrap();
        writer.flush().unwrap();
        assert_eq!(writer.bytes_written(), 0);
    }

    let mut reader = reader_from_path(&path, IocpPolicy::Disabled).unwrap();
    assert_eq!(reader.size(), 0);
    let data = reader.read_all().unwrap();
    assert!(data.is_empty());
}

#[test]
fn policy_default_is_auto() {
    assert_eq!(IocpPolicy::default(), IocpPolicy::Auto);
}

#[test]
fn disk_batch_try_new_returns_none() {
    let config = IocpConfig::default();
    assert!(IocpDiskBatch::try_new(&config).is_none());
}

#[test]
fn disk_batch_new_returns_unsupported() {
    let config = IocpConfig::default();
    let result = IocpDiskBatch::new(&config);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
}

#[test]
fn pump_construction_unsupported_on_stub_platform() {
    let result = CompletionPump::new();
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
}

#[test]
fn pump_with_config_unsupported_on_stub_platform() {
    let result = CompletionPump::with_config(IocpPumpConfig::default());
    assert!(result.is_err());
}

#[test]
fn oneshot_handler_returns_no_op_handler() {
    let (handler, rx) = oneshot_handler();
    // The handler is callable; it just discards the result on this
    // platform because no real pump can fire it.
    handler(Ok(0));
    assert!(rx.try_recv().is_err());
}
