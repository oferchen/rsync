//! Passing a file descriptor plus a small metadata record between two
//! processes over a connected `AF_UNIX` socket pair.
//!
//! The daemon parent forks a helper that owns a listening transport it must
//! not run sessions on itself; the helper hands each accepted connection back
//! to the parent as a descriptor, together with the facts the parent needs to
//! admit it (peer address and the like). This module is the kernel half of
//! that hand-off: one
//! [`FdChannel::send`](crate::fd_pass::FdChannel::send) delivers one record
//! and exactly one descriptor, and one
//! [`FdChannel::recv`](crate::fd_pass::FdChannel::recv) yields them back as
//! an owned pair.
//!
//! The consumer is the daemon's QUIC front process: it accepts QUIC streams
//! and relays each one to the single-threaded daemon parent, which forks the
//! session exactly as it does for TCP.
//! [`RelayRecord`](crate::fd_pass::RelayRecord) is the record that hand-off
//! carries.
//!
//! The channel is record-oriented, so a record can never be split across two
//! receives or merged with the next: `SOCK_SEQPACKET` where the kernel offers
//! it, and `SOCK_DGRAM` on Apple targets, whose `AF_UNIX` family has no
//! sequenced-packet type. On a connected socket pair both types are reliable
//! and ordered.
#![cfg(unix)]

use std::io::{self, IoSlice, IoSliceMut};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

use nix::cmsg_space;
use nix::sys::socket::{
    AddressFamily, ControlMessage, ControlMessageOwned, MsgFlags, SockFlag, SockType, recvmsg,
    sendmsg, socketpair,
};

/// Largest metadata record one message may carry.
///
/// Records larger than this are refused on send, and a received message that
/// the kernel had to truncate to fit is refused on receive, so a record is
/// always seen whole or not at all.
pub const MAX_RECORD: usize = 4096;

/// The record-preserving socket type for this target.
#[cfg(not(target_vendor = "apple"))]
const CHANNEL_TYPE: SockType = SockType::SeqPacket;
#[cfg(target_vendor = "apple")]
const CHANNEL_TYPE: SockType = SockType::Datagram;

/// One end of a connected descriptor-passing channel.
///
/// Created in pairs by [`fd_channel_pair`], typically just before a `fork`:
/// the parent keeps one end and the child the other. Both ends are
/// close-on-exec, so neither leaks into a spawned helper program.
#[derive(Debug)]
pub struct FdChannel {
    socket: OwnedFd,
}

/// Creates a connected pair of descriptor-passing channel ends.
pub fn fd_channel_pair() -> io::Result<(FdChannel, FdChannel)> {
    #[cfg(not(target_vendor = "apple"))]
    let flags = SockFlag::SOCK_CLOEXEC;
    #[cfg(target_vendor = "apple")]
    let flags = SockFlag::empty();
    let (a, b) = socketpair(AddressFamily::Unix, CHANNEL_TYPE, None, flags)?;
    #[cfg(target_vendor = "apple")]
    {
        set_cloexec(&a)?;
        set_cloexec(&b)?;
    }
    Ok((FdChannel { socket: a }, FdChannel { socket: b }))
}

