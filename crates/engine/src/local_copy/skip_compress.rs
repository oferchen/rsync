use std::path::Path;

use thiserror::Error;

/// Default suffixes that should not be compressed when compression is enabled.
const DEFAULT_SKIP_SUFFIXES: &[&str] = &[
    "3g2", "3gp", "7z", "aac", "ace", "apk", "avi", "bz2", "deb", "dmg", "ear", "f4v", "flac",
    "flv", "gpg", "gz", "iso", "jar", "jpeg", "jpg", "lrz", "lz", "lz4", "lzma", "lzo", "m1a",
    "m1v", "m2a", "m2ts", "m2v", "m4a", "m4b", "m4p", "m4r", "m4v", "mka", "mkv", "mov", "mp1",
    "mp2", "mp3", "mp4", "mpa", "mpeg", "mpg", "mpv", "mts", "odb", "odf", "odg", "odi", "odm",
    "odp", "ods", "odt", "oga", "ogg", "ogm", "ogv", "ogx", "opus", "otg", "oth", "otp", "ots",
    "ott", "oxt", "png", "qt", "rar", "rpm", "rz", "rzip", "spx", "squashfs", "sxc", "sxd", "sxg",
    "sxm", "sxw", "sz", "tbz", "tbz2", "tgz", "tlz", "ts", "txz", "tzo", "vob", "war", "webm",
    "webp", "xz", "z", "zip", "zst",
];

/// Errors that can occur when parsing a `--skip-compress` specification.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum SkipCompressParseError {
    /// The specification contained a character that is not supported.
    #[error("invalid character '{0}' in --skip-compress specification")]
    InvalidCharacter(char),
    /// A character class (`[...]`) was not terminated.
    #[error("unterminated character class in --skip-compress specification")]
    UnterminatedClass,
    /// A character class was declared without any characters.
    #[error("empty character class in --skip-compress specification")]
    EmptyClass,
}

/// A parsed list of suffix patterns that should bypass compression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkipCompressList {
    patterns: Vec<SkipCompressPattern>,
}

impl SkipCompressList {
    /// Parses a user-supplied `--skip-compress` specification.
    ///
    /// Upstream rsync treats both `/` and `,` as separators between suffix
    /// patterns, so the implementation mirrors that behaviour to remain
    /// interoperable with existing configurations.
    pub fn parse(spec: &str) -> Result<Self, SkipCompressParseError> {
        if spec.is_empty() {
            return Ok(Self {
                patterns: Vec::new(),
            });
        }

        let mut patterns = Vec::new();
        for part in spec.split(['/', ',']) {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                continue;
            }
            patterns.push(SkipCompressPattern::parse(trimmed)?);
        }

        Ok(Self { patterns })
    }

    /// Returns `true` when the provided path's extension matches a skipped suffix.
    pub fn matches_path(&self, path: &Path) -> bool {
        if self.patterns.is_empty() {
            return false;
        }

        let extension = match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) if !ext.is_empty() => ext.to_ascii_lowercase(),
            _ => return false,
        };

        self.patterns
            .iter()
            .any(|pattern| pattern.matches(&extension))
    }

    /// Returns `true` if no suffixes are configured.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
}

