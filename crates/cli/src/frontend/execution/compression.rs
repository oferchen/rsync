use std::ffi::OsStr;
use std::num::NonZeroU8;

use rsync_compress::zlib::CompressionLevel;
use rsync_core::{
    bandwidth::BandwidthParseError,
    client::{BandwidthLimit, CompressionSetting},
    message::{Message, Role},
    rsync_error,
};

pub(crate) enum CompressLevelArg {
    Disable,
    Level(NonZeroU8),
}

enum CompressionLevelParseError {
    Empty { original: String },
    Invalid { original: String, trimmed: String },
    OutOfRange { trimmed: String, value: i32 },
}

impl CompressionLevelParseError {
    fn into_flag_message(self) -> Message {
        match self {
            Self::Empty { original } | Self::Invalid { original, .. } => {
                rsync_error!(1, "--compress-level={} is invalid", original).with_role(Role::Client)
            }
            Self::OutOfRange { trimmed, .. } => {
                rsync_error!(1, "--compress-level={} must be between 0 and 9", trimmed)
                    .with_role(Role::Client)
            }
        }
    }

    fn into_argument_message(self) -> Message {
        match self {
            Self::Empty { .. } => rsync_error!(
                1,
                "compression level value must not be empty"
            )
            .with_role(Role::Client),
            Self::Invalid { trimmed, .. } => rsync_error!(
                1,
                format!(
                    "invalid compression level '{trimmed}': compression level must be an integer between 0 and 9"
                )
            )
            .with_role(Role::Client),
            Self::OutOfRange { trimmed, value } => rsync_error!(
                1,
                format!(
                    "invalid compression level '{trimmed}': compression level {value} is outside the supported range 0-9"
                )
            )
            .with_role(Role::Client),
        }
    }
}

fn parse_compress_level_value(
    argument: &OsStr,
) -> Result<CompressLevelArg, CompressionLevelParseError> {
    let original = argument.to_string_lossy().into_owned();
    let trimmed_owned = original.trim().to_owned();

    if trimmed_owned.is_empty() {
        return Err(CompressionLevelParseError::Empty { original });
    }

    match trimmed_owned.parse::<i32>() {
        Ok(0) => Ok(CompressLevelArg::Disable),
        Ok(value @ 1..=9) => Ok(CompressLevelArg::Level(
            NonZeroU8::new(value as u8).expect("range guarantees non-zero"),
        )),
        Ok(value) => Err(CompressionLevelParseError::OutOfRange {
            trimmed: trimmed_owned,
            value,
        }),
        Err(_) => Err(CompressionLevelParseError::Invalid {
            original,
            trimmed: trimmed_owned,
        }),
    }
}

pub(crate) fn parse_compress_level(argument: &OsStr) -> Result<CompressLevelArg, Message> {
    parse_compress_level_value(argument).map_err(CompressionLevelParseError::into_flag_message)
}

pub(crate) fn parse_bandwidth_limit(argument: &OsStr) -> Result<Option<BandwidthLimit>, Message> {
    let text = argument.to_string_lossy();
    match BandwidthLimit::parse(&text) {
        Ok(Some(limit)) => Ok(Some(limit)),
        Ok(None) => Ok(None),
        Err(BandwidthParseError::Invalid) => {
            Err(rsync_error!(1, "--bwlimit={} is invalid", text).with_role(Role::Client))
        }
        Err(BandwidthParseError::TooSmall) => Err(rsync_error!(
            1,
            "--bwlimit={} is too small (min: 512 or 0 for unlimited)",
            text
        )
        .with_role(Role::Client)),
        Err(BandwidthParseError::TooLarge) => {
            Err(rsync_error!(1, "--bwlimit={} is too large", text).with_role(Role::Client))
        }
    }
}

pub(crate) fn parse_compress_level_argument(value: &OsStr) -> Result<CompressionSetting, Message> {
    match parse_compress_level_value(value) {
        Ok(CompressLevelArg::Disable) => Ok(CompressionSetting::disabled()),
        Ok(CompressLevelArg::Level(level)) => {
            Ok(CompressionSetting::level(CompressionLevel::precise(level)))
        }
        Err(error) => Err(error.into_argument_message()),
    }
}
