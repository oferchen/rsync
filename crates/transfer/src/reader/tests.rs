use super::*;
use std::io::{Cursor, Read, Write};
use std::sync::{Arc, Mutex};

use compress::algorithm::CompressionAlgorithm;

#[test]
fn server_reader_new_plain() {
    let data = vec![1, 2, 3, 4, 5];
    let reader = ServerReader::new_plain(Cursor::new(data));
    assert!(!reader.is_multiplexed());
}

#[test]
fn server_reader_activate_multiplex() {
    let data = vec![1, 2, 3, 4, 5];
    let reader = ServerReader::new_plain(Cursor::new(data));
    let result = reader.activate_multiplex();
    assert!(result.is_ok());
    let multiplexed = result.unwrap();
    assert!(multiplexed.is_multiplexed());
}

#[test]
fn server_reader_activate_multiplex_twice_fails() {
    let data = vec![1, 2, 3, 4, 5];
    let reader = ServerReader::new_plain(Cursor::new(data));
    let multiplexed = reader.activate_multiplex().unwrap();
    let result = multiplexed.activate_multiplex();
    assert!(result.is_err());
    match result {
        Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists),
        Ok(_) => panic!("expected error"),
    }
}

#[test]
fn server_reader_is_multiplexed_plain() {
    let data = vec![1, 2, 3, 4, 5];
    let reader = ServerReader::new_plain(Cursor::new(data));
    assert!(!reader.is_multiplexed());
}

#[test]
fn server_reader_is_multiplexed_multiplex() {
    let data = vec![1, 2, 3, 4, 5];
    let reader = ServerReader::new_plain(Cursor::new(data))
        .activate_multiplex()
        .unwrap();
    assert!(reader.is_multiplexed());
}

#[test]
fn server_reader_plain_read() {
    let data = vec![1, 2, 3, 4, 5];
    let mut reader = ServerReader::new_plain(Cursor::new(data));
    let mut buf = [0u8; 5];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(buf, [1, 2, 3, 4, 5]);
}

#[test]
fn server_reader_plain_partial_read() {
    let data = vec![1, 2, 3, 4, 5];
    let mut reader = ServerReader::new_plain(Cursor::new(data));
    let mut buf = [0u8; 3];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 3);
    assert_eq!(buf, [1, 2, 3]);
}

#[test]
fn server_reader_plain_empty_read() {
    let data: Vec<u8> = vec![];
    let mut reader = ServerReader::new_plain(Cursor::new(data));
    let mut buf = [0u8; 5];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn server_reader_activate_compression_on_plain_fails() {
    let data = vec![1, 2, 3, 4, 5];
    let reader = ServerReader::new_plain(Cursor::new(data));
    let result = reader.activate_compression(CompressionAlgorithm::Zlib);
    assert!(result.is_err());
    match result {
        Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput),
        Ok(_) => panic!("expected error"),
    }
}

#[test]
fn server_reader_activate_compression_on_multiplex_succeeds() {
    let data = vec![1, 2, 3, 4, 5];
    let reader = ServerReader::new_plain(Cursor::new(data))
        .activate_multiplex()
        .unwrap();
    let result = reader.activate_compression(CompressionAlgorithm::Zlib);
    assert!(result.is_ok());
    let compressed = result.unwrap();
    assert!(compressed.is_multiplexed());
}

#[test]
fn server_reader_activate_compression_twice_fails() {
    let data = vec![1, 2, 3, 4, 5];
    let compressed = ServerReader::new_plain(Cursor::new(data))
        .activate_multiplex()
        .unwrap()
        .activate_compression(CompressionAlgorithm::Zlib)
        .unwrap();
    let result = compressed.activate_compression(CompressionAlgorithm::Zlib);
    assert!(result.is_err());
    match result {
        Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists),
        Ok(_) => panic!("expected error"),
    }
}

