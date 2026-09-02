//! Non-Unix lookup stubs that delegate to the thread-local name converter.
//!
//! On platforms without POSIX NSS (Windows, etc.), lookups succeed only when
//! a name converter is installed via [`super::set_name_converter`]. Without
//! a converter, all lookups return `Ok(None)`.
//!
//! These share
//! [`with_name_converter`](super::converter::with_name_converter) with the
//! POSIX path. There is no host database to fall back to here, so the
//! helper's "converter installed" and "converter answered" arms collapse to
//! the same `Ok(None)` - but going through it keeps one description of how a
//! converter is consulted instead of four copies of the slot dance. A
//! converter that could not answer at all is *not* part of that collapse: it
//! is an error on this path exactly as it is on the POSIX one.

use super::converter::with_name_converter;
use super::error::LookupResult;

/// Looks up the username for a given UID.
///
/// Delegates to the thread-local name converter if installed, otherwise
/// returns `Ok(None)`.
pub fn lookup_user_name(uid: u32) -> LookupResult<Vec<u8>> {
    match with_name_converter(|nc| nc.uid_to_name(uid)) {
        Some(outcome) => outcome
            .into_lookup()
            .map(|name| name.map(String::into_bytes)),
        None => Ok(None),
    }
}

/// Looks up the UID for a given username.
///
/// Delegates to the thread-local name converter if installed, otherwise
/// returns `Ok(None)`.
pub fn lookup_user_by_name(name: &[u8]) -> LookupResult<u32> {
    // upstream: uidlist.c:146-147 `user_to_uid()` - `if (!name || !*name)
    // return 0;`. The guard is in the caller, not in the converter protocol
    // (`namecvt_safe_token("")` is happily True), so it has to be repeated on
    // every platform's lookup or an empty name reaches the converter as the
    // bare request line `usr \n`.
    if name.is_empty() {
        return Ok(None);
    }

    let Ok(name_str) = std::str::from_utf8(name) else {
        return Ok(None);
    };
    match with_name_converter(|nc| nc.name_to_uid(name_str)) {
        Some(outcome) => outcome.into_lookup(),
        None => Ok(None),
    }
}

/// Looks up the group name for a given GID.
///
/// Delegates to the thread-local name converter if installed, otherwise
/// returns `Ok(None)`.
pub fn lookup_group_name(gid: u32) -> LookupResult<Vec<u8>> {
    match with_name_converter(|nc| nc.gid_to_name(gid)) {
        Some(outcome) => outcome
            .into_lookup()
            .map(|name| name.map(String::into_bytes)),
        None => Ok(None),
    }
}

