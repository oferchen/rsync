use std::io;
use std::iter::FusedIterator;

use crate::envelope::MessageCode;

use super::frame::MessageFrame;
use super::helpers::{decode_frame_parts, trailing_frame_data_error};

/// A view into a multiplexed message that borrows the payload from the input slice.
///
/// Borrowed frames are useful when iterating over byte buffers captured from upstream rsync
/// sessions (for example golden transcripts) because they avoid cloning the payload while still
/// validating the header and payload length. Callers can convert the borrowed representation into
/// an owned [`MessageFrame`] via [`BorrowedMessageFrame::into_owned`] if they need to mutate the
/// payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorrowedMessageFrame<'a> {
    code: MessageCode,
    payload: &'a [u8],
}

impl<'a> BorrowedMessageFrame<'a> {
    /// Returns the message code associated with the frame.
    #[must_use]
    #[inline]
    pub const fn code(&self) -> MessageCode {
        self.code
    }

    /// Returns the payload bytes carried by the frame.
    #[must_use]
    #[inline]
    pub fn payload(&self) -> &'a [u8] {
        self.payload
    }

    /// Returns the payload length in bytes.
    #[must_use]
    #[inline]
    pub fn payload_len(&self) -> usize {
        self.payload.len()
    }

    /// Converts the borrowed frame into an owned [`MessageFrame`].
    pub fn into_owned(self) -> io::Result<MessageFrame> {
        MessageFrame::new(self.code, self.payload.to_vec())
    }

    /// Decodes a multiplexed frame from the beginning of `bytes` without cloning the payload.
    ///
    /// The returned tuple contains the borrowed frame and a slice pointing at the remaining bytes.
    /// Callers that require the slice to contain exactly one frame can use
    /// [`BorrowedMessageFrame::try_from`] to receive an error when trailing data is present. Callers
    /// that require an owned representation can use [`BorrowedMessageFrame::into_owned`] on the
    /// borrowed value. Invalid headers or truncated payloads surface the same errors as
    /// [`MessageFrame::decode_from_slice`].
    ///
    /// # Examples
    ///
    /// ```
    /// use oc_rsync_protocol::{BorrowedMessageFrame, MessageCode, MessageHeader};
    ///
    /// let header = MessageHeader::new(MessageCode::Info, 3).unwrap();
    /// let mut bytes = Vec::from(header.encode());
    /// bytes.extend_from_slice(b"abc");
    /// let (frame, remainder) = BorrowedMessageFrame::decode_from_slice(&bytes).unwrap();
    ///
    /// assert_eq!(frame.code(), MessageCode::Info);
    /// assert_eq!(frame.payload(), b"abc");
    /// assert!(remainder.is_empty());
    /// ```
    pub fn decode_from_slice(bytes: &'a [u8]) -> io::Result<(Self, &'a [u8])> {
        let (header, payload, remainder) = decode_frame_parts(bytes)?;
        Ok((
            Self {
                code: header.code(),
                payload,
            },
            remainder,
        ))
    }
}

/// Iterator over multiplexed frames encoded in a contiguous byte slice.
///
/// [`BorrowedMessageFrames`] repeatedly invokes
/// [`BorrowedMessageFrame::decode_from_slice`] to yield borrowed views of each
/// frame stored in the underlying slice. The iterator stops once every frame has
/// been decoded or an error is encountered. Callers can inspect
/// [`BorrowedMessageFrames::remainder`] to determine whether trailing bytes are
/// left over after iteration completes.
///
/// # Examples
///
/// ```
/// # use oc_rsync_protocol::{BorrowedMessageFrames, MessageCode, MessageHeader};
/// # fn example() -> std::io::Result<()> {
/// let mut bytes = Vec::new();
/// let header = MessageHeader::new(MessageCode::Info, 3).expect("payload fits in header");
/// bytes.extend_from_slice(&header.encode());
/// bytes.extend_from_slice(b"abc");
/// let header = MessageHeader::new(MessageCode::Error, 0).expect("payload fits in header");
/// bytes.extend_from_slice(&header.encode());
///
/// let mut iter = BorrowedMessageFrames::new(&bytes);
/// let first = iter.next().unwrap()?;
/// assert_eq!(first.code(), MessageCode::Info);
/// assert_eq!(first.payload(), b"abc");
/// let second = iter.next().unwrap()?;
/// assert_eq!(second.code(), MessageCode::Error);
/// assert!(second.payload().is_empty());
/// assert!(iter.next().is_none());
/// assert!(iter.remainder().is_empty());
/// # Ok(())
/// # }
/// # example().unwrap();
/// ```
#[derive(Clone, Debug)]
pub struct BorrowedMessageFrames<'a> {
    remaining: &'a [u8],
    finished: bool,
}

impl<'a> BorrowedMessageFrames<'a> {
    /// Creates a new iterator over multiplexed frames stored in `bytes`.
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self {
            remaining: bytes,
            finished: false,
        }
    }

    /// Returns the slice remaining after the iterator has finished decoding frames.
    #[must_use]
    pub fn remainder(&self) -> &'a [u8] {
        self.remaining
    }
}

impl<'a> Iterator for BorrowedMessageFrames<'a> {
    type Item = io::Result<BorrowedMessageFrame<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished || self.remaining.is_empty() {
            self.finished = true;
            return None;
        }

        match BorrowedMessageFrame::decode_from_slice(self.remaining) {
            Ok((frame, remainder)) => {
                self.remaining = remainder;
                Some(Ok(frame))
            }
            Err(err) => {
                self.finished = true;
                Some(Err(err))
            }
        }
    }
}

impl<'a> FusedIterator for BorrowedMessageFrames<'a> {}

impl<'a> TryFrom<&'a [u8]> for BorrowedMessageFrame<'a> {
    type Error = io::Error;

    fn try_from(bytes: &'a [u8]) -> Result<Self, Self::Error> {
        let (frame, remainder) = BorrowedMessageFrame::decode_from_slice(bytes)?;
        if remainder.is_empty() {
            Ok(frame)
        } else {
            Err(trailing_frame_data_error(remainder.len()))
        }
    }
}
