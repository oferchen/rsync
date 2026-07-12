use std::ffi::OsStr;
use std::num::NonZeroU8;

use compress::algorithm::{CompressionAlgorithm, CompressionAlgorithmParseError};
use compress::zlib::CompressionLevel;
use core::{
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
}

impl CompressionLevelParseError {
    fn into_flag_message(self) -> Message {
        match self {
            Self::Empty { original } | Self::Invalid { original, .. } => {
                rsync_error!(1, "--compress-level={} is invalid", original).with_role(Role::Client)
            }
        }
    }

    fn into_argument_message(self) -> Message {
        match self {
            Self::Empty { .. } => {
                rsync_error!(1, "compression level value must not be empty").with_role(Role::Client)
            }
            Self::Invalid { trimmed, .. } => rsync_error!(
                1,
                format!(
                    "invalid compression level '{trimmed}': compression level must be an integer"
                )
            )
            .with_role(Role::Client),
        }
    }
}

/// Parses the raw numeric `--compress-level` value.
///
/// Only genuinely malformed input (empty or non-numeric) is rejected, matching
/// upstream: `--compress-level` is a `POPT_ARG_INT`, so any integer is accepted
/// and only later clamped into the codec's range by
/// `token.c:init_compression_level()`.
fn parse_raw_level(argument: &OsStr) -> Result<i32, CompressionLevelParseError> {
    let original = argument.to_string_lossy().into_owned();
    let trimmed = original.trim().to_owned();

    if trimmed.is_empty() {
        return Err(CompressionLevelParseError::Empty { original });
    }

    trimmed
        .parse::<i32>()
        .map_err(|_| CompressionLevelParseError::Invalid { original, trimmed })
}

/// Clamps a raw level into `codec`'s range, mirroring
/// `token.c:init_compression_level()`. Never rejects an out-of-range value.
fn clamp_level(codec: CompressionAlgorithm, raw: i32) -> CompressLevelArg {
    match codec.clamp_level(raw) {
        Some(level) => CompressLevelArg::Level(level),
        None => CompressLevelArg::Disable,
    }
}

pub(crate) fn parse_compress_level(
    argument: &OsStr,
    codec: CompressionAlgorithm,
) -> Result<CompressLevelArg, Message> {
    parse_raw_level(argument)
        .map(|raw| clamp_level(codec, raw))
        .map_err(CompressionLevelParseError::into_flag_message)
}

pub(crate) fn parse_compress_choice(
    argument: &OsStr,
) -> Result<Option<CompressionAlgorithm>, Message> {
    let original = argument.to_string_lossy().into_owned();
    let trimmed = original.trim();

    if trimmed.is_empty() {
        // upstream: compat.c:190 - an unparsable compress name returns
        // RERR_UNSUPPORTED (errcode.h:28), not RERR_SYNTAX.
        return Err(
            rsync_error!(4, "--compress-choice={} is invalid", original).with_role(Role::Client)
        );
    }

    if trimmed.eq_ignore_ascii_case("none") {
        return Ok(None);
    }

    match trimmed.parse::<CompressionAlgorithm>() {
        Ok(algorithm) => Ok(Some(algorithm)),
        Err(err) => Err(render_compress_choice_error(err, trimmed)),
    }
}