impl FdChannel {
    /// Sends `record` together with `fd` as one message.
    ///
    /// The receiver gets its own descriptor for the same open file; the
    /// caller's `fd` stays open and should be closed once the caller no
    /// longer needs it.
    pub fn send(&self, record: &[u8], fd: BorrowedFd<'_>) -> io::Result<()> {
        if record.is_empty() || record.len() > MAX_RECORD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "fd-passing record must be 1..={MAX_RECORD} bytes, got {}",
                    record.len()
                ),
            ));
        }
        let fds = [fd.as_raw_fd()];
        let iov = [IoSlice::new(record)];
        let cmsgs = [ControlMessage::ScmRights(&fds)];
        loop {
            match sendmsg::<()>(
                self.socket.as_raw_fd(),
                &iov,
                &cmsgs,
                MsgFlags::empty(),
                None,
            ) {
                Ok(sent) if sent == record.len() => return Ok(()),
                Ok(sent) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        format!("fd-passing record sent {sent} of {} bytes", record.len()),
                    ));
                }
                // The kernel never ran the send; nothing is being retried.
                Err(nix::errno::Errno::EINTR) => {}
                Err(errno) => return Err(errno.into()),
            }
        }
    }

    /// Receives one message into `buf`, returning the record length and the
    /// descriptor that travelled with it.
    ///
    /// Returns `Ok(None)` once the peer has closed its end. A message that
    /// carries no descriptor, more than one, or a record larger than `buf` is
    /// an error; any descriptors it did carry are closed rather than leaked.
    pub fn recv(&self, buf: &mut [u8]) -> io::Result<Option<(usize, OwnedFd)>> {
        let mut cmsg_buffer = cmsg_space!([RawFd; 1]);
        let (bytes, flags, fds) = loop {
            let mut iov = [IoSliceMut::new(buf)];
            match recvmsg::<()>(
                self.socket.as_raw_fd(),
                &mut iov,
                Some(cmsg_buffer.as_mut_slice()),
                recv_flags(),
            ) {
                Ok(msg) => {
                    let mut fds = Vec::new();
                    for cmsg in msg.cmsgs()? {
                        if let ControlMessageOwned::ScmRights(raw) = cmsg {
                            fds.extend(raw.into_iter().map(own_received_fd));
                        }
                    }
                    break (msg.bytes, msg.flags, fds);
                }
                // The kernel never ran the receive; nothing is being retried.
                Err(nix::errno::Errno::EINTR) => {}
                // A connected `AF_UNIX` datagram socket reports its peer's
                // close as a reset once every queued record has been read.
                #[cfg(target_vendor = "apple")]
                Err(nix::errno::Errno::ECONNRESET) => return Ok(None),
                Err(errno) => return Err(errno.into()),
            }
        };
        if bytes == 0 && fds.is_empty() {
            return Ok(None);
        }
        if flags.contains(MsgFlags::MSG_TRUNC) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("fd-passing record exceeds the {}-byte buffer", buf.len()),
            ));
        }
        #[cfg(target_vendor = "apple")]
        for fd in &fds {
            set_cloexec(fd)?;
        }
        let mut fds = fds.into_iter();
        match (fds.next(), fds.next()) {
            (Some(fd), None) => Ok(Some((bytes, fd))),
            (None, _) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fd-passing record arrived without a descriptor",
            )),
            (Some(_), Some(_)) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fd-passing record carried more than one descriptor",
            )),
        }
    }
}

impl AsFd for FdChannel {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.socket.as_fd()
    }
}

impl AsRawFd for FdChannel {
    fn as_raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }
}

impl From<FdChannel> for OwnedFd {
    fn from(channel: FdChannel) -> Self {
        channel.socket
    }
}

/// Receive flags: received descriptors are close-on-exec from the moment
/// they exist wherever the kernel supports it.
fn recv_flags() -> MsgFlags {
    #[cfg(not(target_vendor = "apple"))]
    {
        MsgFlags::MSG_CMSG_CLOEXEC
    }
    #[cfg(target_vendor = "apple")]
    {
        MsgFlags::empty()
    }
}

/// Takes ownership of a descriptor the kernel installed in this process.
#[allow(unsafe_code)]
fn own_received_fd(raw: RawFd) -> OwnedFd {
    // SAFETY: `raw` comes from an `SCM_RIGHTS` control message that
    // `recvmsg` just decoded. The kernel installed it in this process's
    // descriptor table for this call alone, so nothing else owns or will
    // close it. `nix` exposes the received descriptors only as `RawFd`, so
    // no safe constructor exists to express the ownership transfer.
    unsafe { OwnedFd::from_raw_fd(raw) }
}

