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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_with_none_creates_empty_iterator() {
        let tokens = DigestListTokens::new(None);
        assert_eq!(tokens.count(), 0);
    }

    #[test]
    fn new_with_empty_string_yields_nothing() {
        let tokens = DigestListTokens::new(Some(""));
        assert_eq!(tokens.count(), 0);
    }

    #[test]
    fn new_with_single_token_yields_one() {
        let tokens = DigestListTokens::new(Some("md5"));
        let items: Vec<_> = tokens.collect();
        assert_eq!(items, vec!["md5"]);
    }

    #[test]
    fn new_with_multiple_tokens_yields_all() {
        let tokens = DigestListTokens::new(Some("sha512 sha256 sha1 md5"));
        let items: Vec<_> = tokens.collect();
        assert_eq!(items, vec!["sha512", "sha256", "sha1", "md5"]);
    }

    #[test]
    fn extra_whitespace_is_ignored() {
        let tokens = DigestListTokens::new(Some("  sha512   sha256  "));
        let items: Vec<_> = tokens.collect();
        assert_eq!(items, vec!["sha512", "sha256"]);
    }

    #[test]
    fn size_hint_returns_lower_bound() {
        let tokens = DigestListTokens::new(Some("sha512 sha256"));
        let (lower, upper) = tokens.size_hint();
        assert!(lower <= 2);
        assert!(upper.is_some());
    }

    #[test]
    fn size_hint_empty_is_zero() {
        let tokens = DigestListTokens::new(None);
        let (lower, upper) = tokens.size_hint();
        assert_eq!(lower, 0);
        assert_eq!(upper, Some(0));
    }

    #[test]
    fn fused_iterator_stays_exhausted() {
        let mut tokens = DigestListTokens::new(Some("md5"));
        assert_eq!(tokens.next(), Some("md5"));
        assert!(tokens.next().is_none());
        assert!(tokens.next().is_none());
    }

    #[test]
    fn clone_works() {
        let tokens = DigestListTokens::new(Some("sha256 sha1"));
        let cloned = tokens.clone();
        assert_eq!(cloned.count(), 2);
    }

    #[test]
    fn debug_format() {
        let tokens = DigestListTokens::new(Some("md5"));
        let debug = format!("{:?}", tokens);
        assert!(debug.contains("DigestListTokens"));
    }
}
