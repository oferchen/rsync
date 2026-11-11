//! Parent-to-child hand-off of one accepted connection on Windows.
//!
//! Upstream forks per connection; the child keeps the accepted socket and the
//! parsed configuration, then serves the session (socket.c:755-780). A Windows
//! daemon starts a fresh process instead, so everything the child needs
//! crosses one pipe as a single [`ConnectionHandoff`](crate::connection_handoff::ConnectionHandoff) frame: the duplicated
//! socket and the daemon's own arguments. The child re-reads the configuration
//! from those arguments, which upstream's child does as well: `start_daemon()`
//! calls `load_config()` for every connection (clientserver.c:1434).
//!
//! The frame comes from the daemon, but the child must still refuse a
//! malformed one rather than act on it. Decoding bounds every field before
//! allocating, rejects unknown versions and trailing bytes, and never panics.
//! The encoding is canonical, so a frame that decodes re-encodes to the same
//! bytes.
//!
//! Frame layout (all integers little-endian):
//!
//! ```text
//! u32 body length
//! body:
//!   [u8; 4] magic "OCCH"
//!   u8      version (1)
//!   u8      load default paths (0|1)
//!   u16 len + bytes          duplicated socket description
//!   u16 count                daemon arguments, each:
//!     u16 len + u16 units    one argument as UTF-16 code units
//! ```
//!
//! Arguments travel as UTF-16 code units so a Windows argument that is not
//! valid Unicode crosses unchanged.

use std::fmt;
use std::io::{self, Read, Write};

const MAGIC: [u8; 4] = *b"OCCH";
const VERSION: u8 = 1;

/// Largest duplicated-socket description. A Windows `WSAPROTOCOL_INFOW` is
/// 628 bytes; the bound leaves room without admitting arbitrary sizes.
pub const MAX_SOCKET_INFO_LEN: usize = 1024;

/// Most daemon arguments a frame carries.
pub const MAX_ARGUMENTS: usize = 4096;

/// Longest single argument, in UTF-16 code units: the Windows command-line
/// limit, which no argument the daemon was started with can exceed.
pub const MAX_ARGUMENT_UNITS: usize = 32_767;

/// Largest body [`ConnectionHandoff::read_from`] accepts. The daemon's whole
/// command line fits in [`MAX_ARGUMENT_UNITS`] units, so the arguments never
/// need more than that many units in total plus their length prefixes.
pub const MAX_FRAME_LEN: usize =
    MAGIC.len() + 1 + 1 + 2 + MAX_SOCKET_INFO_LEN + 2 + MAX_ARGUMENTS * 2 + MAX_ARGUMENT_UNITS * 2;

/// What a session child needs to serve one connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionHandoff {
    /// Opaque description of the duplicated client socket.
    pub socket_info: Vec<u8>,
    /// Whether the daemon consults the default configuration paths.
    pub load_default_paths: bool,
    /// The daemon's arguments, each as UTF-16 code units.
    pub arguments: Vec<Vec<u16>>,
}

/// Why a connection hand-off frame was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum HandoffError {
    /// The frame ended before a field was complete.
    Truncated,
    /// The body did not start with the hand-off magic.
    BadMagic,
    /// The frame was written by an unknown codec version.
    UnsupportedVersion(u8),
    /// The default-paths flag was neither 0 nor 1.
    BadFlag(u8),
    /// A field or count exceeded its bound.
    TooLong {
        /// Field name.
        field: &'static str,
        /// Length the frame claimed or the caller supplied.
        len: usize,
        /// Largest accepted length.
        max: usize,
    },
    /// Bytes remained after the last field.
    TrailingBytes(usize),
}