/// Marks `fd` close-on-exec, for targets that cannot request it atomically.
#[cfg(target_vendor = "apple")]
fn set_cloexec(fd: &OwnedFd) -> io::Result<()> {
    use nix::fcntl::{FcntlArg, FdFlag, fcntl};
    fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
    Ok(())
}

/// The facts that travel with a relayed connection's descriptor.
///
/// The process that accepts a connection on a transport the daemon parent
/// does not serve itself (the QUIC front process) hands the parent one of
/// these alongside the connection's descriptor, so the parent can admit the
/// connection - `hosts allow`, logging, `max connections` - exactly as it
/// admits an accepted TCP socket. The receiving side is the daemon's
/// accept engine.
///
/// Encoded as a fixed layout: the peer address, the local address, then an
/// optional client identity (the peer's certificate, DER), each address as a
/// family byte (`4` or `6`), the IP octets and a big-endian port, and the
/// identity as a presence byte followed by a big-endian `u16` length and the
/// bytes. [`RelayRecord::decode`] refuses anything that does not consume the
/// record exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayRecord {
    /// Address of the remote peer that opened the connection.
    pub peer: std::net::SocketAddr,
    /// Local address the connection arrived on.
    pub local: std::net::SocketAddr,
    /// The peer's certificate, when the transport authenticated one.
    pub client_identity: Option<Vec<u8>>,
}

impl RelayRecord {
    /// Encodes the record for [`FdChannel::send`].
    ///
    /// Fails when the identity is too long to fit a record of
    /// [`MAX_RECORD`] bytes.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(64);
        encode_addr(&mut out, self.peer);
        encode_addr(&mut out, self.local);
        match &self.client_identity {
            None => out.push(0),
            Some(identity) => {
                let len = u16::try_from(identity.len()).map_err(|_| oversized())?;
                out.push(1);
                out.extend_from_slice(&len.to_be_bytes());
                out.extend_from_slice(identity);
            }
        }
        if out.len() > MAX_RECORD {
            return Err(oversized());
        }
        Ok(out)
    }

    /// Decodes a record produced by [`RelayRecord::encode`].
    ///
    /// A record that is truncated, names an unknown address family, or
    /// carries trailing bytes is refused with `InvalidData`.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        let mut cursor = bytes;
        let peer = decode_addr(&mut cursor)?;
        let local = decode_addr(&mut cursor)?;
        let client_identity = match take(&mut cursor, 1)?[0] {
            0 => None,
            1 => {
                let len =
                    u16::from_be_bytes(take(&mut cursor, 2)?.try_into().map_err(|_| malformed())?);
                Some(take(&mut cursor, usize::from(len))?.to_vec())
            }
            _ => return Err(malformed()),
        };
        if !cursor.is_empty() {
            return Err(malformed());
        }
        Ok(Self {
            peer,
            local,
            client_identity,
        })
    }
}

fn encode_addr(out: &mut Vec<u8>, addr: std::net::SocketAddr) {
    match addr.ip() {
        std::net::IpAddr::V4(ip) => {
            out.push(4);
            out.extend_from_slice(&ip.octets());
        }
        std::net::IpAddr::V6(ip) => {
            out.push(6);
            out.extend_from_slice(&ip.octets());
        }
    }
    out.extend_from_slice(&addr.port().to_be_bytes());
}

fn decode_addr(cursor: &mut &[u8]) -> io::Result<std::net::SocketAddr> {
    let ip = match take(cursor, 1)?[0] {
        4 => {
            let octets: [u8; 4] = take(cursor, 4)?.try_into().map_err(|_| malformed())?;
            std::net::IpAddr::from(octets)
        }
        6 => {
            let octets: [u8; 16] = take(cursor, 16)?.try_into().map_err(|_| malformed())?;
            std::net::IpAddr::from(octets)
        }
        _ => return Err(malformed()),
    };
    let port = u16::from_be_bytes(take(cursor, 2)?.try_into().map_err(|_| malformed())?);
    Ok(std::net::SocketAddr::new(ip, port))
}

