//! Directory scan and best-match selection for [`FuzzyMatcher`].
//!
//! Faithful port of upstream `generator.c:find_fuzzy()`: a size/modtime
//! fast-path pass followed by a lowest-distance pass over every candidate in
//! the destination directory (and, at level 2, the reference directories).
//!
//! upstream: generator.c:831 `find_fuzzy()`.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::distance::{find_filename_suffix, fuzzy_name_distance};
use super::trace::{trace_fuzzy_distance, trace_fuzzy_size_mtime_match};
use super::{FUZZY_LEVEL_2, FuzzyMatch, FuzzyMatcher};

/// A destination-directory file eligible to be a fuzzy basis.
struct Candidate {
    /// Basename (lossy) used for distance scoring and tracing.
    name: String,
    /// Absolute path to the candidate file.
    path: PathBuf,
    /// File length in bytes.
    size: u64,
    /// Modification time in whole seconds since the Unix epoch, if available.
    mtime_secs: Option<i64>,
}

impl FuzzyMatcher {
    /// Finds the best fuzzy basis for `target_name` in `dest_dir`.
    ///
    /// Mirrors upstream `find_fuzzy()`: an exact size + mtime match (when
    /// `target_mtime` is known) returns immediately; otherwise the candidate
    /// with the lowest [`fuzzy_name_distance`] within the matcher's distance
    /// cap wins. At level 2 the configured `fuzzy_basis_dirs` are scanned after
    /// the destination directory, sharing a single running lowest distance so
    /// ordering and tie-breaks match upstream's single-pass loop.
    ///
    /// `target_mtime` is the source file's modification time in whole seconds
    /// since the Unix epoch; `None` disables the fast-path.
    ///
    /// upstream: generator.c:831 `find_fuzzy()`.
    pub fn find_fuzzy_basis(
        &self,
        target_name: &OsStr,
        dest_dir: &Path,
        target_size: u64,
        target_mtime: Option<i64>,
    ) -> Option<FuzzyMatch> {
        let target = target_name.to_string_lossy();
        let target_bytes = target.as_bytes();
        let target_suffix = find_filename_suffix(target_bytes).to_vec();

        // upstream: generator.c:843/868 - iterate dirlist_array[0..fuzzy_basis],
        // the destination directory first then each reference directory.
        let mut dirs: Vec<&Path> = Vec::with_capacity(1 + self.fuzzy_basis_dirs.len());
        dirs.push(dest_dir);
        if self.fuzzy_level >= FUZZY_LEVEL_2 {
            dirs.extend(self.fuzzy_basis_dirs.iter().map(PathBuf::as_path));
        }

        let per_dir: Vec<Vec<Candidate>> = dirs
            .iter()
            .map(|dir| collect_candidates(dir, &target))
            .collect();

        // Pass 1: exact size + mtime fast-path. upstream: generator.c:842-866.
        if let Some(target_secs) = target_mtime {
            for candidates in &per_dir {
                for candidate in candidates {
                    if candidate.size == target_size && candidate.mtime_secs == Some(target_secs) {
                        trace_fuzzy_size_mtime_match(&candidate.name);
                        return Some(FuzzyMatch {
                            path: candidate.path.clone(),
                            distance: 0,
                        });
                    }
                }
            }
        }

        // Pass 2: lowest-distance wins. upstream: generator.c:868-908.
        let mut lowest_dist = self.max_distance;
        let mut best: Option<FuzzyMatch> = None;
        for candidates in &per_dir {
            for candidate in candidates {
                let dist = fuzzy_name_distance(
                    candidate.name.as_bytes(),
                    target_bytes,
                    &target_suffix,
                    lowest_dist,
                );
                // upstream: generator.c:896-899 - emit each candidate's distance
                // as fixed-point `%d.%05d` for --debug=FUZZY parsers.
                trace_fuzzy_distance(&candidate.name, dist);
                // upstream: generator.c:900 - `<=` lets a later equal-distance
                // candidate win; the sorted scan order makes this deterministic.
                if dist <= lowest_dist {
                    lowest_dist = dist;
                    best = Some(FuzzyMatch {
                        path: candidate.path.clone(),
                        distance: dist,
                    });
                }
            }
        }

        best
    }
}

