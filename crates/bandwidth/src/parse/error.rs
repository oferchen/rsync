use std::error::Error;
use std::fmt;

/// Errors returned when parsing a bandwidth limit fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BandwidthParseError {
    /// The argument did not follow rsync's recognised syntax.
    Invalid,
    /// The requested rate was too small (less than 512 bytes per second).
    TooSmall,
    /// The requested rate overflowed the supported range.
    TooLarge,
}

impl fmt::Display for BandwidthParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = match self {
            BandwidthParseError::Invalid => "invalid bandwidth limit syntax",
            BandwidthParseError::TooSmall => {
                "bandwidth limit is below the minimum of 512 bytes per second"
            }
            BandwidthParseError::TooLarge => "bandwidth limit exceeds the supported range",
        };

        f.write_str(description)
    }
}

impl Error for BandwidthParseError {}