fn take<'a>(cursor: &mut &'a [u8], len: usize) -> io::Result<&'a [u8]> {
    if cursor.len() < len {
        return Err(malformed());
    }
    let (head, rest) = cursor.split_at(len);
    *cursor = rest;
    Ok(head)
}

fn malformed() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "malformed relay record")
}

fn oversized() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("relay record exceeds {MAX_RECORD} bytes"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    /// The descriptor that arrives is a working handle to the SAME open
    /// socket that was sent - bytes written through it reach the sender's
    /// peer - and the record arrives byte-exact alongside it.
    #[test]
    fn descriptor_and_record_round_trip() {
        let (tx, rx) = fd_channel_pair().expect("channel pair");
        let (mut near, far) = UnixStream::pair().expect("stream pair");

        tx.send(b"peer=192.0.2.7:4242", far.as_fd()).expect("send");
        drop(far);

        let mut buf = [0u8; MAX_RECORD];
        let (len, fd) = rx.recv(&mut buf).expect("recv").expect("not EOF");
        assert_eq!(&buf[..len], b"peer=192.0.2.7:4242");

        let mut received = UnixStream::from(fd);
        received.write_all(b"hello").expect("write via received fd");
        let mut got = [0u8; 5];
        near.read_exact(&mut got).expect("read at the far peer");
        assert_eq!(&got, b"hello");
    }

    /// Records never merge: two sends arrive as two receives, each with its
    /// own descriptor, in order.
    #[test]
    fn records_keep_their_boundaries() {
        let (tx, rx) = fd_channel_pair().expect("channel pair");
        let (one, _one_peer) = UnixStream::pair().expect("pair one");
        let (two, _two_peer) = UnixStream::pair().expect("pair two");
        tx.send(b"first", one.as_fd()).expect("send one");
        tx.send(b"second-record", two.as_fd()).expect("send two");

        let mut buf = [0u8; MAX_RECORD];
        let (len, _) = rx.recv(&mut buf).expect("recv").expect("first");
        assert_eq!(&buf[..len], b"first");
        let (len, _) = rx.recv(&mut buf).expect("recv").expect("second");
        assert_eq!(&buf[..len], b"second-record");
    }

    /// A closed peer reads as `None` once its queued records are drained,
    /// which is how the receiver learns the sending process is gone.
    #[test]
    fn peer_close_reads_as_end_of_channel() {
        let (tx, rx) = fd_channel_pair().expect("channel pair");
        let (fd, _peer) = UnixStream::pair().expect("pair");
        tx.send(b"last", fd.as_fd()).expect("send");
        drop(tx);
        let mut buf = [0u8; MAX_RECORD];
        // A record sent before the close is still delivered, then the end.
        let (len, _) = rx.recv(&mut buf).expect("recv").expect("queued record");
        assert_eq!(&buf[..len], b"last");
        assert!(rx.recv(&mut buf).expect("recv").is_none());
    }

    /// A record that does not fit the receive buffer is refused instead of
    /// being silently cut short.
    #[test]
    fn oversized_record_is_refused_not_truncated() {
        let (tx, rx) = fd_channel_pair().expect("channel pair");
        let (fd, _peer) = UnixStream::pair().expect("pair");
        tx.send(&[7u8; 64], fd.as_fd()).expect("send");
        let mut small = [0u8; 16];
        let error = rx.recv(&mut small).expect_err("truncation must be refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    /// Empty and over-limit records are rejected before anything is sent.
    #[test]
    fn send_rejects_empty_and_oversized_records() {
        let (tx, _rx) = fd_channel_pair().expect("channel pair");
        let (fd, _peer) = UnixStream::pair().expect("pair");
        for record in [Vec::new(), vec![0u8; MAX_RECORD + 1]] {
            let error = tx.send(&record, fd.as_fd()).expect_err("must reject");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }

    /// Both channel ends and every received descriptor are close-on-exec, so
    /// a hook the daemon spawns cannot inherit a client connection.
    #[test]
    fn channel_and_received_descriptors_are_close_on_exec() {
        use nix::fcntl::{FcntlArg, FdFlag, fcntl};

        let (tx, rx) = fd_channel_pair().expect("channel pair");
        let (fd, _peer) = UnixStream::pair().expect("pair");
        tx.send(b"x", fd.as_fd()).expect("send");
        let mut buf = [0u8; 8];
        let (_, received) = rx.recv(&mut buf).expect("recv").expect("record");

        for owned in [tx.as_fd(), rx.as_fd(), received.as_fd()] {
            let flags = fcntl(owned, FcntlArg::F_GETFD).expect("F_GETFD");
            assert!(
                FdFlag::from_bits_truncate(flags).contains(FdFlag::FD_CLOEXEC),
                "descriptor must be close-on-exec"
            );
        }
    }

    fn record(peer: &str, local: &str, identity: Option<&[u8]>) -> RelayRecord {
        RelayRecord {
            peer: peer.parse().expect("peer"),
            local: local.parse().expect("local"),
            client_identity: identity.map(<[u8]>::to_vec),
        }
    }

    /// Both address families and an identity survive the encoding, so the
    /// parent admits a relayed connection on exactly the address the front
    /// process saw.
    #[test]
    fn relay_record_round_trips_v4_v6_and_identity() {
        for original in [
            record("192.0.2.7:40001", "198.51.100.1:873", None),
            record(
                "[2001:db8::7]:873",
                "[2001:db8::1]:8873",
                Some(b"\x30\x82der"),
            ),
            record("192.0.2.7:1", "[::1]:65535", Some(b"")),
        ] {
            let bytes = original.encode().expect("encode");
            assert_eq!(RelayRecord::decode(&bytes).expect("decode"), original);
        }
    }

    /// A cut-short or padded record is refused, never admitted under a
    /// partially read address: every strict prefix fails, as does one extra
    /// byte and an unknown family.
    #[test]
    fn relay_record_rejects_truncated_padded_and_unknown_family() {
        let bytes = record("[2001:db8::7]:873", "192.0.2.1:873", Some(b"id"))
            .encode()
            .expect("encode");
        for len in 0..bytes.len() {
            let err = RelayRecord::decode(&bytes[..len]).expect_err("truncated must fail");
            assert_eq!(
                err.kind(),
                io::ErrorKind::InvalidData,
                "prefix of {len} bytes"
            );
        }
        let mut padded = bytes.clone();
        padded.push(0);
        assert!(RelayRecord::decode(&padded).is_err());
        let mut bad_family = bytes;
        bad_family[0] = 5;
        assert!(RelayRecord::decode(&bad_family).is_err());
    }

    /// An identity too large for one record is refused on encode rather than
    /// silently cut.
    #[test]
    fn relay_record_refuses_an_identity_larger_than_a_record() {
        let huge = vec![0u8; MAX_RECORD];
        let err = record("192.0.2.7:1", "192.0.2.1:2", Some(&huge))
            .encode()
            .expect_err("oversized");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// The record and the descriptor travel together: the far side decodes
    /// the metadata and talks through the received socket.
    #[test]
    fn relay_record_travels_with_its_descriptor() {
        let (tx, rx) = fd_channel_pair().expect("channel pair");
        let (mut near, far) = UnixStream::pair().expect("stream pair");
        let sent = record("[2001:db8::9]:5000", "[2001:db8::1]:873", None);

        tx.send(&sent.encode().expect("encode"), far.as_fd())
            .expect("send");
        drop(far);

        let mut buf = [0u8; MAX_RECORD];
        let (len, fd) = rx.recv(&mut buf).expect("recv").expect("not EOF");
        assert_eq!(RelayRecord::decode(&buf[..len]).expect("decode"), sent);
        let mut received = UnixStream::from(fd);
        received.write_all(b"ok").expect("write");
        let mut got = [0u8; 2];
        near.read_exact(&mut got).expect("read");
        assert_eq!(&got, b"ok");
    }
}
