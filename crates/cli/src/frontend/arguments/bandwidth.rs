use std::ffi::OsString;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BandwidthArgument {
    Limit(OsString),
    Disabled,
}
