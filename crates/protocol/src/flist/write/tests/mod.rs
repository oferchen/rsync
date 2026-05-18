pub(super) use super::super::flags::{
    XMIT_EXTENDED_FLAGS, XMIT_HLINK_FIRST, XMIT_HLINKED, XMIT_IO_ERROR_ENDLIST, XMIT_LONG_NAME,
    XMIT_SAME_MODE, XMIT_SAME_NAME, XMIT_SAME_TIME, XMIT_TOP_DIR,
};
pub(super) use super::*;

pub(super) fn test_protocol() -> ProtocolVersion {
    ProtocolVersion::try_from(32u8).unwrap()
}

mod basic;
mod compression;
mod device;
mod extended_flags;
mod filename;
mod hardlink;
mod names;
mod protocol_boundaries;
mod symlink;
mod xattr;