impl Default for SkipCompressList {
    fn default() -> Self {
        let patterns = DEFAULT_SKIP_SUFFIXES
            .iter()
            .filter_map(|suffix| SkipCompressPattern::parse(suffix).ok())
            .collect();
        Self { patterns }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SkipCompressPattern {
    tokens: Vec<SkipCompressToken>,
}

impl SkipCompressPattern {
    fn parse(text: &str) -> Result<Self, SkipCompressParseError> {
        let mut tokens = Vec::new();
        let mut chars = text.chars();

        while let Some(ch) = chars.next() {
            match ch {
                '.' if tokens.is_empty() => {
                    // Ignore optional leading dots so patterns such as ".gz" match
                    // extensions without requiring the dot to be present. Upstream
                    // rsync treats the suffixes as dot-less regardless of the
                    // caller's notation, making the parsing tolerant to the
                    // commonly documented form.
                    continue;
                }
                '[' => {
                    let mut class = Vec::new();
                    let mut terminated = false;
                    for item in chars.by_ref() {
                        if item == ']' {
                            terminated = true;
                            break;
                        }
                        if !item.is_ascii() {
                            return Err(SkipCompressParseError::InvalidCharacter(item));
                        }
                        class.push(item.to_ascii_lowercase() as u8);
                    }
                    if !terminated {
                        return Err(SkipCompressParseError::UnterminatedClass);
                    }
                    if class.is_empty() {
                        return Err(SkipCompressParseError::EmptyClass);
                    }
                    class.sort_unstable();
                    class.dedup();
                    tokens.push(SkipCompressToken::Class(class));
                }
                ']' => return Err(SkipCompressParseError::InvalidCharacter(']')),
                _ => {
                    if !ch.is_ascii() {
                        return Err(SkipCompressParseError::InvalidCharacter(ch));
                    }
                    tokens.push(SkipCompressToken::Literal(ch.to_ascii_lowercase() as u8));
                }
            }
        }

        if tokens.is_empty() {
            // The caller filtered empty segments, so reaching here indicates a
            // pattern comprised solely of whitespace. Treat this as invalid.
            return Err(SkipCompressParseError::InvalidCharacter(' '));
        }

        Ok(Self { tokens })
    }

    fn matches(&self, extension: &str) -> bool {
        let bytes = extension.as_bytes();
        if bytes.len() != self.tokens.len() {
            return false;
        }

        for (token, byte) in self.tokens.iter().zip(bytes.iter()) {
            let normalized = byte.to_ascii_lowercase();
            match token {
                SkipCompressToken::Literal(expected) => {
                    if normalized != *expected {
                        return false;
                    }
                }
                SkipCompressToken::Class(options) => {
                    if !options.contains(&normalized) {
                        return false;
                    }
                }
            }
        }

        true
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SkipCompressToken {
    Literal(u8),
    Class(Vec<u8>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn default_list_matches_known_suffix() {
        let list = SkipCompressList::default();
        assert!(list.matches_path(Path::new("archive.tar.gz")));
        assert!(list.matches_path(Path::new("movie.MP4")));
        assert!(!list.matches_path(Path::new("notes.txt")));
    }

    #[test]
    fn parse_accepts_character_classes() {
        let list = SkipCompressList::parse("mp[34]/zst").expect("parse succeeds");
        assert!(list.matches_path(Path::new("track.mp3")));
        assert!(list.matches_path(Path::new("track.mp4")));
        assert!(!list.matches_path(Path::new("track.mp5")));
        assert!(list.matches_path(Path::new("archive.zst")));
    }

    #[test]
    fn parse_accepts_comma_separator() {
        let list = SkipCompressList::parse("gz, xz ,zip").expect("parse succeeds");
        assert!(list.matches_path(Path::new("archive.gz")));
        assert!(list.matches_path(Path::new("archive.xz")));
        assert!(list.matches_path(Path::new("bundle.ZIP")));
        assert!(!list.matches_path(Path::new("notes.txt")));
    }

    #[test]
    fn parse_rejects_unterminated_class() {
        let error = SkipCompressList::parse("mp[3").expect_err("parse should fail");
        assert_eq!(error, SkipCompressParseError::UnterminatedClass);
    }

    #[test]
    fn parse_rejects_empty_class() {
        let error = SkipCompressList::parse("mp[]").expect_err("parse should fail");
        assert_eq!(error, SkipCompressParseError::EmptyClass);
    }

    #[test]
    fn parse_ignores_empty_segments() {
        let list = SkipCompressList::parse("//gz//").expect("parse succeeds");
        assert!(list.matches_path(Path::new("file.gz")));
    }

    #[test]
    fn parse_accepts_leading_dots() {
        let list = SkipCompressList::parse(".gz/.mp[34]").expect("parse succeeds");
        assert!(list.matches_path(Path::new("archive.GZ")));
        assert!(list.matches_path(Path::new("track.mp3")));
        assert!(list.matches_path(Path::new("track.mp4")));
        assert!(!list.matches_path(Path::new("track.mp5")));
    }

    #[test]
    fn matches_handles_non_ascii_extensions() {
        let list = SkipCompressList::parse("gz").expect("parse succeeds");
        let path = PathBuf::from("archivo.g\u{00FA}z");
        assert!(!list.matches_path(&path));
    }
}
