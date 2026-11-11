//! The `MSG_ERROR_EXIT` exchange that ends a failed transfer.
//!
//! A side that hits a fatal error sends its code and then reads the peer until
//! the peer is gone (cleanup.c:242-253, io.c:1066-1085 noop_io_until_death()).
//! A side that receives the code answers it and exits through the same path
//! (io.c:1892-1930). Both ends therefore close with nothing unread, so neither
//! sees a reset.
//!
//! Upstream splits the receiving side into a generator and a receiver after
//! do_recv() forks (main.c:1077). The receiver reads the socket, but its
//! `send_msg()` goes to the generator over a pipe, and the generator repeats a
//! code on the socket only from its own read_a_msg(). oc runs one process, so
//! the answer is chosen from where upstream's socket reader would stand.

use std::io::{self, Read, Write};

use protocol::MessageCode;

use crate::reader::ServerReader;
use crate::writer::ServerWriter;

/// The upstream process that reads the socket when a `MSG_ERROR_EXIT` arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitReader {
    /// The sender (`am_sender`).
    Sender,
    /// The receiving side before do_recv() forks: neither `am_generator` nor
    /// `am_receiver` is set yet.
    UnforkedReceiver,
    /// The receiver child after the fork (`am_receiver`).
    ForkedReceiver,
}

impl ExitReader {
    /// The name upstream's who_am_i() gives this process (rsync.c:987-995).
    pub(crate) const fn who(self) -> &'static str {
        match self {
            Self::Sender => "sender",
            Self::UnforkedReceiver => "Receiver",
            Self::ForkedReceiver => "receiver",
        }
    }
}

/// The code _exit_cleanup() runs with for a failure of this side's own.
///
/// A read that outlived a socket's receive timeout is `--timeout` expiring,
/// RERR_TIMEOUT, however the socket reported it (upstream: io.c:259-266
/// check_timeout()).
pub(crate) fn local_exit_code(error: &io::Error) -> i32 {
    match error.kind() {
        io::ErrorKind::WouldBlock => RERR_TIMEOUT,
        _ => crate::error::rerr_for_io_error(error),
    }
}

/// upstream: errcode.h `RERR_TIMEOUT`.
const RERR_TIMEOUT: i32 = 30;

/// How the wait for the peer's exit failed, in upstream's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitWaitFailure {
    /// io.c:918-925 `rsyserr(FERROR_SOCKET, errno, "read error")`: the text
    /// after the `RSYNC_NAME ": "` prefix log.c:483-500 puts in front of it.
    ReadError(String),
    /// io.c:261-262 `[who] io timeout after N seconds -- exiting`, unprefixed.
    Timeout(String),
}

impl ExitWaitFailure {
    /// The line as a daemon logs it, with the `rsync: ` prefix a read error
    /// carries (log.c:483-500).
    #[must_use]
    pub fn log_line(&self) -> String {
        match self {
            Self::ReadError(text) => format!("rsync: {text}"),
            Self::Timeout(text) => text.clone(),
        }
    }
}

/// A client transfer's error carried together with the failed wait that
/// followed it, so the caller prints the wait's line after the `rsync error:`
/// trailer the error produces, as _exit_cleanup() does (cleanup.c:221-253:
/// log_exit() runs before noop_io_until_death()).
#[derive(Debug)]
struct AfterExitWait {
    error: io::Error,
    failure: ExitWaitFailure,
}

impl std::fmt::Display for AfterExitWait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for AfterExitWait {}

/// Attaches the failed wait to the error that ended the transfer, keeping the
/// error's kind.
pub(crate) fn after_exit_wait(error: io::Error, failure: ExitWaitFailure) -> io::Error {
    io::Error::new(error.kind(), AfterExitWait { error, failure })
}

/// Separates a client transfer's error from the failed wait for the peer's
/// exit that followed it, if one did.
#[must_use]
pub fn split_exit_wait(error: io::Error) -> (io::Error, Option<ExitWaitFailure>) {
    if !error
        .get_ref()
        .is_some_and(|inner| inner.is::<AfterExitWait>())
    {
        return (error, None);
    }
    let kind = error.kind();
    match error
        .into_inner()
        .map(|inner| inner.downcast::<AfterExitWait>())
    {
        Some(Ok(wait)) => (wait.error, Some(wait.failure)),
        Some(Err(inner)) => (io::Error::new(kind, inner), None),
        None => (io::Error::from(kind), None),
    }
}