#[test]
fn multiplex_reader_new() {
    let data = vec![1, 2, 3, 4, 5];
    let mux = MultiplexReader::new(Cursor::new(data));
    assert!(mux.buffer.is_empty());
    assert_eq!(mux.pos, 0);
}

#[test]
fn multiplex_reader_buffered_read() {
    let data = vec![];
    let mut mux = MultiplexReader::new(Cursor::new(data));

    // Manually populate the buffer as if we had read a message
    mux.buffer = vec![10, 20, 30, 40, 50];
    mux.pos = 0;

    let mut buf = [0u8; 3];
    let n = mux.read(&mut buf).unwrap();
    assert_eq!(n, 3);
    assert_eq!(buf, [10, 20, 30]);
    assert_eq!(mux.pos, 3);
}

#[test]
fn multiplex_reader_buffered_read_complete() {
    let data = vec![];
    let mut mux = MultiplexReader::new(Cursor::new(data));

    mux.buffer = vec![10, 20, 30];
    mux.pos = 0;

    let mut buf = [0u8; 5];
    let n = mux.read(&mut buf).unwrap();
    assert_eq!(n, 3);
    assert_eq!(&buf[..3], &[10, 20, 30]);
    assert!(mux.buffer.is_empty());
    assert_eq!(mux.pos, 0);
}

#[test]
fn multiplex_reader_buffered_partial_read() {
    let data = vec![];
    let mut mux = MultiplexReader::new(Cursor::new(data));

    mux.buffer = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    mux.pos = 2;

    let mut buf = [0u8; 3];
    let n = mux.read(&mut buf).unwrap();
    assert_eq!(n, 3);
    assert_eq!(buf, [3, 4, 5]);
    assert_eq!(mux.pos, 5);
}

#[test]
fn multiplex_reader_accumulates_msg_io_error() {
    // upstream: io.c:1521-1526
    let mut stream = Vec::new();

    let io_err_val: i32 = 1; // IOERR_GENERAL
    protocol::send_msg(
        &mut stream,
        protocol::MessageCode::IoError,
        &io_err_val.to_le_bytes(),
    )
    .unwrap();

    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"hello").unwrap();

    let io_err_val2: i32 = 2; // IOERR_VANISHED
    protocol::send_msg(
        &mut stream,
        protocol::MessageCode::IoError,
        &io_err_val2.to_le_bytes(),
    )
    .unwrap();

    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"world").unwrap();

    let mut mux = MultiplexReader::new(Cursor::new(stream));

    let mut buf = [0u8; 5];
    let n = mux.read(&mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b"hello");

    assert_eq!(mux.io_error, 1);

    let n = mux.read(&mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b"world");

    // Both flags should be OR'd together: 1 | 2 = 3
    assert_eq!(mux.io_error, 3);

    let taken = mux.take_io_error();
    assert_eq!(taken, 3);
    assert_eq!(mux.io_error, 0);
}

#[test]
fn multiplex_reader_io_error_wrong_payload_length_ignored() {
    // upstream: io.c:1522 `if (msg_bytes != 4) goto invalid_msg;`
    let mut stream = Vec::new();

    protocol::send_msg(&mut stream, protocol::MessageCode::IoError, &[1, 0, 0]).unwrap();

    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"ok").unwrap();

    let mut mux = MultiplexReader::new(Cursor::new(stream));
    let mut buf = [0u8; 2];
    let n = mux.read(&mut buf).unwrap();
    assert_eq!(n, 2);
    assert_eq!(&buf, b"ok");

    assert_eq!(mux.io_error, 0);
}

#[test]
fn server_reader_take_io_error_plain_returns_zero() {
    let mut reader = ServerReader::new_plain(Cursor::new(vec![]));
    assert_eq!(reader.take_io_error(), 0);
}

