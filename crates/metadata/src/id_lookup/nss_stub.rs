//! Non-Unix lookup stubs that delegate to the thread-local name converter.
//!
//! On platforms without POSIX NSS (Windows, etc.), lookups succeed only when
//! a name converter is installed via [`super::set_name_converter`]. Without
//! a converter, all lookups return `Ok(None)`.

use super::converter::NAME_CONVERTER_SLOT;
use std::io;

/// Looks up the username for a given UID.
///
/// Delegates to the thread-local name converter if installed, otherwise
/// returns `Ok(None)`.
pub fn lookup_user_name(uid: u32) -> Result<Option<Vec<u8>>, io::Error> {
    let converted = NAME_CONVERTER_SLOT.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .and_then(|nc| nc.uid_to_name(uid))
    });
    Ok(converted.map(String::into_bytes))
}

/// Looks up the UID for a given username.
///
/// Delegates to the thread-local name converter if installed, otherwise
/// returns `Ok(None)`.
pub fn lookup_user_by_name(name: &[u8]) -> Result<Option<u32>, io::Error> {
    let Ok(name_str) = std::str::from_utf8(name) else {
        return Ok(None);
    };
    let converted = NAME_CONVERTER_SLOT.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .and_then(|nc| nc.name_to_uid(name_str))
    });
    Ok(converted)
}

/// Looks up the group name for a given GID.
///
/// Delegates to the thread-local name converter if installed, otherwise
/// returns `Ok(None)`.
pub fn lookup_group_name(gid: u32) -> Result<Option<Vec<u8>>, io::Error> {
    let converted = NAME_CONVERTER_SLOT.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .and_then(|nc| nc.gid_to_name(gid))
    });
    Ok(converted.map(String::into_bytes))
}

/// Looks up the GID for a given group name.
///
/// Delegates to the thread-local name converter if installed, otherwise
/// returns `Ok(None)`.
pub fn lookup_group_by_name(name: &[u8]) -> Result<Option<u32>, io::Error> {
    let Ok(name_str) = std::str::from_utf8(name) else {
        return Ok(None);
    };
    let converted = NAME_CONVERTER_SLOT.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .and_then(|nc| nc.name_to_gid(name_str))
    });
    Ok(converted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id_lookup::converter::{
        NameConverterCallbacks, clear_name_converter, set_name_converter,
    };

    struct TestConverter;

    impl NameConverterCallbacks for TestConverter {
        fn uid_to_name(&mut self, uid: u32) -> Option<String> {
            if uid == 0 {
                Some("root".to_string())
            } else {
                None
            }
        }
        fn gid_to_name(&mut self, gid: u32) -> Option<String> {
            if gid == 0 {
                Some("wheel".to_string())
            } else {
                None
            }
        }
        fn name_to_uid(&mut self, name: &str) -> Option<u32> {
            if name == "root" { Some(0) } else { None }
        }
        fn name_to_gid(&mut self, name: &str) -> Option<u32> {
            if name == "wheel" { Some(0) } else { None }
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
