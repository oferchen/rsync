//! Iterator types for legacy daemon greeting digest lists.

use ::core::iter::FusedIterator;
use std::str::SplitAsciiWhitespace;

/// Iterator over whitespace-separated digest tokens advertised by a legacy daemon.
///
/// Instances of this iterator are created via
/// [`crate::legacy::greeting::LegacyDaemonGreeting::digest_tokens`] or
/// [`crate::legacy::greeting::LegacyDaemonGreetingOwned::digest_tokens`]. The iterator yields each
/// digest exactly once in the
/// order received from the peer, matching upstream rsync's processing of the challenge/response list.
#[derive(Clone, Debug)]
pub struct DigestListTokens<'a> {
    inner: Option<SplitAsciiWhitespace<'a>>,
}

impl<'a> DigestListTokens<'a> {
    pub(super) fn new(digest_list: Option<&'a str>) -> Self {
        Self {
            inner: digest_list.map(|list| list.split_ascii_whitespace()),
        }
    }
}

impl<'a> Iterator for DigestListTokens<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let iter = self.inner.as_mut()?;
        match iter.next() {
            Some(token) => Some(token),
            None => {
                self.inner = None;
                None
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner
            .as_ref()
            .map_or((0, Some(0)), Iterator::size_hint)
    }
}

impl<'a> FusedIterator for DigestListTokens<'a> {}