#[test]
fn server_reader_take_io_error_multiplex_accumulates() {
    let mut stream = Vec::new();
    let io_err: i32 = 1; // IOERR_GENERAL
    protocol::send_msg(
        &mut stream,
        protocol::MessageCode::IoError,
        &io_err.to_le_bytes(),
    )
    .unwrap();
    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"data").unwrap();

    let mut reader = ServerReader::new_plain(Cursor::new(stream))
        .activate_multiplex()
        .unwrap();

    let mut buf = [0u8; 4];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 4);
    assert_eq!(&buf, b"data");

    let io_error = reader.take_io_error();
    assert_eq!(io_error, 1);

    assert_eq!(reader.take_io_error(), 0);
}

#[test]
fn msg_io_error_round_trip_through_multiplex_layer() {
    // Verifies the full MSG_IO_ERROR round-trip:
    // 1. Sender writes MSG_IO_ERROR via multiplex writer
    // 2. Receiver reads it via multiplex reader (accumulates flags)
    // 3. Receiver forwards accumulated flags via multiplex writer
    // 4. Generator receives the forwarded MSG_IO_ERROR
    //
    // upstream: io.c:1521-1528
    use crate::io_error_flags;
    use protocol::{MessageCode, MplexWriter};
    use std::io::Write;

    // Step 1: Build a wire stream with two MSG_IO_ERROR messages
    let mut wire = Vec::new();
    {
        let mut writer = MplexWriter::new(&mut wire);

        let flags1 = io_error_flags::IOERR_GENERAL;
        writer
            .write_message(MessageCode::IoError, &flags1.to_le_bytes())
            .unwrap();
        writer.write_all(b"part1").unwrap();
        writer.flush().unwrap();

        let flags2 = io_error_flags::IOERR_VANISHED;
        writer
            .write_message(MessageCode::IoError, &flags2.to_le_bytes())
            .unwrap();
        writer.write_all(b"part2").unwrap();
        writer.flush().unwrap();
    }

    // Step 2: Receiver reads through the stream
    let mut reader = MultiplexReader::new(Cursor::new(wire));
    let mut buf = [0u8; 5];

    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b"part1");

    let first = reader.take_io_error();
    assert_eq!(first, io_error_flags::IOERR_GENERAL);

    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b"part2");

    let second = reader.take_io_error();
    assert_eq!(second, io_error_flags::IOERR_VANISHED);

    let combined = first | second;
    assert_eq!(
        combined,
        io_error_flags::IOERR_GENERAL | io_error_flags::IOERR_VANISHED
    );

    // Step 3: Receiver forwards the accumulated io_error to the generator
    let mut forward_wire = Vec::new();
    {
        let mut fwd_writer = MplexWriter::new(&mut forward_wire);
        fwd_writer
            .write_message(MessageCode::IoError, &combined.to_le_bytes())
            .unwrap();
    }

    // Step 4: Generator receives the forwarded MSG_IO_ERROR
    let mut fwd_cursor = Cursor::new(forward_wire);
    let frame = protocol::recv_msg(&mut fwd_cursor).unwrap();
    assert_eq!(frame.code(), MessageCode::IoError);
    assert_eq!(frame.payload().len(), 4);
    let forwarded_flags = i32::from_le_bytes(frame.payload().try_into().unwrap());
    assert_eq!(
        forwarded_flags,
        io_error_flags::IOERR_GENERAL | io_error_flags::IOERR_VANISHED
    );

    let exit_code = io_error_flags::to_exit_code(forwarded_flags);
    assert_eq!(exit_code, 23); // RERR_PARTIAL
}

