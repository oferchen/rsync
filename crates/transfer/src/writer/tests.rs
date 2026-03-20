use std::io::{self, IoSlice, Write};

use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;
use protocol::MessageCode;

use super::counting::CountingWriter;
use super::msg_info::MsgInfoSender;
use super::multiplex::MultiplexWriter;
use super::server::ServerWriter;

#[test]
fn server_writer_new_plain() {
    let buf = Vec::new();
    let writer = ServerWriter::new_plain(buf);
    assert!(matches!(writer, ServerWriter::Plain(_)));
}

#[test]
fn server_writer_activate_multiplex() {
    let buf = Vec::new();
    let writer = ServerWriter::new_plain(buf);
    let result = writer.activate_multiplex();
    assert!(result.is_ok());
    let multiplexed = result.unwrap();
    assert!(matches!(multiplexed, ServerWriter::Multiplex(_)));
}

#[test]
fn server_writer_activate_multiplex_twice_fails() {
    let buf = Vec::new();
    let writer = ServerWriter::new_plain(buf);
    let multiplexed = writer.activate_multiplex().unwrap();
    let result = multiplexed.activate_multiplex();
    assert!(result.is_err());
    match result {
        Err(err) => assert_eq!(err.kind(), io::ErrorKind::AlreadyExists),
        Ok(_) => panic!("expected error"),
    }
}

#[test]
fn server_writer_is_multiplexed() {
    let buf = Vec::new();
    let plain_writer = ServerWriter::new_plain(buf);
    assert!(!plain_writer.is_multiplexed());

    let buf2 = Vec::new();
    let mux_writer = ServerWriter::new_plain(buf2).activate_multiplex().unwrap();
    assert!(mux_writer.is_multiplexed());
}

#[test]
fn server_writer_activate_multiplex_in_place() {
    let buf = Vec::new();
    let mut writer = ServerWriter::new_plain(buf);
    assert!(!writer.is_multiplexed());

    let result = writer.activate_multiplex_in_place();
    assert!(result.is_ok());
    assert!(writer.is_multiplexed());
}

#[test]
fn server_writer_activate_multiplex_in_place_twice_fails() {
    let buf = Vec::new();
    let mut writer = ServerWriter::new_plain(buf);
    writer.activate_multiplex_in_place().unwrap();

    let result = writer.activate_multiplex_in_place();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
}

#[test]
fn server_writer_plain_write() {
    let mut buf = Vec::new();
    {
        let mut writer = ServerWriter::new_plain(&mut buf);
        writer.write_all(b"hello").unwrap();
        writer.flush().unwrap();
    }
    assert_eq!(buf, b"hello");
}