fn render_compress_choice_error(err: CompressionAlgorithmParseError, trimmed: &str) -> Message {
    let display = if trimmed.is_empty() {
        err.input()
    } else {
        trimmed
    };
    #[allow(unused_mut)] // REASON: mutated when lz4 or zstd features are enabled
    let mut supported = vec!["zlib", "zlibx"];
    #[cfg(feature = "lz4")]
    {
        supported.push("lz4");
    }
    #[cfg(feature = "zstd")]
    {
        supported.push("zstd");
    }
    let supported_list = supported.join(", ");
    let rendered = format!(
        "invalid compression algorithm '{display}': supported values include {supported_list}"
    );
    // upstream: compat.c:190 - an unknown compress name returns
    // RERR_UNSUPPORTED (errcode.h:28), not RERR_SYNTAX.
    rsync_error!(4, rendered).with_role(Role::Client)
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

/// Upper bound for `--compress-threads`. Mirrors zstd's documented worker cap
/// and matches what upstream rsync 3.4.2 accepts before clamping. Upstream
/// silently clamps negative values to 0; we reject them so users get a clear
/// diagnostic instead.
const COMPRESS_THREADS_MAX: i32 = 64;

/// Parses `--compress-threads=N` into an optional worker count.
///
/// Returns `Ok(None)` for `0` (delegates the choice to zstd, matching
/// `do_compression_threads = 0` in upstream `options.c:89`). Returns
/// `Ok(Some(n))` for positive integers up to [`COMPRESS_THREADS_MAX`].
/// Rejects negative values, non-numeric input, and out-of-range positive
/// integers with a user-facing error message.
///
/// # Upstream Reference
///
/// - `options.c:760-761` - `{"compress-threads", 0, POPT_ARG_INT, &do_compression_threads, 0, 0, 0 }`.
/// - `options.c:2016-2017` - upstream clamps negative values to 0.
pub(crate) fn parse_compress_threads(argument: &OsStr) -> Result<Option<NonZeroU8>, Message> {
    let original = argument.to_string_lossy().into_owned();
    let trimmed = original.trim();

    if trimmed.is_empty() {
        return Err(
            rsync_error!(1, "--compress-threads={} is invalid", original).with_role(Role::Client),
        );
    }

    match trimmed.parse::<i32>() {
        Ok(0) => Ok(None),
        Ok(value @ 1..=COMPRESS_THREADS_MAX) => {
            let byte = u8::try_from(value).expect("range 1..=64 fits in u8");
            Ok(Some(
                NonZeroU8::new(byte).expect("range guarantees non-zero"),
            ))
        }
        Ok(_) => Err(rsync_error!(
            1,
            format!(
                "--compress-threads={} must be between 0 and {}",
                trimmed, COMPRESS_THREADS_MAX
            )
        )
        .with_role(Role::Client)),
        Err(_) => Err(
            rsync_error!(1, "--compress-threads={} is invalid", original).with_role(Role::Client),
        ),
    }
}

pub(crate) fn parse_compress_level_argument(
    value: &OsStr,
    codec: CompressionAlgorithm,
) -> Result<CompressionSetting, Message> {
    match parse_raw_level(value).map(|raw| clamp_level(codec, raw)) {
        Ok(CompressLevelArg::Disable) => Ok(CompressionSetting::disabled()),
        Ok(CompressLevelArg::Level(level)) => {
            Ok(CompressionSetting::level(CompressionLevel::precise(level)))
        }
        Err(error) => Err(error.into_argument_message()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn parse_compress_choice_none_disables_compression() {
        let parsed = parse_compress_choice(OsStr::new("none"));
        assert!(matches!(parsed, Ok(None)));
    }

    #[test]
    fn parse_compress_choice_accepts_zlib_aliases() {
        let parsed = parse_compress_choice(OsStr::new("zlib"));
        assert!(matches!(parsed, Ok(Some(CompressionAlgorithm::Zlib))));

        let alias = parse_compress_choice(OsStr::new(" zlibx "));
        assert!(matches!(alias, Ok(Some(CompressionAlgorithm::Zlib))));
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn parse_compress_choice_accepts_zstd() {
        let parsed = parse_compress_choice(OsStr::new("zstd"));
        assert!(matches!(parsed, Ok(Some(CompressionAlgorithm::Zstd))));
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn parse_compress_choice_accepts_lz4() {
        let parsed = parse_compress_choice(OsStr::new("lz4"));
        assert!(matches!(parsed, Ok(Some(CompressionAlgorithm::Lz4))));
    }

    #[test]
    fn parse_compress_choice_rejects_unknown_algorithm() {
        let error =
            parse_compress_choice(OsStr::new("brotli")).expect_err("brotli should be rejected");
        let message = error.to_string();
        assert!(message.contains("invalid compression algorithm"));
        assert!(message.contains("brotli"));
    }

    #[test]
    fn parse_compress_threads_accepts_positive_value() {
        let parsed = parse_compress_threads(OsStr::new("4")).expect("4 should parse");
        assert_eq!(parsed.map(NonZeroU8::get), Some(4));
    }

    #[test]
    fn parse_compress_threads_zero_means_zstd_default() {
        let parsed = parse_compress_threads(OsStr::new("0")).expect("0 should parse");
        assert!(parsed.is_none());
    }

    #[test]
    fn parse_compress_threads_rejects_negative() {
        let error =
            parse_compress_threads(OsStr::new("-1")).expect_err("negative should be rejected");
        let rendered = error.to_string();
        assert!(rendered.contains("--compress-threads=-1 must be between 0 and 64"));
    }

    #[test]
    fn parse_compress_threads_rejects_non_numeric() {
        let error =
            parse_compress_threads(OsStr::new("abc")).expect_err("non-numeric should be rejected");
        let rendered = error.to_string();
        assert!(rendered.contains("--compress-threads=abc is invalid"));
    }

    #[test]
    fn parse_compress_threads_rejects_above_cap() {
        let error = parse_compress_threads(OsStr::new("99")).expect_err("99 should exceed the cap");
        let rendered = error.to_string();
        assert!(rendered.contains("must be between 0 and 64"));
    }
}