#[test]
fn multiplex_reader_accumulates_msg_no_send() {
    // upstream: io.c:1618-1627, sender.c:367-368
    let mut stream = Vec::new();

    let ndx1: i32 = 42;
    protocol::send_msg(
        &mut stream,
        protocol::MessageCode::NoSend,
        &ndx1.to_le_bytes(),
    )
    .unwrap();

    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"hello").unwrap();

    let ndx2: i32 = 99;
    protocol::send_msg(
        &mut stream,
        protocol::MessageCode::NoSend,
        &ndx2.to_le_bytes(),
    )
    .unwrap();

    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"world").unwrap();

    let mut mux = MultiplexReader::new(Cursor::new(stream));

    let mut buf = [0u8; 5];
    let n = mux.read(&mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b"hello");

    assert_eq!(mux.no_send_indices, vec![42]);

    let n = mux.read(&mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b"world");

    assert_eq!(mux.no_send_indices, vec![42, 99]);

    let taken = mux.take_no_send_indices();
    assert_eq!(taken, vec![42, 99]);
    assert!(mux.no_send_indices.is_empty());
}

#[test]
fn multiplex_reader_no_send_wrong_payload_length_ignored() {
    // upstream: io.c:1619 `if (msg_bytes != 4) goto invalid_msg;`
    let mut stream = Vec::new();

    protocol::send_msg(&mut stream, protocol::MessageCode::NoSend, &[1, 0, 0]).unwrap();

    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"ok").unwrap();

    let mut mux = MultiplexReader::new(Cursor::new(stream));
    let mut buf = [0u8; 2];
    let n = mux.read(&mut buf).unwrap();
    assert_eq!(n, 2);
    assert_eq!(&buf, b"ok");

    assert!(mux.no_send_indices.is_empty());
}

#[test]
fn server_reader_take_no_send_indices_plain_returns_empty() {
    let mut reader = ServerReader::new_plain(Cursor::new(vec![]));
    assert!(reader.take_no_send_indices().is_empty());
}

#[test]
fn server_reader_take_no_send_indices_multiplex_accumulates() {
    let mut stream = Vec::new();
    let ndx: i32 = 7;
    protocol::send_msg(
        &mut stream,
        protocol::MessageCode::NoSend,
        &ndx.to_le_bytes(),
    )
    .unwrap();
    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"data").unwrap();

    let mut reader = ServerReader::new_plain(Cursor::new(stream))
        .activate_multiplex()
        .unwrap();

    let mut buf = [0u8; 4];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 4);
    assert_eq!(&buf, b"data");

    let indices = reader.take_no_send_indices();
    assert_eq!(indices, vec![7]);

    assert!(reader.take_no_send_indices().is_empty());
}

#[test]
fn multiplex_reader_accumulates_msg_redo() {
    // upstream: io.c:1514-1519, receiver.c:970-974
    let mut stream = Vec::new();

    let ndx1: i32 = 5;
    protocol::send_msg(
        &mut stream,
        protocol::MessageCode::Redo,
        &ndx1.to_le_bytes(),
    )
    .unwrap();

    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"chunk1").unwrap();

    let ndx2: i32 = 17;
    protocol::send_msg(
        &mut stream,
        protocol::MessageCode::Redo,
        &ndx2.to_le_bytes(),
    )
    .unwrap();

    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"chunk2").unwrap();

    let mut mux = MultiplexReader::new(Cursor::new(stream));

    let mut buf = [0u8; 6];
    let n = mux.read(&mut buf).unwrap();
    assert_eq!(n, 6);
    assert_eq!(&buf, b"chunk1");

    assert_eq!(mux.redo_indices, vec![5]);

    let n = mux.read(&mut buf).unwrap();
    assert_eq!(n, 6);
    assert_eq!(&buf, b"chunk2");

    assert_eq!(mux.redo_indices, vec![5, 17]);

    let taken = mux.take_redo_indices();
    assert_eq!(taken, vec![5, 17]);
    assert!(mux.redo_indices.is_empty());
}

#[test]
fn multiplex_reader_redo_wrong_payload_length_ignored() {
    // upstream: io.c:1516 reads exactly 4 bytes for val
    let mut stream = Vec::new();

    protocol::send_msg(&mut stream, protocol::MessageCode::Redo, &[1, 0, 0]).unwrap();

    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"ok").unwrap();

    let mut mux = MultiplexReader::new(Cursor::new(stream));
    let mut buf = [0u8; 2];
    let n = mux.read(&mut buf).unwrap();
    assert_eq!(n, 2);
    assert_eq!(&buf, b"ok");

    assert!(mux.redo_indices.is_empty());
}