#[test]
fn server_writer_send_message_plain_mode_fails() {
    let mut buf = Vec::new();
    let mut writer = ServerWriter::new_plain(&mut buf);
    let result = writer.send_message(MessageCode::Data, b"test");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn server_writer_write_raw_plain() {
    let mut buf = Vec::new();
    {
        let mut writer = ServerWriter::new_plain(&mut buf);
        writer.write_raw(b"raw data").unwrap();
    }
    assert_eq!(buf, b"raw data");
}

#[test]
fn server_writer_write_raw_multiplexed() {
    let mut buf = Vec::new();
    {
        let mut writer = ServerWriter::new_plain(&mut buf)
            .activate_multiplex()
            .unwrap();
        writer.write_raw(b"raw").unwrap();
    }
    assert_eq!(buf, b"raw");
}

#[test]
fn multiplex_writer_empty_write() {
    let mut buf = Vec::new();
    let mut mux = MultiplexWriter::new(&mut buf);
    let n = mux.write(&[]).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn multiplex_writer_flush_empty_buffer() {
    let mut buf = Vec::new();
    {
        let mut mux = MultiplexWriter::new(&mut buf);
        mux.flush().unwrap();
    }
    assert!(buf.is_empty());
}

#[test]
fn activate_compression_on_plain_mode_fails() {
    use std::num::NonZeroU8;
    let buf = Vec::new();
    let writer = ServerWriter::new_plain(buf);
    let level = CompressionLevel::precise(NonZeroU8::new(6).unwrap());
    let result = writer.activate_compression(CompressionAlgorithm::Zlib, level);
    assert!(result.is_err());
    match result {
        Err(err) => assert_eq!(err.kind(), io::ErrorKind::InvalidInput),
        Ok(_) => panic!("expected error"),
    }
}

#[test]
fn activate_compression_on_multiplex_succeeds() {
    use std::num::NonZeroU8;
    let buf = Vec::new();
    let writer = ServerWriter::new_plain(buf).activate_multiplex().unwrap();
    let level = CompressionLevel::precise(NonZeroU8::new(6).unwrap());
    let result = writer.activate_compression(CompressionAlgorithm::Zlib, level);
    assert!(result.is_ok());
    let compressed = result.unwrap();
    assert!(compressed.is_multiplexed());
}

#[test]
fn activate_compression_twice_fails() {
    use std::num::NonZeroU8;
    let buf = Vec::new();
    let level = CompressionLevel::precise(NonZeroU8::new(6).unwrap());
    let writer = ServerWriter::new_plain(buf)
        .activate_multiplex()
        .unwrap()
        .activate_compression(CompressionAlgorithm::Zlib, level)
        .unwrap();
    let level2 = CompressionLevel::precise(NonZeroU8::new(6).unwrap());
    let result = writer.activate_compression(CompressionAlgorithm::Zlib, level2);
    assert!(result.is_err());
    match result {
        Err(err) => assert_eq!(err.kind(), io::ErrorKind::AlreadyExists),
        Ok(_) => panic!("expected error"),
    }
}

#[test]
fn taken_state_activate_multiplex_returns_error() {
    let writer: ServerWriter<Vec<u8>> = ServerWriter::Taken;
    let result = writer.activate_multiplex();
    match result {
        Err(err) => {
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(err.to_string().contains("Taken"));
        }
        Ok(_) => panic!("expected error"),
    }
}

#[test]
fn taken_state_activate_compression_returns_error() {
    use std::num::NonZeroU8;
    let writer: ServerWriter<Vec<u8>> = ServerWriter::Taken;
    let level = CompressionLevel::precise(NonZeroU8::new(6).unwrap());
    let result = writer.activate_compression(CompressionAlgorithm::Zlib, level);
    match result {
        Err(err) => {
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(err.to_string().contains("Taken"));
        }
        Ok(_) => panic!("expected error"),
    }
}

#[test]
fn taken_state_activate_multiplex_in_place_returns_error() {
    let mut writer: ServerWriter<Vec<u8>> = ServerWriter::Taken;
    let result = writer.activate_multiplex_in_place();
    match result {
        Err(err) => {
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(err.to_string().contains("Taken"));
        }
        Ok(_) => panic!("expected error"),
    }
}

#[test]
fn taken_state_send_message_returns_error() {
    let mut writer: ServerWriter<Vec<u8>> = ServerWriter::Taken;
    let result = writer.send_message(MessageCode::Data, b"test");
    match result {
        Err(err) => {
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(err.to_string().contains("Taken"));
        }
        Ok(_) => panic!("expected error"),
    }
}

#[test]
fn taken_state_write_raw_returns_error() {
    let mut writer: ServerWriter<Vec<u8>> = ServerWriter::Taken;
    let result = writer.write_raw(b"test");
    match result {
        Err(err) => {
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(err.to_string().contains("Taken"));
        }
        Ok(_) => panic!("expected error"),
    }
}

#[test]
fn taken_state_write_returns_error() {
    let mut writer: ServerWriter<Vec<u8>> = ServerWriter::Taken;
    let result = writer.write(b"test");
    match result {
        Err(err) => {
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(err.to_string().contains("Taken"));
        }
        Ok(_) => panic!("expected error"),
    }
}

#[test]
fn taken_state_flush_returns_error() {
    let mut writer: ServerWriter<Vec<u8>> = ServerWriter::Taken;
    let result = writer.flush();
    match result {
        Err(err) => {
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(err.to_string().contains("Taken"));
        }
        Ok(_) => panic!("expected error"),
    }
}

#[test]
fn counting_writer_tracks_bytes() {
    let mut buf = Vec::new();
    let mut writer = CountingWriter::new(&mut buf);
    assert_eq!(writer.bytes_written(), 0);

    writer.write_all(b"hello").unwrap();
    assert_eq!(writer.bytes_written(), 5);

    writer.write_all(b" world").unwrap();
    assert_eq!(writer.bytes_written(), 11);
}

#[test]
fn counting_writer_into_inner() {
    let buf: Vec<u8> = Vec::new();
    let writer = CountingWriter::new(buf);
    let inner = writer.into_inner();
    assert!(inner.is_empty());
}

#[test]
fn counting_writer_flush() {
    let mut buf = Vec::new();
    let mut writer = CountingWriter::new(&mut buf);
    writer.write_all(b"test").unwrap();
    writer.flush().unwrap();
    assert_eq!(buf, b"test");
}

#[test]
fn counting_writer_partial_write() {
    let mut buf = [0u8; 3];
    let mut cursor = std::io::Cursor::new(&mut buf[..]);
    let mut writer = CountingWriter::new(&mut cursor);

    let n = writer.write(b"ab").unwrap();
    assert_eq!(n, 2);
    assert_eq!(writer.bytes_written(), 2);
}

#[test]
fn multiplex_writer_write_vectored_empty() {
    let mut buf = Vec::new();
    let mut mux = MultiplexWriter::new(&mut buf);
    let bufs: [IoSlice<'_>; 0] = [];
    let n = mux.write_vectored(&bufs).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn multiplex_writer_write_vectored_single_slice() {
    let mut buf = Vec::new();
    {
        let mut mux = MultiplexWriter::new(&mut buf);
        let data = b"hello";
        let bufs = [IoSlice::new(data)];
        let n = mux.write_vectored(&bufs).unwrap();
        assert_eq!(n, 5);
        mux.flush().unwrap();
    }
    assert_eq!(buf.len(), 4 + 5);
    assert_eq!(&buf[4..], b"hello");
}

#[test]
fn multiplex_writer_write_vectored_multiple_slices() {
    let mut buf = Vec::new();
    {
        let mut mux = MultiplexWriter::new(&mut buf);
        let data1 = b"hello";
        let data2 = b" ";
        let data3 = b"world";
        let bufs = [
            IoSlice::new(data1),
            IoSlice::new(data2),
            IoSlice::new(data3),
        ];
        let n = mux.write_vectored(&bufs).unwrap();
        assert_eq!(n, 11);
        mux.flush().unwrap();
    }
    assert_eq!(buf.len(), 4 + 11);
    assert_eq!(&buf[4..], b"hello world");
}

#[test]
fn multiplex_writer_write_vectored_batches_small_writes() {
    let mut buf = Vec::new();
    {
        let mut mux = MultiplexWriter::new(&mut buf);
        for _ in 0..3 {
            let data = b"x";
            let bufs = [IoSlice::new(data)];
            let _ = mux.write_vectored(&bufs).unwrap();
        }
        mux.flush().unwrap();
    }
    assert_eq!(buf.len(), 4 + 3);
    assert_eq!(&buf[4..], b"xxx");
}

#[test]
fn server_writer_write_vectored_plain() {
    let mut buf = Vec::new();
    {
        let mut writer = ServerWriter::new_plain(&mut buf);
        let data1 = b"hello";
        let data2 = b" world";
        let bufs = [IoSlice::new(data1), IoSlice::new(data2)];
        let n = writer.write_vectored(&bufs).unwrap();
        assert_eq!(n, 11);
    }
    assert_eq!(buf, b"hello world");
}

#[test]
fn server_writer_write_vectored_multiplex() {
    let mut buf = Vec::new();
    {
        let mut writer = ServerWriter::new_plain(&mut buf)
            .activate_multiplex()
            .unwrap();
        let data1 = b"hello";
        let data2 = b" world";
        let bufs = [IoSlice::new(data1), IoSlice::new(data2)];
        let n = writer.write_vectored(&bufs).unwrap();
        assert_eq!(n, 11);
        writer.flush().unwrap();
    }
    assert_eq!(buf.len(), 4 + 11);
    assert_eq!(&buf[4..], b"hello world");
}

#[test]
fn server_writer_write_vectored_taken_returns_error() {
    let mut writer: ServerWriter<Vec<u8>> = ServerWriter::Taken;
    let data = b"test";
    let bufs = [IoSlice::new(data)];
    let result = writer.write_vectored(&bufs);
    match result {
        Err(err) => {
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(err.to_string().contains("Taken"));
        }
        Ok(_) => panic!("expected error"),
    }
}

#[test]
fn counting_writer_write_vectored() {
    let mut buf = Vec::new();
    let mut writer = CountingWriter::new(&mut buf);
    let data1 = b"hello";
    let data2 = b" world";
    let bufs = [IoSlice::new(data1), IoSlice::new(data2)];
    let n = writer.write_vectored(&bufs).unwrap();
    assert_eq!(n, 11);
    assert_eq!(writer.bytes_written(), 11);
    assert_eq!(buf, b"hello world");
}

#[test]
fn multiplex_writer_write_vectored_with_empty_slices() {
    let mut buf = Vec::new();
    {
        let mut mux = MultiplexWriter::new(&mut buf);
        let data1 = b"hello";
        let empty: &[u8] = b"";
        let data2 = b"world";
        let bufs = [
            IoSlice::new(data1),
            IoSlice::new(empty),
            IoSlice::new(data2),
        ];
        let n = mux.write_vectored(&bufs).unwrap();
        assert_eq!(n, 10);
        mux.flush().unwrap();
    }
    assert_eq!(buf.len(), 4 + 10);
    assert_eq!(&buf[4..], b"helloworld");
}

#[test]
fn server_writer_send_redo_multiplex() {
    let mut buf = Vec::new();
    {
        let mut writer = ServerWriter::new_plain(&mut buf)
            .activate_multiplex()
            .unwrap();
        writer.send_redo(42).unwrap();
    }
    assert_eq!(buf.len(), 8);
    let payload = &buf[4..8];
    assert_eq!(payload, &42_i32.to_le_bytes());
}

#[test]
fn server_writer_send_redo_plain_mode_fails() {
    let mut buf = Vec::new();
    let mut writer = ServerWriter::new_plain(&mut buf);
    let result = writer.send_redo(42);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn msg_info_sender_plain_mode_noop() {
    let mut buf = Vec::new();
    let mut writer = ServerWriter::new_plain(&mut buf);
    writer.send_msg_info(b"test info").unwrap();
    assert!(buf.is_empty());
}

#[test]
fn msg_info_sender_multiplex_sends_frame() {
    let mut buf = Vec::new();
    let mut writer = ServerWriter::new_plain(&mut buf)
        .activate_multiplex()
        .unwrap();
    writer.send_msg_info(b"test info").unwrap();
    assert!(!buf.is_empty());
    assert_eq!(buf.len(), 4 + 9);
}

#[test]
fn counting_writer_delegates_msg_info() {
    let mut buf = Vec::new();
    let mut server = ServerWriter::new_plain(&mut buf)
        .activate_multiplex()
        .unwrap();
    let mut counting = CountingWriter::new(&mut server);
    counting.send_msg_info(b"hello").unwrap();
    assert!(!buf.is_empty());
}

#[test]
fn mut_ref_delegates_msg_info() {
    let mut buf = Vec::new();
    let mut server = ServerWriter::new_plain(&mut buf)
        .activate_multiplex()
        .unwrap();
    let writer: &mut ServerWriter<&mut Vec<u8>> = &mut server;
    writer.send_msg_info(b"ref test").unwrap();
    assert!(!buf.is_empty());
}
