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

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_NUMBERS: [u8; 4] = [28, 29, 30, 31];
    static TEST_VERSIONS: [ProtocolVersion; 4] = [
        ProtocolVersion::V28,
        ProtocolVersion::V29,
        ProtocolVersion::V30,
        ProtocolVersion::V31,
    ];

    #[test]
    fn numbers_iter_yields_all_elements() {
        let iter = SupportedProtocolNumbersIter::new(&TEST_NUMBERS);
        let collected: Vec<_> = iter.collect();
        assert_eq!(collected, vec![28, 29, 30, 31]);
    }

    #[test]
    fn numbers_iter_empty_slice() {
        let iter = SupportedProtocolNumbersIter::new(&[]);
        assert_eq!(iter.len(), 0);
        assert_eq!(iter.collect::<Vec<_>>(), Vec::<u8>::new());
    }

    #[test]
    fn numbers_iter_size_hint_accurate() {
        let iter = SupportedProtocolNumbersIter::new(&TEST_NUMBERS);
        assert_eq!(iter.size_hint(), (4, Some(4)));
    }

    #[test]
    fn numbers_iter_exact_size() {
        let iter = SupportedProtocolNumbersIter::new(&TEST_NUMBERS);
        assert_eq!(iter.len(), 4);
    }

    #[test]
    fn numbers_iter_double_ended() {
        let mut iter = SupportedProtocolNumbersIter::new(&TEST_NUMBERS);
        assert_eq!(iter.next(), Some(28));
        assert_eq!(iter.next_back(), Some(31));
        assert_eq!(iter.next_back(), Some(30));
        assert_eq!(iter.next(), Some(29));
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next_back(), None);
    }

    #[test]
    fn numbers_iter_fused() {
        let mut iter = SupportedProtocolNumbersIter::new(&[42]);
        assert_eq!(iter.next(), Some(42));
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn numbers_iter_remaining_updates() {
        let mut iter = SupportedProtocolNumbersIter::new(&TEST_NUMBERS);
        assert_eq!(iter.len(), 4);
        iter.next();
        assert_eq!(iter.len(), 3);
        iter.next_back();
        assert_eq!(iter.len(), 2);
    }

    #[test]
    fn versions_iter_yields_all_elements() {
        let iter = SupportedVersionsIter::new(&TEST_VERSIONS);
        let collected: Vec<_> = iter.collect();
        assert_eq!(collected, TEST_VERSIONS.to_vec());
    }

    #[test]
    fn versions_iter_empty_slice() {
        let iter = SupportedVersionsIter::new(&[]);
        assert_eq!(iter.len(), 0);
        assert_eq!(iter.collect::<Vec<_>>(), Vec::<ProtocolVersion>::new());
    }

    #[test]
    fn versions_iter_size_hint_accurate() {
        let iter = SupportedVersionsIter::new(&TEST_VERSIONS);
        assert_eq!(iter.size_hint(), (4, Some(4)));
    }

    #[test]
    fn versions_iter_exact_size() {
        let iter = SupportedVersionsIter::new(&TEST_VERSIONS);
        assert_eq!(iter.len(), 4);
    }

    #[test]
    fn versions_iter_double_ended() {
        let mut iter = SupportedVersionsIter::new(&TEST_VERSIONS);
        assert_eq!(iter.next(), Some(ProtocolVersion::V28));
        assert_eq!(iter.next_back(), Some(ProtocolVersion::V31));
        assert_eq!(iter.next_back(), Some(ProtocolVersion::V30));
        assert_eq!(iter.next(), Some(ProtocolVersion::V29));
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next_back(), None);
    }

    #[test]
    fn versions_iter_fused() {
        let mut iter = SupportedVersionsIter::new(&[ProtocolVersion::V31]);
        assert_eq!(iter.next(), Some(ProtocolVersion::V31));
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn versions_iter_remaining_updates() {
        let mut iter = SupportedVersionsIter::new(&TEST_VERSIONS);
        assert_eq!(iter.len(), 4);
        iter.next();
        assert_eq!(iter.len(), 3);
        iter.next_back();
        assert_eq!(iter.len(), 2);
    }

    #[test]
    fn numbers_iter_cloneable() {
        let iter = SupportedProtocolNumbersIter::new(&TEST_NUMBERS);
        let cloned = iter;
        assert_eq!(cloned.len(), 4);
    }

    #[test]
    fn versions_iter_cloneable() {
        let iter = SupportedVersionsIter::new(&TEST_VERSIONS);
        let cloned = iter;
        assert_eq!(cloned.len(), 4);
    }
}
