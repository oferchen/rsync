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
