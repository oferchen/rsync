//! Iterators over the supported protocol versions.

use ::core::iter::FusedIterator;

use super::ProtocolVersion;

/// Iterator over the numeric rsync protocol versions supported by this implementation.
#[must_use = "iterators are lazy and do nothing unless consumed"]
#[derive(Clone, Copy, Debug)]
pub struct SupportedProtocolNumbersIter {
    slice: &'static [u8],
    front: usize,
    back: usize,
}

impl SupportedProtocolNumbersIter {
    pub(crate) const fn new(slice: &'static [u8]) -> Self {
        Self {
            slice,
            front: 0,
            back: slice.len(),
        }
    }

    const fn remaining(&self) -> usize {
        self.back.saturating_sub(self.front)
    }
}

impl Iterator for SupportedProtocolNumbersIter {
    type Item = u8;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }

        let value = self.slice[self.front];
        self.front += 1;
        Some(value)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.remaining();
        (remaining, Some(remaining))
    }
}

impl DoubleEndedIterator for SupportedProtocolNumbersIter {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }

        self.back -= 1;
        Some(self.slice[self.back])
    }
}

impl ExactSizeIterator for SupportedProtocolNumbersIter {
    #[inline]
    fn len(&self) -> usize {
        self.remaining()
    }
}

impl FusedIterator for SupportedProtocolNumbersIter {}

/// Iterator over the strongly typed rsync protocol versions supported by this implementation.
#[must_use = "iterators are lazy and do nothing unless consumed"]
#[derive(Clone, Copy, Debug)]
pub struct SupportedVersionsIter {
    slice: &'static [ProtocolVersion],
    front: usize,
    back: usize,
}

impl SupportedVersionsIter {
    pub(crate) const fn new(slice: &'static [ProtocolVersion]) -> Self {
        Self {
            slice,
            front: 0,
            back: slice.len(),
        }
    }

    const fn remaining(&self) -> usize {
        self.back.saturating_sub(self.front)
    }
}

impl Iterator for SupportedVersionsIter {
    type Item = ProtocolVersion;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }

        let value = self.slice[self.front];
        self.front += 1;
        Some(value)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.remaining();
        (remaining, Some(remaining))
    }
}

impl DoubleEndedIterator for SupportedVersionsIter {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }

        self.back -= 1;
        Some(self.slice[self.back])
    }
}

impl ExactSizeIterator for SupportedVersionsIter {
    #[inline]
    fn len(&self) -> usize {
        self.remaining()
    }
}

impl FusedIterator for SupportedVersionsIter {}
