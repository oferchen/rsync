use std::ffi::OsStr;
use std::num::NonZeroU8;

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

pub(crate) fn parse_compress_level(argument: &OsStr) -> Result<CompressLevelArg, Message> {
    let text = argument.to_string_lossy();
    let trimmed = text.trim();

    if trimmed.is_empty() {
        return Err(rsync_error!(1, "--compress-level={} is invalid", text).with_role(Role::Client));
    }

    match trimmed.parse::<i32>() {
        Ok(0) => Ok(CompressLevelArg::Disable),
        Ok(value @ 1..=9) => Ok(CompressLevelArg::Level(
            NonZeroU8::new(value as u8).expect("range guarantees non-zero"),
        )),
        Ok(_) => Err(
            rsync_error!(1, "--compress-level={} must be between 0 and 9", trimmed)
                .with_role(Role::Client),
        ),
        Err(_) => {
            Err(rsync_error!(1, "--compress-level={} is invalid", text).with_role(Role::Client))
        }
    }
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
    let text = value.to_string_lossy();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(
            rsync_error!(1, "compression level value must not be empty").with_role(Role::Client)
        );
    }

    if !trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(
            rsync_error!(
                1,
                format!(
                    "invalid compression level '{trimmed}': compression level must be an integer between 0 and 9"
                )
            )
            .with_role(Role::Client),
        );
    }

    let level: u32 = trimmed.parse().map_err(|_| {
        rsync_error!(
            1,
            format!(
                "invalid compression level '{trimmed}': compression level must be an integer between 0 and 9"
            )
        )
        .with_role(Role::Client)
    })?;

    CompressionSetting::try_from_numeric(level).map_err(|error| {
        rsync_error!(
            1,
            format!(
                "invalid compression level '{trimmed}': compression level {} is outside the supported range 0-9",
                error.level()
            )
        )
        .with_role(Role::Client)
    })
}