impl fmt::Display for HandoffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("connection hand-off frame is truncated"),
            Self::BadMagic => f.write_str("connection hand-off frame has a bad magic"),
            Self::UnsupportedVersion(v) => {
                write!(f, "connection hand-off version {v} is not supported")
            }
            Self::BadFlag(flag) => {
                write!(
                    f,
                    "connection hand-off default-paths flag {flag} is invalid"
                )
            }
            Self::TooLong { field, len, max } => write!(
                f,
                "connection hand-off field {field} is {len}, more than {max}"
            ),
            Self::TrailingBytes(n) => {
                write!(f, "connection hand-off frame has {n} trailing bytes")
            }
        }
    }
}

impl std::error::Error for HandoffError {}

impl From<HandoffError> for io::Error {
    fn from(error: HandoffError) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, error)
    }
}

impl ConnectionHandoff {
    /// Encodes the body, refusing any field over its bound so the child can
    /// never be handed a frame it would reject.
    pub fn encode(&self) -> Result<Vec<u8>, HandoffError> {
        check("socket_info", self.socket_info.len(), MAX_SOCKET_INFO_LEN)?;
        check("arguments", self.arguments.len(), MAX_ARGUMENTS)?;
        let mut out = Vec::with_capacity(16 + self.socket_info.len());
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.push(u8::from(self.load_default_paths));
        out.extend_from_slice(&(self.socket_info.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.socket_info);
        out.extend_from_slice(&(self.arguments.len() as u16).to_le_bytes());
        for argument in &self.arguments {
            check("argument", argument.len(), MAX_ARGUMENT_UNITS)?;
            out.extend_from_slice(&(argument.len() as u16).to_le_bytes());
            for unit in argument {
                out.extend_from_slice(&unit.to_le_bytes());
            }
        }
        check("frame", out.len(), MAX_FRAME_LEN)?;
        Ok(out)
    }

    /// Decodes a body produced by [`encode`](Self::encode).
    pub fn decode(body: &[u8]) -> Result<Self, HandoffError> {
        let mut cur = Cursor { rest: body };
        if cur.take(MAGIC.len())? != MAGIC {
            return Err(HandoffError::BadMagic);
        }
        let version = cur.u8()?;
        if version != VERSION {
            return Err(HandoffError::UnsupportedVersion(version));
        }
        let load_default_paths = match cur.u8()? {
            0 => false,
            1 => true,
            flag => return Err(HandoffError::BadFlag(flag)),
        };
        let info_len = usize::from(cur.u16()?);
        check("socket_info", info_len, MAX_SOCKET_INFO_LEN)?;
        let socket_info = cur.take(info_len)?.to_vec();
        let count = usize::from(cur.u16()?);
        check("arguments", count, MAX_ARGUMENTS)?;
        let mut arguments = Vec::with_capacity(count);
        for _ in 0..count {
            let units = usize::from(cur.u16()?);
            check("argument", units, MAX_ARGUMENT_UNITS)?;
            let bytes = cur.take(units * 2)?;
            arguments.push(
                bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect(),
            );
        }
        if !cur.rest.is_empty() {
            return Err(HandoffError::TrailingBytes(cur.rest.len()));
        }
        Ok(Self {
            socket_info,
            load_default_paths,
            arguments,
        })
    }

    /// Writes one length-prefixed frame.
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let body = self.encode()?;
        writer.write_all(&(body.len() as u32).to_le_bytes())?;
        writer.write_all(&body)?;
        writer.flush()
    }

    /// Reads one length-prefixed frame, refusing a length over
    /// [`MAX_FRAME_LEN`] before allocating for it.
    pub fn read_from<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut len = [0u8; 4];
        reader.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len) as usize;
        check("frame", len, MAX_FRAME_LEN)?;
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body)?;
        Ok(Self::decode(&body)?)
    }
}

fn check(field: &'static str, len: usize, max: usize) -> Result<(), HandoffError> {
    if len > max {
        return Err(HandoffError::TooLong { field, len, max });
    }
    Ok(())
}

