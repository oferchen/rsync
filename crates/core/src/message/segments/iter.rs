use std::io::IoSlice;
use std::slice;

use super::{MAX_MESSAGE_SEGMENTS, MessageSegments};

impl<'a> AsRef<[IoSlice<'a>]> for MessageSegments<'a> {
    #[inline]
    fn as_ref(&self) -> &[IoSlice<'a>] {
        self.as_slices()
    }
}

impl<'a> IntoIterator for &'a MessageSegments<'a> {
    type Item = &'a IoSlice<'a>;
    type IntoIter = slice::Iter<'a, IoSlice<'a>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut MessageSegments<'a> {
    type Item = &'a mut IoSlice<'a>;
    type IntoIter = slice::IterMut<'a, IoSlice<'a>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<'a> IntoIterator for MessageSegments<'a> {
    type Item = IoSlice<'a>;
    type IntoIter = std::iter::Take<std::array::IntoIter<IoSlice<'a>, MAX_MESSAGE_SEGMENTS>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.segments.into_iter().take(self.count)
    }
}