#[test]
fn server_reader_take_redo_indices_plain_returns_empty() {
    let mut reader = ServerReader::new_plain(Cursor::new(vec![]));
    assert!(reader.take_redo_indices().is_empty());
}

#[test]
fn server_reader_take_redo_indices_multiplex_accumulates() {
    let mut stream = Vec::new();
    let ndx: i32 = 13;
    protocol::send_msg(&mut stream, protocol::MessageCode::Redo, &ndx.to_le_bytes()).unwrap();
    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"data").unwrap();

    let mut reader = ServerReader::new_plain(Cursor::new(stream))
        .activate_multiplex()
        .unwrap();

    let mut buf = [0u8; 4];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(n, 4);
    assert_eq!(&buf, b"data");

    let indices = reader.take_redo_indices();
    assert_eq!(indices, vec![13]);

    assert!(reader.take_redo_indices().is_empty());
}

#[test]
fn multiplex_reader_redo_and_no_send_interleaved() {
    let mut stream = Vec::new();

    let redo_ndx: i32 = 3;
    protocol::send_msg(
        &mut stream,
        protocol::MessageCode::Redo,
        &redo_ndx.to_le_bytes(),
    )
    .unwrap();

    let no_send_ndx: i32 = 7;
    protocol::send_msg(
        &mut stream,
        protocol::MessageCode::NoSend,
        &no_send_ndx.to_le_bytes(),
    )
    .unwrap();

    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"x").unwrap();

    let mut mux = MultiplexReader::new(Cursor::new(stream));
    let mut buf = [0u8; 1];
    let n = mux.read(&mut buf).unwrap();
    assert_eq!(n, 1);
    assert_eq!(&buf, b"x");

    assert_eq!(mux.redo_indices, vec![3]);
    assert_eq!(mux.no_send_indices, vec![7]);
}

#[test]
fn multiplex_reader_batch_recorder_captures_demuxed_data() {
    // Verify that the batch recorder captures post-demux MSG_DATA payloads.
    // upstream: io.c:read_buf() tees data to batch_fd after demultiplexing.
    let payload = b"hello batch reader";
    let mut stream = Vec::new();
    protocol::send_msg(&mut stream, protocol::MessageCode::Data, payload).unwrap();

    let mut mux = MultiplexReader::new(Cursor::new(stream));
    let recorder_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    mux.batch_recorder = Some(recorder_buf.clone());

    let mut buf = vec![0u8; 64];
    let n = mux.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], payload);

    let recorded = recorder_buf.lock().unwrap();
    assert_eq!(
        &*recorded, payload,
        "recorder should capture exact demuxed bytes"
    );
}

#[test]
fn multiplex_reader_batch_recorder_skips_control_messages() {
    // Verify that control messages (MSG_IO_ERROR) are NOT recorded -
    // only MSG_DATA payloads go to the batch recorder.
    let mut stream = Vec::new();
    protocol::send_msg(
        &mut stream,
        protocol::MessageCode::IoError,
        &1i32.to_le_bytes(),
    )
    .unwrap();
    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"data").unwrap();

    let mut mux = MultiplexReader::new(Cursor::new(stream));
    let recorder_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    mux.batch_recorder = Some(recorder_buf.clone());

    let mut buf = vec![0u8; 64];
    let n = mux.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"data");

    let recorded = recorder_buf.lock().unwrap();
    assert_eq!(
        &*recorded, b"data",
        "recorder should only contain MSG_DATA payloads"
    );
}

