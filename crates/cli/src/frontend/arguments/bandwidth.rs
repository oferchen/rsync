use std::ffi::OsString;

/// Captures how the user requested a bandwidth limit on the command line.
///
/// Distinguishes an explicit limit value (parsed later by the bandwidth
/// crate) from `--no-bwlimit`, which disables any inherited setting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BandwidthArgument {
    /// A `--bwlimit=VALUE` argument preserving the raw user-supplied text.
    Limit(OsString),
    /// `--no-bwlimit` was supplied; any prior bandwidth limit is cleared.
    Disabled,
}