/// Collects the eligible fuzzy candidates in `dir`, sorted by basename to
/// mirror upstream's sorted dirlist ordering (flist.c:3451
/// `flist_sort_and_clean`).
///
/// Skips non-regular files, zero-length files, and the exact-name file (which,
/// when present, is used as the direct basis before fuzzy matching runs).
///
/// upstream: generator.c:852-856,883 - `F_IS_ACTIVE`, `S_ISREG`, and
/// `!F_LENGTH(fp)` screening.
fn collect_candidates(dir: &Path, target_name: &str) -> Vec<Candidate> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let metadata = match entry.metadata() {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };

        // upstream: generator.c:855 - skip zero-length files.
        if metadata.len() == 0 {
            continue;
        }

        let path = entry.path();
        let name = match path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => continue,
        };

        // The exact-name file is used as the direct basis (FNAMECMP_FNAME)
        // before fuzzy matching runs, never as a fuzzy candidate.
        if name == target_name {
            continue;
        }

        out.push(Candidate {
            name,
            path,
            size: metadata.len(),
            mtime_secs: metadata.modified().ok().map(system_time_to_secs),
        });
    }

    out.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    out
}

/// Converts a [`SystemTime`] to whole seconds since the Unix epoch, matching
/// the second-granularity comparison in upstream `same_time(..., 0, ..., 0)`.
fn system_time_to_secs(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(delta) => delta.as_secs() as i64,
        Err(err) => -(err.duration().as_secs() as i64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuzzy::{FUZZY_LEVEL_1, MAX_FUZZY_DISTANCE};
    use std::path::PathBuf;

    mod fuzzy_matcher_tests {
        use super::*;

        #[test]
        fn new_default_values() {
            let matcher = FuzzyMatcher::new();
            assert_eq!(matcher.fuzzy_level(), FUZZY_LEVEL_1);
            assert_eq!(matcher.max_distance(), MAX_FUZZY_DISTANCE);
            assert!(matcher.fuzzy_basis_dirs.is_empty());
        }

        #[test]
        fn with_level() {
            let matcher = FuzzyMatcher::with_level(2);
            assert_eq!(matcher.fuzzy_level(), 2);
            assert_eq!(matcher.max_distance(), MAX_FUZZY_DISTANCE);
        }

        #[test]
        fn default_trait() {
            // Derived Default leaves fields at 0; FuzzyMatcher::new() is the
            // supported way to obtain a usable level-1 matcher.
            let matcher = FuzzyMatcher::default();
            assert_eq!(matcher.fuzzy_level(), 0);
            assert_eq!(matcher.max_distance(), 0);
        }

        #[test]
        fn with_max_distance() {
            let matcher = FuzzyMatcher::new().with_max_distance(100);
            assert_eq!(matcher.max_distance(), 100);
        }

        #[test]
        fn with_fuzzy_basis_dirs() {
            let dirs = vec![PathBuf::from("/tmp/basis1"), PathBuf::from("/tmp/basis2")];
            let matcher = FuzzyMatcher::new().with_fuzzy_basis_dirs(dirs.clone());
            assert_eq!(matcher.fuzzy_basis_dirs, dirs);
        }

        #[test]
        fn builder_chaining() {
            let dirs = vec![PathBuf::from("/tmp/basis")];
            let matcher = FuzzyMatcher::with_level(2)
                .with_max_distance(50)
                .with_fuzzy_basis_dirs(dirs.clone());
            assert_eq!(matcher.fuzzy_level(), 2);
            assert_eq!(matcher.max_distance(), 50);
            assert_eq!(matcher.fuzzy_basis_dirs, dirs);
        }

        #[test]
        fn debug_impl() {
            let matcher = FuzzyMatcher::new();
            let debug = format!("{matcher:?}");
            assert!(debug.contains("FuzzyMatcher"));
        }

        #[test]
        fn find_in_nonexistent_dir() {
            let matcher = FuzzyMatcher::new();
            let result = matcher.find_fuzzy_basis(
                std::ffi::OsStr::new("test.txt"),
                Path::new("/nonexistent/dir"),
                1000,
                None,
            );
            assert!(result.is_none());
        }

        #[test]
        fn level_2_skips_basis_dirs_without_config() {
            // Level 2 without configured basis dirs degenerates to level 1
            // behaviour (search the destination directory only).
            let matcher = FuzzyMatcher::with_level(2);
            assert!(matcher.fuzzy_basis_dirs.is_empty());
        }
    }

    mod fast_path_tests {
        use super::*;
        use std::io::Write;

        fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
            let path = dir.join(name);
            let mut f = fs::File::create(&path).unwrap();
            f.write_all(bytes).unwrap();
            path
        }

        /// A candidate with the exact target size and mtime must be returned by
        /// the fast-path (distance 0) ahead of any closer-named candidate.
        ///
        /// upstream: generator.c:858-863 - size/modtime match short-circuits.
        #[test]
        fn size_mtime_match_wins_over_closer_name() {
            let dir = tempfile::tempdir().unwrap();
            // Closer name but wrong size: must lose to the fast-path.
            write_file(dir.path(), "target_v1.bin", b"different length data here");
            let exact = write_file(dir.path(), "unrelated.bin", b"exactly-ten");

            // Use the file's own on-disk mtime as the target mtime so the test
            // needs no time-setting dependency.
            let exact_secs = system_time_to_secs(fs::metadata(&exact).unwrap().modified().unwrap());

            let matcher = FuzzyMatcher::new();
            let result = matcher
                .find_fuzzy_basis(
                    OsStr::new("target_v2.bin"),
                    dir.path(),
                    "exactly-ten".len() as u64,
                    Some(exact_secs),
                )
                .expect("fast-path should select the exact size/mtime file");
            assert_eq!(result.path, exact);
            assert_eq!(result.distance, 0);
        }

        /// Without a target mtime the fast-path is disabled and selection falls
        /// through to the lowest-distance pass.
        #[test]
        fn no_target_mtime_disables_fast_path() {
            let dir = tempfile::tempdir().unwrap();
            let closer = write_file(dir.path(), "report_2023.csv", b"some csv payload");
            write_file(dir.path(), "wholly-different.log", b"some csv payload");

            let matcher = FuzzyMatcher::new();
            let result = matcher
                .find_fuzzy_basis(OsStr::new("report_2024.csv"), dir.path(), 16, None)
                .expect("distance pass should find the closest name");
            assert_eq!(result.path, closer);
            assert!(result.distance > 0);
        }

        /// Zero-length candidates are screened out (upstream `!F_LENGTH(fp)`).
        #[test]
        fn zero_length_candidate_skipped() {
            let dir = tempfile::tempdir().unwrap();
            write_file(dir.path(), "report_2023.csv", b"");

            let matcher = FuzzyMatcher::new();
            let result =
                matcher.find_fuzzy_basis(OsStr::new("report_2024.csv"), dir.path(), 0, None);
            assert!(result.is_none(), "empty candidate must not be a basis");
        }
    }

    mod fuzzy_match_tests {
        use super::*;

        #[test]
        fn clone() {
            let m = FuzzyMatch {
                path: PathBuf::from("/tmp/test.txt"),
                distance: 100,
            };
            let cloned = m.clone();
            assert_eq!(cloned.path, m.path);
            assert_eq!(cloned.distance, m.distance);
        }

        #[test]
        fn debug() {
            let m = FuzzyMatch {
                path: PathBuf::from("/tmp/test.txt"),
                distance: 100,
            };
            let debug = format!("{m:?}");
            assert!(debug.contains("FuzzyMatch"));
            assert!(debug.contains("100"));
        }
    }
}
