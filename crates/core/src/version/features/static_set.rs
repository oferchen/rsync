use std::iter::FusedIterator;

use super::CompiledFeature;
use super::bitmap::{COMPILED_FEATURE_BITMAP, COMPILED_FEATURE_COUNT};

/// Zero-allocation view of the compiled feature list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaticCompiledFeatures {
    features: [CompiledFeature; COMPILED_FEATURE_COUNT],
    len: usize,
    bitmap: u8,
}

impl StaticCompiledFeatures {
    const fn new() -> Self {
        let mut features = [CompiledFeature::Acl; COMPILED_FEATURE_COUNT];
        let mut len = 0usize;
        let mut index = 0usize;

        if COMPILED_FEATURE_BITMAP != 0 {
            while index < COMPILED_FEATURE_COUNT {
                let feature = CompiledFeature::ALL[index];
                if feature.is_enabled() {
                    features[len] = feature;
                    len += 1;
                }

                index += 1;
            }
        }

        Self {
            features,
            len,
            bitmap: COMPILED_FEATURE_BITMAP,
        }
    }

    /// Returns the number of compiled features captured by the view.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Reports whether any optional features were compiled in.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Exposes the canonical slice describing the compiled feature list.
    #[must_use]
    pub fn as_slice(&self) -> &[CompiledFeature] {
        &self.features[..self.len]
    }

    /// Returns the bitmap describing which optional capabilities were compiled in.
    #[must_use]
    pub const fn bitmap(&self) -> u8 {
        self.bitmap
    }

    /// Reports whether the provided feature is part of the compiled set.
    #[must_use]
    pub const fn contains(&self, feature: CompiledFeature) -> bool {
        (self.bitmap & feature.bit()) != 0
    }

    /// Returns an iterator over the compiled features without allocating.
    #[must_use]
    pub const fn iter(&self) -> StaticCompiledFeaturesIter<'_> {
        StaticCompiledFeaturesIter::new(&self.features, self.len)
    }
}

impl Default for StaticCompiledFeatures {
    fn default() -> Self {
        Self::new()
    }
}

impl AsRef<[CompiledFeature]> for StaticCompiledFeatures {
    fn as_ref(&self) -> &[CompiledFeature] {
        self.as_slice()
    }
}

/// Iterator over the statically computed feature set.
#[derive(Clone, Debug)]
pub struct StaticCompiledFeaturesIter<'a> {
    slice: &'a [CompiledFeature; COMPILED_FEATURE_COUNT],
    start: usize,
    end: usize,
}

impl<'a> StaticCompiledFeaturesIter<'a> {
    pub(super) const fn new(
        slice: &'a [CompiledFeature; COMPILED_FEATURE_COUNT],
        len: usize,
    ) -> Self {
        Self {
            slice,
            start: 0,
            end: len,
        }
    }
}

impl<'a> Iterator for StaticCompiledFeaturesIter<'a> {
    type Item = CompiledFeature;

    fn next(&mut self) -> Option<Self::Item> {
        if self.start >= self.end {
            return None;
        }

        let feature = self.slice[self.start];
        self.start += 1;
        Some(feature)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end.saturating_sub(self.start);
        (remaining, Some(remaining))
    }
}

impl<'a> DoubleEndedIterator for StaticCompiledFeaturesIter<'a> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.start >= self.end {
            return None;
        }

        self.end -= 1;
        Some(self.slice[self.end])
    }
}

impl<'a> ExactSizeIterator for StaticCompiledFeaturesIter<'a> {
    fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

impl<'a> FusedIterator for StaticCompiledFeaturesIter<'a> {}

impl<'a> IntoIterator for &'a StaticCompiledFeatures {
    type Item = CompiledFeature;
    type IntoIter = StaticCompiledFeaturesIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Statically computed compiled feature view used by helper accessors.
pub const COMPILED_FEATURES_STATIC: StaticCompiledFeatures = StaticCompiledFeatures::new();