#[test]
fn multiplex_reader_batch_recorder_multiple_reads() {
    // Verify that multiple reads accumulate correctly in the recorder.
    let mut stream = Vec::new();
    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"first").unwrap();
    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"second").unwrap();

    let mut mux = MultiplexReader::new(Cursor::new(stream));
    let recorder_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    mux.batch_recorder = Some(recorder_buf.clone());

    let mut buf = vec![0u8; 64];
    let n1 = mux.read(&mut buf).unwrap();
    let n2 = mux.read(&mut buf).unwrap();

    assert!(n1 > 0);
    assert!(n2 > 0);

    let recorded = recorder_buf.lock().unwrap();
    assert_eq!(&*recorded, b"firstsecond");
}

#[test]
fn server_reader_set_batch_recorder_multiplex() {
    let mut stream = Vec::new();
    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"test").unwrap();

    let reader = ServerReader::new_plain(Cursor::new(stream));
    let mut mux_reader = reader.activate_multiplex().unwrap();

    let recorder: Arc<Mutex<dyn Write + Send>> = Arc::new(Mutex::new(Vec::<u8>::new()));
    mux_reader.set_batch_recorder(recorder);
}

#[test]
fn server_reader_pending_batch_recorder_propagates_on_activate() {
    // Verify that a batch recorder set in Plain mode gets propagated
    // to the MultiplexReader when activate_multiplex() is called.
    let mut stream = Vec::new();
    protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"propagated").unwrap();

    let mut reader = ServerReader::new_plain(Cursor::new(stream));

    let recorder_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder: Arc<Mutex<dyn Write + Send>> = recorder_buf.clone();
    reader.set_batch_recorder(recorder);

    // Activate multiplex - recorder should propagate
    let mut mux_reader = reader.activate_multiplex().unwrap();

    let mut buf = vec![0u8; 64];
    let n = mux_reader.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"propagated");

    let recorded = recorder_buf.lock().unwrap();
    assert_eq!(&*recorded, b"propagated");
}

#[test]
fn batch_recorder_roundtrip_writer_reader_capture_same_data() {
    // End-to-end: write data through MultiplexWriter with recorder,
    // read back through MultiplexReader with recorder, verify both
    // recorders captured identical pre-mux / post-demux data.
    use crate::writer::multiplex::MultiplexWriter;

    let mut wire = Vec::new();

    // Writer side: record pre-mux data
    let write_recorder: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let mut mux_writer = MultiplexWriter::new(&mut wire);
        mux_writer.batch_recorder = Some(write_recorder.clone());
        mux_writer.write_all(b"roundtrip test data").unwrap();
        mux_writer.flush().unwrap();
    }

    // Reader side: record post-demux data
    let read_recorder: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let mut mux_reader = MultiplexReader::new(Cursor::new(wire));
    mux_reader.batch_recorder = Some(read_recorder.clone());

    let mut buf = vec![0u8; 64];
    let n = mux_reader.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"roundtrip test data");

    let write_recorded = write_recorder.lock().unwrap();
    let read_recorded = read_recorder.lock().unwrap();
    assert_eq!(
        &*write_recorded, &*read_recorded,
        "writer and reader recorders should capture identical data"
    );
    assert_eq!(&*write_recorded, b"roundtrip test data");
}

// upstream: io.c:read_buf() tees data to batch_fd BEFORE decompression.
// The batch recorder must stay on MultiplexReader (not CompressedReader)
// so it captures compressed wire bytes. The batch header stores
// do_compression=true so replay decompresses the tokens.

