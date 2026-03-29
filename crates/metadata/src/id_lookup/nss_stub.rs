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