/// Looks up the GID for a given group name.
///
/// Delegates to the thread-local name converter if installed, otherwise
/// returns `Ok(None)`.
pub fn lookup_group_by_name(name: &[u8]) -> LookupResult<u32> {
    // upstream: uidlist.c:172-173 `group_to_gid()` - the same empty-name rule
    // as `user_to_uid()`, decided before any back end is consulted.
    if name.is_empty() {
        return Ok(None);
    }

    let Ok(name_str) = std::str::from_utf8(name) else {
        return Ok(None);
    };
    match with_name_converter(|nc| nc.name_to_gid(name_str)) {
        Some(outcome) => outcome.into_lookup(),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id_lookup::converter::{
        ConverterOutcome, NameConverterCallbacks, clear_name_converter, set_name_converter,
    };
    use std::io;

    struct TestConverter;

    impl NameConverterCallbacks for TestConverter {
        fn uid_to_name(&mut self, uid: u32) -> ConverterOutcome<String> {
            if uid == 0 {
                ConverterOutcome::Resolved("root".to_string())
            } else {
                ConverterOutcome::Unknown
            }
        }
        fn gid_to_name(&mut self, gid: u32) -> ConverterOutcome<String> {
            if gid == 0 {
                ConverterOutcome::Resolved("wheel".to_string())
            } else {
                ConverterOutcome::Unknown
            }
        }
        fn name_to_uid(&mut self, name: &str) -> ConverterOutcome<u32> {
            if name == "root" {
                ConverterOutcome::Resolved(0)
            } else {
                ConverterOutcome::Unknown
            }
        }
        fn name_to_gid(&mut self, name: &str) -> ConverterOutcome<u32> {
            if name == "wheel" {
                ConverterOutcome::Resolved(0)
            } else {
                ConverterOutcome::Unknown
            }
        }
    }

    /// A converter whose request/answer stream has broken.
    struct DeadConverter;

    impl NameConverterCallbacks for DeadConverter {
        fn uid_to_name(&mut self, _uid: u32) -> ConverterOutcome<String> {
            ConverterOutcome::Failed(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn gid_to_name(&mut self, _gid: u32) -> ConverterOutcome<String> {
            ConverterOutcome::Failed(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn name_to_uid(&mut self, _name: &str) -> ConverterOutcome<u32> {
            ConverterOutcome::Failed(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn name_to_gid(&mut self, _name: &str) -> ConverterOutcome<u32> {
            ConverterOutcome::Failed(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn lookup_user_name_without_converter_returns_none() {
        clear_name_converter();
        let result = lookup_user_name(0).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn lookup_group_name_without_converter_returns_none() {
        clear_name_converter();
        let result = lookup_group_name(0).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn lookup_user_by_name_without_converter_returns_none() {
        clear_name_converter();
        let result = lookup_user_by_name(b"root").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn lookup_group_by_name_without_converter_returns_none() {
        clear_name_converter();
        let result = lookup_group_by_name(b"wheel").unwrap();
        assert!(result.is_none());
    }

    /// upstream: uidlist.c:146-147 / :172-173 - an empty name is decided
    /// before any back end is consulted, so no request is framed for it.
    #[test]
    fn an_empty_name_never_reaches_the_converter() {
        set_name_converter(Box::new(DeadConverter));

        // A converter that fails every query it is asked makes the guard
        // observable: without it, these would be errors, not `None`.
        assert!(
            lookup_user_by_name(b"")
                .expect("empty name is not a query")
                .is_none()
        );
        assert!(
            lookup_group_by_name(b"")
                .expect("empty name is not a query")
                .is_none()
        );

        // Non-vacuity: the same converter IS installed and IS reached by a
        // non-empty name.
        assert!(lookup_user_by_name(b"root").is_err());

        clear_name_converter();
    }

    /// A converter that cannot answer must not read as "no such name".
    #[test]
    fn a_failed_converter_is_an_error_not_a_missing_name() {
        set_name_converter(Box::new(DeadConverter));

        assert!(lookup_user_name(0).is_err());
        assert!(lookup_group_name(0).is_err());
        assert!(lookup_user_by_name(b"root").is_err());
        assert!(lookup_group_by_name(b"wheel").is_err());

        clear_name_converter();
    }

    #[test]
    fn lookup_user_by_name_invalid_utf8_returns_none() {
        let result = lookup_user_by_name(b"\xff\xfe").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn lookup_group_by_name_invalid_utf8_returns_none() {
        let result = lookup_group_by_name(b"\xff\xfe").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn lookup_with_converter_delegates() {
        set_name_converter(Box::new(TestConverter));

        let user = lookup_user_name(0).unwrap();
        assert_eq!(user, Some(b"root".to_vec()));

        let uid = lookup_user_by_name(b"root").unwrap();
        assert_eq!(uid, Some(0));

        let group = lookup_group_name(0).unwrap();
        assert_eq!(group, Some(b"wheel".to_vec()));

        let gid = lookup_group_by_name(b"wheel").unwrap();
        assert_eq!(gid, Some(0));

        clear_name_converter();
    }

    #[test]
    fn lookup_with_converter_unknown_returns_none() {
        set_name_converter(Box::new(TestConverter));

        let user = lookup_user_name(9999).unwrap();
        assert!(user.is_none());

        let uid = lookup_user_by_name(b"nobody").unwrap();
        assert!(uid.is_none());

        let group = lookup_group_name(9999).unwrap();
        assert!(group.is_none());

        let gid = lookup_group_by_name(b"nobody").unwrap();
        assert!(gid.is_none());

        clear_name_converter();
    }
}
