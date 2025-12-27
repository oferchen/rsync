use std::iter::FusedIterator;

use super::flags::CompatibilityFlags;
use super::known::KnownCompatibilityFlag;

/// Iterator over the known compatibility flags set within a [`CompatibilityFlags`] value.
///
/// [`Iterator::next`] yields flags in ascending bit order (lowest bit first) so
/// callers observe the same ordering exposed by [`CompatibilityFlags::iter_known`].
/// The iterator also implements [`DoubleEndedIterator`], allowing reverse
/// traversal via [`DoubleEndedIterator::next_back`] without allocating intermediate
/// collections.
#[derive(Clone, Debug)]
pub struct KnownCompatibilityFlagsIter {
    remaining: u32,
}

impl KnownCompatibilityFlagsIter {
    pub(super) const fn new(flags: CompatibilityFlags) -> Self {
        Self {
            remaining: flags.bits() & CompatibilityFlags::KNOWN_MASK,
        }
    }
}

impl Iterator for KnownCompatibilityFlagsIter {
    type Item = KnownCompatibilityFlag;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        let bit_index = self.remaining.trailing_zeros();
        let bit_mask = 1u32 << bit_index;
        self.remaining &= !bit_mask;
        KnownCompatibilityFlag::from_bits(bit_mask)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.remaining.count_ones() as usize;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for KnownCompatibilityFlagsIter {
    fn len(&self) -> usize {
        self.remaining.count_ones() as usize
    }
}

impl FusedIterator for KnownCompatibilityFlagsIter {}

impl DoubleEndedIterator for KnownCompatibilityFlagsIter {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        let bit_index = u32::BITS - 1 - self.remaining.leading_zeros();
        let bit_mask = 1u32 << bit_index;
        self.remaining &= !bit_mask;
        KnownCompatibilityFlag::from_bits(bit_mask)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_flags() -> CompatibilityFlags {
        CompatibilityFlags::from_bits(0)
    }

    #[test]
    fn empty_flags_yields_nothing() {
        let flags = empty_flags();
        let iter = KnownCompatibilityFlagsIter::new(flags);
        assert_eq!(iter.len(), 0);
        assert_eq!(iter.count(), 0);
    }

    #[test]
    fn size_hint_matches_len() {
        let flags = empty_flags();
        let iter = KnownCompatibilityFlagsIter::new(flags);
        let (lower, upper) = iter.size_hint();
        assert_eq!(lower, iter.len());
        assert_eq!(upper, Some(iter.len()));
    }

    #[test]
    fn clone_preserves_state() {
        let flags = empty_flags();
        let iter = KnownCompatibilityFlagsIter::new(flags);
        let cloned = iter.clone();
        assert_eq!(iter.len(), cloned.len());
    }

    #[test]
    fn debug_format() {
        let flags = empty_flags();
        let iter = KnownCompatibilityFlagsIter::new(flags);
        let debug = format!("{iter:?}");
        assert!(debug.contains("KnownCompatibilityFlagsIter"));
    }

    #[test]
    fn fused_iterator_remains_exhausted() {
        let flags = empty_flags();
        let mut iter = KnownCompatibilityFlagsIter::new(flags);
        assert!(iter.next().is_none());
        assert!(iter.next().is_none());
    }

    #[test]
    fn next_back_empty_returns_none() {
        let flags = empty_flags();
        let mut iter = KnownCompatibilityFlagsIter::new(flags);
        assert!(iter.next_back().is_none());
    }

    #[test]
    fn exact_size_iterator_len() {
        let flags = empty_flags();
        let iter = KnownCompatibilityFlagsIter::new(flags);
        assert_eq!(iter.len(), 0);
    }

    #[test]
    fn non_empty_flags_yields_items() {
        let flags = CompatibilityFlags::INC_RECURSE;
        let iter = KnownCompatibilityFlagsIter::new(flags);
        assert!(iter.len() > 0);
    }

    #[test]
    fn forward_iteration_collects_all() {
        let flags = CompatibilityFlags::INC_RECURSE;
        let items: Vec<_> = KnownCompatibilityFlagsIter::new(flags).collect();
        assert!(!items.is_empty());
    }

    #[test]
    fn reverse_iteration_collects_all() {
        let flags = CompatibilityFlags::INC_RECURSE;
        let items: Vec<_> = KnownCompatibilityFlagsIter::new(flags).rev().collect();
        assert!(!items.is_empty());
    }
}
