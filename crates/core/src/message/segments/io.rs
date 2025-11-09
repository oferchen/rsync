use std::io::{self, IoSlice, Write as IoWrite};

use super::super::OVERREPORTED_BYTES_ERROR;
use super::base::MessageSegments;

impl<'a> MessageSegments<'a> {
    /// Streams the message segments into the provided writer.
    ///
    /// The helper prefers vectored writes when the message spans multiple
    /// segments so downstream sinks receive the payload in a single
    /// [`write_vectored`](IoWrite::write_vectored) call. Leading empty slices are
    /// trimmed before issuing the first vectored write to avoid writers
    /// reporting a spurious [`io::ErrorKind::WriteZero`] even though payload
    /// bytes remain. When the writer reports that vectored I/O is unsupported or
    /// performs a partial write, the remaining bytes are flushed sequentially to
    /// mirror upstream rsync's formatting logic.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let mut scratch = MessageScratch::new();
    /// let message = Message::error(12, "example")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    ///
    /// let segments = message.as_segments(&mut scratch, false);
    /// let mut buffer = Vec::new();
    /// segments.write_to(&mut buffer).unwrap();
    ///
    /// assert_eq!(buffer, message.to_bytes().unwrap());
    /// ```
    #[must_use = "rsync message streaming can fail when the underlying writer reports an I/O error"]
    pub fn write_to<W: IoWrite>(&self, writer: &mut W) -> io::Result<()> {
        if self.is_empty() {
            return Ok(());
        }

        if self.count == 1 {
            let bytes: &[u8] = self.segments[0].as_ref();

            if bytes.is_empty() {
                return Ok(());
            }

            writer.write_all(bytes)?;
            return Ok(());
        }

        let borrowed = trim_leading_empty_slices(&self.segments[..self.count]);
        let mut remaining = self.total_len;

        if borrowed.is_empty() {
            return Ok(());
        }

        loop {
            match writer.write_vectored(borrowed) {
                Ok(0) => {
                    return Err(io::Error::from(io::ErrorKind::WriteZero));
                }
                Ok(written) => {
                    if written > remaining {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            OVERREPORTED_BYTES_ERROR,
                        ));
                    }
                    remaining -= written;

                    if remaining == 0 {
                        return Ok(());
                    }

                    let mut storage = self.segments;
                    let mut view = trim_leading_empty_slices_mut(&mut storage[..self.count]);
                    IoSlice::advance_slices(&mut view, written);
                    view = trim_leading_empty_slices_mut(view);

                    return write_owned_view(writer, view, remaining);
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::Unsupported => {
                    return write_borrowed_sequential(writer, borrowed, remaining);
                }
                Err(err) => return Err(err),
            }
        }
    }
}

fn write_owned_view<'a, W: IoWrite>(
    writer: &mut W,
    mut view: &mut [IoSlice<'a>],
    mut remaining: usize,
) -> io::Result<()> {
    while !view.is_empty() && remaining != 0 {
        match writer.write_vectored(view) {
            Ok(0) => {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            Ok(written) => {
                if written > remaining {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        OVERREPORTED_BYTES_ERROR,
                    ));
                }
                remaining -= written;

                if remaining == 0 {
                    return Ok(());
                }

                IoSlice::advance_slices(&mut view, written);
                view = trim_leading_empty_slices_mut(view);
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) if err.kind() == io::ErrorKind::Unsupported => break,
            Err(err) => return Err(err),
        }
    }

    write_borrowed_sequential(writer, view, remaining)
}

fn write_borrowed_sequential<W: IoWrite>(
    writer: &mut W,
    slices: &[IoSlice<'_>],
    mut remaining: usize,
) -> io::Result<()> {
    let view = trim_leading_empty_slices(slices);

    for slice in view.iter() {
        let bytes: &[u8] = slice.as_ref();

        if bytes.is_empty() {
            continue;
        }

        if bytes.len() > remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                OVERREPORTED_BYTES_ERROR,
            ));
        }

        writer.write_all(bytes)?;
        debug_assert!(bytes.len() <= remaining);
        remaining -= bytes.len();
    }

    if remaining != 0 {
        return Err(io::Error::from(io::ErrorKind::WriteZero));
    }

    Ok(())
}

fn trim_leading_empty_slices_mut<'a, 'b>(
    mut slices: &'b mut [IoSlice<'a>],
) -> &'b mut [IoSlice<'a>] {
    loop {
        let Some(is_empty) = slices.first().map(|slice| {
            let bytes: &[u8] = slice.as_ref();
            bytes.is_empty()
        }) else {
            return slices;
        };

        if !is_empty {
            return slices;
        }

        let (_, rest) = slices
            .split_first_mut()
            .expect("slice is non-empty after first() check");
        slices = rest;
    }
}

fn trim_leading_empty_slices<'a, 'b>(mut slices: &'b [IoSlice<'a>]) -> &'b [IoSlice<'a>] {
    while let Some((first, rest)) = slices.split_first() {
        let first_bytes: &[u8] = first.as_ref();
        if !first_bytes.is_empty() {
            break;
        }

        slices = rest;
    }

    slices
}