struct Cursor<'a> {
    rest: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], HandoffError> {
        if self.rest.len() < n {
            return Err(HandoffError::Truncated);
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, HandoffError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, HandoffError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn sample() -> ConnectionHandoff {
        ConnectionHandoff {
            socket_info: vec![0xAB; 628],
            load_default_paths: false,
            arguments: vec![
                units("--port=8730"),
                units("--config=C:\\rsyncd.conf"),
                vec![0xD800, u16::from(b'x')],
            ],
        }
    }

    #[test]
    fn a_frame_round_trips_through_the_pipe() {
        let mut pipe = Vec::new();
        sample().write_to(&mut pipe).expect("write");
        assert_eq!(
            ConnectionHandoff::read_from(&mut pipe.as_slice()).expect("read"),
            sample()
        );
    }

    /// An argument that is not valid UTF-16 (a lone surrogate) is still a
    /// valid Windows argument and must cross unchanged.
    #[test]
    fn an_argument_that_is_not_valid_unicode_crosses_unchanged() {
        let body = sample().encode().expect("encode");
        let decoded = ConnectionHandoff::decode(&body).expect("decode");
        assert_eq!(decoded.arguments[2], vec![0xD800, u16::from(b'x')]);
        assert_eq!(decoded.encode().expect("re-encode"), body);
    }

    #[test]
    fn every_truncation_is_refused() {
        let body = sample().encode().expect("encode");
        for cut in 0..body.len() {
            assert!(
                ConnectionHandoff::decode(&body[..cut]).is_err(),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn trailing_bytes_a_bad_magic_and_a_new_version_are_refused() {
        let body = sample().encode().expect("encode");
        let mut trailing = body.clone();
        trailing.push(0);
        assert_eq!(
            ConnectionHandoff::decode(&trailing),
            Err(HandoffError::TrailingBytes(1))
        );
        let mut magic = body.clone();
        magic[0] ^= 1;
        assert_eq!(
            ConnectionHandoff::decode(&magic),
            Err(HandoffError::BadMagic)
        );
        let mut version = body.clone();
        version[4] = 2;
        assert_eq!(
            ConnectionHandoff::decode(&version),
            Err(HandoffError::UnsupportedVersion(2))
        );
        let mut flag = body;
        flag[5] = 2;
        assert_eq!(
            ConnectionHandoff::decode(&flag),
            Err(HandoffError::BadFlag(2))
        );
    }

    /// A length prefix past the bound is refused before the body is read, so a
    /// corrupt pipe cannot make the child allocate gigabytes.
    #[test]
    fn an_oversized_frame_is_refused_before_allocation() {
        let mut pipe = u32::MAX.to_le_bytes().to_vec();
        pipe.extend_from_slice(&[0; 16]);
        let error = ConnectionHandoff::read_from(&mut pipe.as_slice()).expect_err("refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_field_over_its_bound_is_refused_on_both_sides() {
        let long = ConnectionHandoff {
            socket_info: vec![0; MAX_SOCKET_INFO_LEN + 1],
            load_default_paths: true,
            arguments: Vec::new(),
        };
        assert!(matches!(
            long.encode(),
            Err(HandoffError::TooLong {
                field: "socket_info",
                ..
            })
        ));
        let mut body = sample().encode().expect("encode");
        let info_len_at = MAGIC.len() + 2;
        body[info_len_at..info_len_at + 2]
            .copy_from_slice(&((MAX_SOCKET_INFO_LEN + 1) as u16).to_le_bytes());
        assert!(matches!(
            ConnectionHandoff::decode(&body),
            Err(HandoffError::TooLong {
                field: "socket_info",
                ..
            })
        ));
    }

    /// The daemon's command line cannot exceed the Windows limit, so the
    /// largest frame it can ever produce fits the frame bound.
    #[test]
    fn a_command_line_at_the_windows_limit_fits_the_frame_bound() {
        let largest = ConnectionHandoff {
            socket_info: vec![0; MAX_SOCKET_INFO_LEN],
            load_default_paths: true,
            arguments: vec![vec![u16::from(b'a'); MAX_ARGUMENT_UNITS]],
        };
        assert!(largest.encode().expect("encode").len() <= MAX_FRAME_LEN);
    }
}