/// The `MSG_ERROR_EXIT` frame a side writes to the socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Echo {
    /// The zero-length frame (`send_msg(MSG_ERROR_EXIT, "", 0, 0)`).
    Empty,
    /// The 4-byte frame carrying a code (`send_msg_int(MSG_ERROR_EXIT, val)`).
    Code(i32),
}

/// What reaches the socket when `reader` receives a `MSG_ERROR_EXIT` carrying
/// `received` (`None` for the zero-length frame), at `protocol`.
///
/// upstream: io.c:1904-1926 read_a_msg(). A zero-length frame is answered only
/// by a process that is neither sender nor generator; after the fork that is
/// the receiver child, whose answer goes to the generator, which does not
/// repeat a zero-length frame. A code is answered at protocol 31 and up: a
/// generator or receiver sends the code back (the forked receiver through the
/// generator), anyone else the zero-length frame.
pub(crate) const fn echo_for(
    reader: ExitReader,
    received: Option<i32>,
    protocol: u8,
) -> Option<Echo> {
    match (received, reader) {
        (None, ExitReader::UnforkedReceiver) => Some(Echo::Empty),
        (None, _) => None,
        (Some(_), _) if protocol < 31 => None,
        (Some(code), ExitReader::ForkedReceiver) => Some(Echo::Code(code)),
        (Some(_), _) => Some(Echo::Empty),
    }
}

/// Whether a side exiting with `code` waits for the peer to go before it
/// closes the connection.
///
/// upstream: cleanup.c:242-253 - a nonzero code that is not a transport failure
/// (`RERR_SOCKETIO`, `RERR_STREAMIO`, `RERR_SIGNAL1`, `RERR_TIMEOUT`) calls
/// noop_io_until_death() when `protocol_version >= 31 || am_receiver`; that
/// returns at once unless the connection is a daemon socket
/// (io.c:1073-1075, `msgs2stderr` is clear only there, clientserver.c:1178).
pub(crate) const fn waits_for_peer(
    code: i32,
    protocol: u8,
    reader: ExitReader,
    over_daemon_socket: bool,
) -> bool {
    const RERR_SOCKETIO: i32 = 10;
    const RERR_STREAMIO: i32 = 12;
    const RERR_SIGNAL1: i32 = 19;
    const RERR_TIMEOUT: i32 = 30;
    over_daemon_socket
        && code != 0
        && !matches!(
            code,
            RERR_SOCKETIO | RERR_STREAMIO | RERR_SIGNAL1 | RERR_TIMEOUT
        )
        && (protocol >= 31 || matches!(reader, ExitReader::ForkedReceiver))
}

/// Writes `echo` and flushes it.
///
/// upstream: io.c:1910-1911 sends the zero-length frame with
/// `io_flush(FULL_FLUSH)`; the code is flushed by _exit_cleanup()'s
/// `io_flush(MSG_FLUSH)` (cleanup.c:251-252).
pub(crate) fn send_echo<W: Write>(writer: &mut ServerWriter<W>, echo: Echo) -> io::Result<()> {
    match echo {
        Echo::Empty => writer.send_message(MessageCode::ErrorExit, &[])?,
        Echo::Code(code) => writer.send_error_exit(code)?,
    }
    writer.flush_all_pending()
}

