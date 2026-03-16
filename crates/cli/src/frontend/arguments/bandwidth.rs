use std::ffi::OsString;

/// Parsed state of the `--bwlimit` / `--no-bwlimit` CLI argument.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BandwidthArgument {
    /// A bandwidth cap was specified (raw value to be parsed later).
    Limit(OsString),
    /// Bandwidth limiting was explicitly disabled via `--no-bwlimit`.
    Disabled,
}