#[test]
fn server_reader_compressed_batch_recorder_captures_compressed_wire_bytes() {
    // When compression is active, the batch recorder on the inner
    // MultiplexReader captures the compressed wire bytes (not decompressed).
    // This matches upstream io.c:read_buf() behavior.
    use crate::compressed_writer::CompressedWriter;
    use crate::writer::multiplex::MultiplexWriter;
    use compress::zlib::CompressionLevel;

    let original = b"server reader compressed batch test data payload";

    // Build a compressed+multiplexed wire stream
    let mut wire = Vec::new();
    {
        let mux = MultiplexWriter::new(&mut wire);
        let mut compressed =
            CompressedWriter::new(mux, CompressionAlgorithm::Zlib, CompressionLevel::Default)
                .unwrap();
        compressed.write_all(original).unwrap();
        compressed.finish().unwrap();
    }

    // Read back through ServerReader with compression and batch recorder
    let reader = ServerReader::new_plain(Cursor::new(wire));
    let mux_reader = reader.activate_multiplex().unwrap();
    let mut compressed_reader = mux_reader
        .activate_compression(CompressionAlgorithm::Zlib)
        .unwrap();

    let recorder_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder: Arc<Mutex<dyn Write + Send>> = recorder_buf.clone();
    compressed_reader.set_batch_recorder(recorder);

    let mut output = vec![0u8; original.len()];
    compressed_reader.read_exact(&mut output).unwrap();
    assert_eq!(&output, original);

    let recorded = recorder_buf.lock().unwrap();
    // The recorded data should be compressed (different from original).
    // It should NOT equal the original uncompressed data.
    assert_ne!(
        &*recorded, original,
        "batch recorder must capture compressed wire bytes, not decompressed data"
    );
    assert!(
        !recorded.is_empty(),
        "batch recorder must capture some data"
    );
}

#[test]
fn server_reader_batch_recorder_stays_on_mux_after_compression_activation() {
    // When a batch recorder is set on MultiplexReader before compression is
    // activated, it stays on MultiplexReader (not moved to CompressedReader).
    // This captures compressed wire bytes matching upstream behavior.
    use crate::compressed_writer::CompressedWriter;
    use crate::writer::multiplex::MultiplexWriter;
    use compress::zlib::CompressionLevel;

    let original = b"recorder stays on mux test data";

    let mut wire = Vec::new();
    {
        let mux = MultiplexWriter::new(&mut wire);
        let mut compressed =
            CompressedWriter::new(mux, CompressionAlgorithm::Zlib, CompressionLevel::Default)
                .unwrap();
        compressed.write_all(original).unwrap();
        compressed.finish().unwrap();
    }

    let reader = ServerReader::new_plain(Cursor::new(wire));
    let mut mux_reader = reader.activate_multiplex().unwrap();

    let recorder_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder: Arc<Mutex<dyn Write + Send>> = recorder_buf.clone();
    mux_reader.set_batch_recorder(recorder);

    // Activate compression - recorder stays on MultiplexReader
    let mut compressed_reader = mux_reader
        .activate_compression(CompressionAlgorithm::Zlib)
        .unwrap();

    let mut output = vec![0u8; original.len()];
    compressed_reader.read_exact(&mut output).unwrap();
    assert_eq!(&output, original);

    let recorded = recorder_buf.lock().unwrap();
    // Recorded data is compressed wire bytes, not equal to original
    assert_ne!(
        &*recorded, original,
        "recorder on mux layer should capture compressed wire bytes"
    );
    assert!(!recorded.is_empty());
}

