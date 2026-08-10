/// Number of optional feature variants (must match `CompiledFeature::ALL.len()`).
pub(super) const COMPILED_FEATURE_COUNT: usize = 5;

pub(super) const ACL_FEATURE_BIT: u8 = 1 << 0;
pub(super) const XATTR_FEATURE_BIT: u8 = 1 << 1;
pub(super) const ZSTD_FEATURE_BIT: u8 = 1 << 2;
pub(super) const ICONV_FEATURE_BIT: u8 = 1 << 3;
pub(super) const SD_NOTIFY_FEATURE_BIT: u8 = 1 << 4;

/// Bitmap describing the optional features compiled into this build.
#[doc(alias = "--version")]
pub const COMPILED_FEATURE_BITMAP: u8 = {
    let mut bitmap = 0u8;

    if cfg!(feature = "acl") {
        bitmap |= ACL_FEATURE_BIT;
    }

    if cfg!(feature = "xattr") {
        bitmap |= XATTR_FEATURE_BIT;
    }

    if cfg!(feature = "zstd") {
        bitmap |= ZSTD_FEATURE_BIT;
    }

    if cfg!(feature = "iconv") {
        bitmap |= ICONV_FEATURE_BIT;
    }

    if cfg!(feature = "sd-notify") {
        bitmap |= SD_NOTIFY_FEATURE_BIT;
    }

    bitmap
};
