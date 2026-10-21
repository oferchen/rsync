//! One owner for both directions of a transfer connection.
//!
//! Upstream keeps a single file-scope `iobuf` that holds the input and output
//! buffers, both descriptors and the multiplex state (upstream: io.c:101-110),
//! and every read and every flush goes through `perform_io()` on that one
//! object (io.c:647). Nothing in upstream can write without the reader's state
//! in reach, which is what lets `perform_io()` read peer input while it waits
//! to write and drain the peer's messages when a write fails (io.c:961-967).
//!
//! [`IoBuf`] is that owner. A role receives one `&mut IoBuf` instead of a
//! reader and a writer threaded side by side, so the decisions that need both
//! halves - whether a blocking read must first flush, and what to do with
//! buffered input when output fails - have one place to live.

use std::io::{self, IoSlice, Read, Write};

use crate::reader::{BufferedInputHint, ServerReader};
use crate::writer::ServerWriter;

/// The input and output halves of one connection, owned together.
///
/// `I` is the input stack (a [`ServerReader`] in production); the output stack
/// is always a [`ServerWriter`]. Reads go to the input and writes to the
/// output, through [`Read`] and [`Write`] on the owner itself.
pub struct IoBuf<I, W: Write> {
    input: I,
    output: ServerWriter<W>,
}

impl<I, W: Write> IoBuf<I, W> {
    /// Takes ownership of both halves of a connection.
    pub fn new(input: I, output: ServerWriter<W>) -> Self {
        Self { input, output }
    }

    /// The input stack, for operations only it has (frame-state queries,
    /// peer-message ledgers, mode switches).
    pub(crate) fn input(&mut self) -> &mut I {
        &mut self.input
    }

    /// The output stack, for operations only it has (control messages,
    /// keepalives, compression finalisation).
    pub(crate) fn output(&mut self) -> &mut ServerWriter<W> {
        &mut self.output
    }

    /// Both stacks at once, for an exchange that reads while it may have to
    /// write - a signature read that sends keepalives, or the goodbye
    /// handshake.
    pub(crate) fn halves(&mut self) -> (&mut I, &mut ServerWriter<W>) {
        (&mut self.input, &mut self.output)
    }

    /// Gives up ownership of the output stack, for the error report the
    /// connection sends after its role has ended.
    pub(crate) fn into_output(self) -> ServerWriter<W> {
        self.output
    }
}

impl<I: BufferedInputHint, W: Write> IoBuf<I, W> {
    /// Whether the next read is served from bytes already received, so a
    /// pending flush need not happen first.
    ///
    /// upstream: io.c:661 - `perform_io(needed, PIO_NEED_INPUT)` returns at
    /// once when `iobuf.in.len >= needed`, without touching the output; only a
    /// read that must wait drives the output flush (io.c:742-779).
    pub(crate) fn input_buffered(&self) -> bool {
        self.input.has_buffered_input()
    }

    /// The bytes [`IoBuf::input_buffered`] reports, for a caller that looks
    /// ahead at requests already received without consuming them.
    pub(crate) fn buffered_input(&self) -> &[u8] {
        self.input.buffered_input()
    }
}

impl<R: Read, W: Write> IoBuf<ServerReader<R>, W> {
    /// Starts demultiplexing the input.
    ///
    /// upstream: io.c:2699 io_start_multiplex_in() switches the one `iobuf`.
    pub(crate) fn activate_input_multiplex(&mut self) -> io::Result<()> {
        self.input.activate_multiplex_in_place()
    }
}

impl<I: Read, W: Write> Read for IoBuf<I, W> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.input.read(buf)
    }
}

impl<I, W: Write> Write for IoBuf<I, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.write(buf)
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.output.write_vectored(bufs)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// A reader whose buffered-input answer is fixed by the test.
    struct Hinted {
        bytes: Cursor<Vec<u8>>,
        buffered: bool,
    }

    impl Read for Hinted {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.bytes.read(buf)
        }
    }

    impl BufferedInputHint for Hinted {
        fn has_buffered_input(&self) -> bool {
            self.buffered
        }
    }

    /// WHY: a role that holds one `IoBuf` must see the same bytes it would have
    /// read and written through the two halves; the owner adds no framing.
    #[test]
    fn reads_reach_the_input_and_writes_the_output() {
        let mut io = IoBuf::new(
            Cursor::new(b"peer".to_vec()),
            ServerWriter::new_plain(Vec::new()),
        );
        let mut got = [0u8; 4];
        io.read_exact(&mut got).unwrap();
        io.write_all(b"ours").unwrap();
        io.flush().unwrap();
        assert_eq!(&got, b"peer");
        match io.into_output() {
            ServerWriter::Plain(out) => assert_eq!(out, b"ours"),
            _ => panic!("the output stays in plain mode"),
        }
    }

    /// WHY: the flush-before-read decision is upstream's `iobuf.in.len >=
    /// needed` test; it must report the input's own answer, both ways.
    #[test]
    fn input_buffered_reports_the_input_state() {
        for buffered in [false, true] {
            let io = IoBuf::new(
                Hinted {
                    bytes: Cursor::new(Vec::new()),
                    buffered,
                },
                ServerWriter::new_plain(Vec::new()),
            );
            assert_eq!(io.input_buffered(), buffered);
        }
    }

    /// WHY: the generator switches the input to multiplex mode after the
    /// filter exchange; doing so in place must leave a demultiplexing reader.
    #[test]
    fn activating_input_multiplex_in_place_demultiplexes() {
        let mut wire = Vec::new();
        protocol::send_msg(&mut wire, protocol::MessageCode::Data, b"framed").unwrap();
        let mut io = IoBuf::new(
            ServerReader::new_plain(Cursor::new(wire)),
            ServerWriter::new_plain(Vec::new()),
        );
        io.activate_input_multiplex().unwrap();
        let mut got = [0u8; 6];
        io.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"framed");
        assert!(
            io.activate_input_multiplex().is_err(),
            "a second activation is a mode-sequencing error"
        );
    }
}