/// Reads the peer until it closes the connection or sends a `MSG_ERROR_EXIT`,
/// answering that frame as `reader_side` would.
///
/// Data is discarded and every other message is handled as the transfer would
/// handle it. The read is bounded by the connection's `--timeout`, or by 60
/// seconds when none is set, through the transport the reader sits on.
///
/// Returns how the wait itself failed, if it did: a read error or the
/// `--timeout` expiry (io.c:261-262, already worded so by the transport).
///
/// upstream: io.c:1066-1085 noop_io_until_death(); io.c:828 - end of input
/// there is a quiet exit (`kluge_around_eof == 2`); a `MSG_ERROR_EXIT` read
/// there re-enters _exit_cleanup(), which leaves without waiting again.
pub(crate) fn drain_until_peer_exits<R: Read, W: Write>(
    reader: &mut ServerReader<R>,
    writer: &mut ServerWriter<W>,
    reader_side: ExitReader,
    protocol: u8,
) -> Option<ExitWaitFailure> {
    let mut sink = [0u8; 1024];
    loop {
        let read = reader.read(&mut sink);
        if let Some(received) = reader.take_error_exit() {
            let received = (received != 0).then_some(received);
            if let Some(echo) = echo_for(reader_side, received, protocol) {
                let _ = send_echo(writer, echo);
            }
            return None;
        }
        match read {
            // The multiplex layer reports a close between frames as a
            // truncated header; both are the peer's exit.
            Ok(0) => return None,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return None,
            // upstream: io.c:915-916 - an interrupted or not-ready read is
            // retried; only the transport's --timeout ends the wait.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                return Some(ExitWaitFailure::Timeout(error.to_string()));
            }
            Err(error) => {
                return Some(ExitWaitFailure::ReadError(format!(
                    "[{}] read error: {}",
                    reader_side.who(),
                    logging::upstream_errno_text(&error)
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rows of io.c:1904-1926 for every socket reader, frame length and
    /// protocol side of the 31 boundary.
    #[test]
    fn echo_follows_read_a_msg() {
        use Echo::{Code, Empty};
        use ExitReader::{ForkedReceiver, Sender, UnforkedReceiver};
        let cases = [
            (UnforkedReceiver, None, 32, Some(Empty)),
            (ForkedReceiver, None, 32, None),
            (Sender, None, 32, None),
            (UnforkedReceiver, None, 30, Some(Empty)),
            (UnforkedReceiver, Some(11), 32, Some(Empty)),
            (ForkedReceiver, Some(11), 32, Some(Code(11))),
            (Sender, Some(11), 32, Some(Empty)),
            (Sender, Some(11), 31, Some(Empty)),
            (Sender, Some(11), 30, None),
            (ForkedReceiver, Some(11), 30, None),
        ];
        for (reader, received, protocol, expected) in cases {
            assert_eq!(
                echo_for(reader, received, protocol),
                expected,
                "{reader:?} receiving {received:?} at protocol {protocol}"
            );
        }
    }

    /// WHY: a socket receive timeout surfacing at the exit is `--timeout`
    /// expiring; graded as anything else it would hold the side open waiting
    /// on a peer that has gone silent, which cleanup.c:242 never does for
    /// RERR_TIMEOUT.
    #[test]
    fn a_socket_read_timeout_exits_as_rerr_timeout() {
        assert_eq!(
            local_exit_code(&io::Error::from(io::ErrorKind::WouldBlock)),
            30
        );
        assert_eq!(
            local_exit_code(&io::Error::from(io::ErrorKind::TimedOut)),
            30
        );
        assert_eq!(
            local_exit_code(&io::Error::from(io::ErrorKind::BrokenPipe)),
            10
        );
    }

    /// cleanup.c:242-253 with io.c:1073-1075: only a daemon socket waits, only
    /// for a nonzero code that is not a transport failure, and below protocol
    /// 31 only the forked receiver.
    #[test]
    fn waiting_follows_exit_cleanup() {
        use ExitReader::{ForkedReceiver, Sender, UnforkedReceiver};
        assert!(waits_for_peer(11, 32, Sender, true));
        assert!(waits_for_peer(11, 32, UnforkedReceiver, true));
        assert!(!waits_for_peer(11, 32, UnforkedReceiver, false));
        assert!(!waits_for_peer(0, 32, UnforkedReceiver, true));
        for transport in [10, 12, 19, 30] {
            assert!(!waits_for_peer(transport, 32, ForkedReceiver, true));
        }
        assert!(!waits_for_peer(11, 30, UnforkedReceiver, true));
        assert!(waits_for_peer(11, 30, ForkedReceiver, true));
    }

    /// A peer stream of multiplexed frames, ending at `eof`.
    fn peer(frames: &[(MessageCode, &[u8])]) -> ServerReader<io::Cursor<Vec<u8>>> {
        let mut wire = Vec::new();
        for (code, payload) in frames {
            protocol::send_msg(&mut wire, *code, payload).unwrap();
        }
        ServerReader::new_plain(io::Cursor::new(wire))
            .activate_multiplex()
            .unwrap()
    }

    /// The frames written to `out`, decoded.
    fn frames(out: &[u8]) -> Vec<(MessageCode, Vec<u8>)> {
        let mut cursor = io::Cursor::new(out);
        let mut decoded = Vec::new();
        while (cursor.position() as usize) < out.len() {
            let mut payload = Vec::new();
            let code = protocol::recv_msg_into(&mut cursor, &mut payload).unwrap();
            decoded.push((code, payload));
        }
        decoded
    }

    fn drained(
        frames_in: &[(MessageCode, &[u8])],
        side: ExitReader,
        protocol: u8,
    ) -> Vec<(MessageCode, Vec<u8>)> {
        let mut reader = peer(frames_in);
        let mut out = Vec::new();
        {
            let mut writer = ServerWriter::new_plain(&mut out)
                .activate_multiplex()
                .unwrap();
            assert_eq!(
                drain_until_peer_exits(&mut reader, &mut writer, side, protocol),
                None
            );
            writer.flush().unwrap();
        }
        frames(&out)
    }

    /// WHY: a side waiting for its peer to go must answer the peer's
    /// zero-length frame before it closes, as 3.5.1's pre-fork receiver does;
    /// the 3.5.1 client otherwise reads a reset and prints `read error`.
    #[test]
    fn the_unforked_receiver_answers_the_peers_empty_frame_and_stops() {
        let out = drained(
            &[
                (MessageCode::Data, b"late data"),
                (MessageCode::ErrorExit, b""),
                (MessageCode::Data, b"never read"),
            ],
            ExitReader::UnforkedReceiver,
            32,
        );
        assert_eq!(out, vec![(MessageCode::ErrorExit, Vec::new())]);
    }

    /// WHY: a sender and a forked receiver leave the zero-length frame
    /// unanswered on the socket (io.c:1904-1912), so adding an answer would put
    /// a frame on the wire 3.5.1 never sends.
    #[test]
    fn sender_and_forked_receiver_leave_the_empty_frame_unanswered() {
        for side in [ExitReader::Sender, ExitReader::ForkedReceiver] {
            let out = drained(&[(MessageCode::ErrorExit, b"")], side, 32);
            assert!(out.is_empty(), "{side:?} wrote {out:?}");
        }
    }

    /// WHY: a peer's code is answered by the forked receiver with the same
    /// code and by the sender with an empty frame, only from protocol 31.
    #[test]
    fn a_code_is_answered_by_role_from_protocol_31() {
        let code = 11i32.to_le_bytes();
        let frame = [(MessageCode::ErrorExit, &code[..])];
        assert_eq!(
            drained(&frame, ExitReader::ForkedReceiver, 32),
            vec![(MessageCode::ErrorExit, code.to_vec())]
        );
        assert_eq!(
            drained(&frame, ExitReader::Sender, 32),
            vec![(MessageCode::ErrorExit, Vec::new())]
        );
        assert!(drained(&frame, ExitReader::Sender, 30).is_empty());
    }

    /// A peer that resets the connection mid-wait.
    struct Reset;

    impl Read for Reset {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from_raw_os_error(104))
        }
    }

    /// WHY: a reset during the wait is reported in upstream's rsyserr form,
    /// naming the process that waits; that line is what a 3.5.1 client prints
    /// when an oc daemon closed with its answer unread.
    #[test]
    fn a_reset_during_the_wait_is_reported_as_upstreams_read_error() {
        let mut reader = ServerReader::new_plain(Reset).activate_multiplex().unwrap();
        let mut writer = ServerWriter::new_plain(Vec::new())
            .activate_multiplex()
            .unwrap();
        let failure = drain_until_peer_exits(&mut reader, &mut writer, ExitReader::Sender, 32)
            .expect("the reset ends the wait");
        let errno_text = logging::upstream_errno_text(&io::Error::from_raw_os_error(104));
        assert_eq!(
            failure.log_line(),
            format!("rsync: [sender] read error: {errno_text}")
        );
    }

    /// WHY: the failed wait rides out of a client transfer behind the error
    /// that ended it, which must come back unchanged - kind, text and any
    /// typed inner error - for the caller to classify, with the wait's line
    /// beside it to print after the trailer.
    #[test]
    fn a_failed_wait_travels_behind_the_error_and_splits_off_intact() {
        let failure = ExitWaitFailure::ReadError("[Receiver] read error: x (104)".to_owned());
        let wrapped = after_exit_wait(
            io::Error::new(io::ErrorKind::PermissionDenied, "mkdir failed"),
            failure.clone(),
        );
        assert_eq!(wrapped.kind(), io::ErrorKind::PermissionDenied);
        let (error, split) = split_exit_wait(wrapped);
        assert_eq!(split, Some(failure));
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "mkdir failed");

        let plain = io::Error::from_raw_os_error(13);
        let (error, split) = split_exit_wait(plain);
        assert_eq!(split, None);
        assert_eq!(error.raw_os_error(), Some(13));
    }

    /// WHY: the wait ends at the peer's close without writing anything; the
    /// data read on the way is discarded.
    #[test]
    fn the_wait_ends_quietly_at_the_peers_close() {
        let out = drained(
            &[(MessageCode::Data, b"tail of the transfer")],
            ExitReader::UnforkedReceiver,
            32,
        );
        assert!(out.is_empty());
    }
}