#[test]
fn batch_recorder_roundtrip_compressed_captures_identical_wire_bytes() {
    // End-to-end: write data through compressed+multiplexed writer with
    // batch recorder on mux layer, read back through compressed+multiplexed
    // reader with batch recorder on mux layer. Both recorders capture
    // identical COMPRESSED wire bytes.
    use crate::compressed_writer::CompressedWriter;
    use crate::writer::multiplex::MultiplexWriter;
    use compress::zlib::CompressionLevel;

    let original = b"compressed roundtrip batch verification payload data";
    let mut wire = Vec::new();

    // Writer side: record post-compression (compressed) data at mux layer
    let write_recorder: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let mut mux = MultiplexWriter::new(&mut wire);
        mux.batch_recorder = Some(write_recorder.clone());
        let mut compressed =
            CompressedWriter::new(mux, CompressionAlgorithm::Zlib, CompressionLevel::Default)
                .unwrap();
        compressed.write_all(original).unwrap();
        compressed.finish().unwrap();
    }

    // Reader side: record pre-decompression (compressed) data at mux layer
    let read_recorder: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let mut mux = MultiplexReader::new(Cursor::new(wire));
        mux.batch_recorder = Some(read_recorder.clone());
        let mut compressed =
            crate::compressed_reader::CompressedReader::new(mux, CompressionAlgorithm::Zlib)
                .unwrap();

        let mut output = vec![0u8; original.len()];
        compressed.read_exact(&mut output).unwrap();
        assert_eq!(&output, original);
    }

    let write_recorded = write_recorder.lock().unwrap();
    let read_recorded = read_recorder.lock().unwrap();
    assert_eq!(
        &*write_recorded, &*read_recorded,
        "writer and reader batch recorders should capture identical compressed wire bytes"
    );
    // Both should contain compressed data, not the original
    assert_ne!(&*write_recorded, original);
    assert!(!write_recorded.is_empty());
}

#[test]
fn server_reader_batch_recorder_stays_on_mux_lz4() {
    // Verify batch recorder stays on MultiplexReader with LZ4 compression.
    #[cfg(feature = "lz4")]
    {
        use crate::compressed_writer::CompressedWriter;
        use crate::writer::multiplex::MultiplexWriter;
        use compress::zlib::CompressionLevel;

        let original = b"lz4 mux recorder test";

        let mut wire = Vec::new();
        {
            let mux = MultiplexWriter::new(&mut wire);
            let mut compressed =
                CompressedWriter::new(mux, CompressionAlgorithm::Lz4, CompressionLevel::Default)
                    .unwrap();
            compressed.write_all(original).unwrap();
            compressed.finish().unwrap();
        }

        let reader = ServerReader::new_plain(Cursor::new(wire));
        let mut mux_reader = reader.activate_multiplex().unwrap();

        let recorder_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder: Arc<Mutex<dyn Write + Send>> = recorder_buf.clone();
        mux_reader.set_batch_recorder(recorder);

        let mut compressed_reader = mux_reader
            .activate_compression(CompressionAlgorithm::Lz4)
            .unwrap();

        let mut output = vec![0u8; original.len()];
        compressed_reader.read_exact(&mut output).unwrap();
        assert_eq!(&output, original);

        let recorded = recorder_buf.lock().unwrap();
        assert!(!recorded.is_empty(), "recorder must capture data");
    }
}

#[test]
fn server_reader_batch_recorder_stays_on_mux_zstd() {
    // Verify batch recorder stays on MultiplexReader with Zstd compression.
    #[cfg(feature = "zstd")]
    {
        use crate::compressed_writer::CompressedWriter;
        use crate::writer::multiplex::MultiplexWriter;
        use compress::zlib::CompressionLevel;

        let original = b"zstd mux recorder test";

        let mut wire = Vec::new();
        {
            let mux = MultiplexWriter::new(&mut wire);
            let mut compressed =
                CompressedWriter::new(mux, CompressionAlgorithm::Zstd, CompressionLevel::Default)
                    .unwrap();
            compressed.write_all(original).unwrap();
            compressed.finish().unwrap();
        }

        let reader = ServerReader::new_plain(Cursor::new(wire));
        let mut mux_reader = reader.activate_multiplex().unwrap();

        let recorder_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder: Arc<Mutex<dyn Write + Send>> = recorder_buf.clone();
        mux_reader.set_batch_recorder(recorder);

        let mut compressed_reader = mux_reader
            .activate_compression(CompressionAlgorithm::Zstd)
            .unwrap();

        let mut output = vec![0u8; original.len()];
        compressed_reader.read_exact(&mut output).unwrap();
        assert_eq!(&output, original);

        let recorded = recorder_buf.lock().unwrap();
        assert!(!recorded.is_empty(), "recorder must capture data");
    }
}
