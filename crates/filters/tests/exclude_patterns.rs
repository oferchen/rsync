//! Comprehensive tests for --exclude pattern matching.
//!
//! These tests verify the behavior of exclude patterns as they would be specified
//! via rsync's `--exclude` flag. The tests cover:
//!
//! 1. Simple filename patterns (*.txt)
//! 2. Directory patterns (dir/)
//! 3. Anchored patterns (/root/file)
//! 4. Double-star patterns (**/deep/file)
//! 5. Character classes ([abc])
//! 6. Negation patterns (! modifier)
//! 7. Multiple --exclude flags
//! 8. Case sensitivity
//!
//! Reference: rsync 3.4.1 exclude.c and rsync(1) man page INCLUDE/EXCLUDE PATTERN RULES

use filters::{FilterRule, FilterSet};
use std::path::Path;

// ============================================================================
// 1. Simple Filename Patterns (*.txt)
// ============================================================================

mod simple_filename_patterns {
    use super::*;

    /// Test: --exclude "*.txt"
    /// Excludes all files ending with .txt extension at any depth.
    #[test]
    fn exclude_by_extension() {
        let set = FilterSet::from_rules([FilterRule::exclude("*.txt")]).unwrap();

        // Should exclude .txt files at root
        assert!(!set.allows(Path::new("readme.txt"), false));
        assert!(!set.allows(Path::new("notes.txt"), false));
        assert!(!set.allows(Path::new("a.txt"), false));

        // Should exclude .txt files at any depth
        assert!(!set.allows(Path::new("dir/file.txt"), false));
        assert!(!set.allows(Path::new("a/b/c/deep.txt"), false));
        assert!(!set.allows(Path::new("docs/readme.txt"), false));

        // Should allow non-.txt files
        assert!(set.allows(Path::new("readme.md"), false));
        assert!(set.allows(Path::new("file.text"), false));
        assert!(set.allows(Path::new("file.txt.bak"), false));
    }

    /// Test: --exclude "*.log"
    /// Another common extension-based exclusion.
    #[test]
    fn exclude_log_files() {
        let set = FilterSet::from_rules([FilterRule::exclude("*.log")]).unwrap();

        assert!(!set.allows(Path::new("app.log"), false));
        assert!(!set.allows(Path::new("error.log"), false));
        assert!(!set.allows(Path::new("logs/debug.log"), false));

        assert!(set.allows(Path::new("app.txt"), false));
        assert!(set.allows(Path::new("logger.rs"), false));
    }

    /// Test: --exclude "*.o"
    /// Exclude object files.
    #[test]
    fn exclude_object_files() {
        let set = FilterSet::from_rules([FilterRule::exclude("*.o")]).unwrap();

        assert!(!set.allows(Path::new("main.o"), false));
        assert!(!set.allows(Path::new("build/lib.o"), false));

        assert!(set.allows(Path::new("main.c"), false));
        assert!(set.allows(Path::new("video.mov"), false)); // .o is not .mov
    }

    /// Test: --exclude "file*"
    /// Exclude files starting with "file".
    #[test]
    fn exclude_by_prefix() {
        let set = FilterSet::from_rules([FilterRule::exclude("file*")]).unwrap();

        assert!(!set.allows(Path::new("file"), false));
        assert!(!set.allows(Path::new("file.txt"), false));
        assert!(!set.allows(Path::new("filename"), false));
        assert!(!set.allows(Path::new("file123"), false));
        assert!(!set.allows(Path::new("file.tar.gz"), false));

        assert!(set.allows(Path::new("myfile"), false));
        assert!(set.allows(Path::new("afile.txt"), false));
    }

    /// Test: --exclude "*~"
    /// Exclude editor backup files (common pattern).
    #[test]
    fn exclude_backup_files() {
        let set = FilterSet::from_rules([FilterRule::exclude("*~")]).unwrap();

        assert!(!set.allows(Path::new("file~"), false));
        assert!(!set.allows(Path::new("document.txt~"), false));
        assert!(!set.allows(Path::new("~"), false));

        assert!(set.allows(Path::new("file.txt"), false));
        assert!(set.allows(Path::new("~file"), false)); // Starts with ~, not ends
    }

    /// Test: --exclude "*.bak"
    /// Exclude backup files with .bak extension.
    #[test]
    fn exclude_bak_extension() {
        let set = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();

        assert!(!set.allows(Path::new("file.bak"), false));
        assert!(!set.allows(Path::new("config.ini.bak"), false));
        assert!(!set.allows(Path::new("subdir/data.bak"), false));

        assert!(set.allows(Path::new("backup/data.txt"), false));
    }

    /// Test: --exclude ".*"
    /// Exclude hidden files (dot-prefixed).
    #[test]
    fn exclude_hidden_files() {
        let set = FilterSet::from_rules([FilterRule::exclude(".*")]).unwrap();

        assert!(!set.allows(Path::new(".gitignore"), false));
        assert!(!set.allows(Path::new(".env"), false));
        assert!(!set.allows(Path::new(".hidden"), false));
        assert!(!set.allows(Path::new(".DS_Store"), false));

        assert!(set.allows(Path::new("visible.txt"), false));
        assert!(set.allows(Path::new("not.hidden"), false));
    }

    /// Test: --exclude "core"
    /// Exclude exact filename.
    #[test]
    fn exclude_exact_filename() {
        let set = FilterSet::from_rules([FilterRule::exclude("core")]).unwrap();

        assert!(!set.allows(Path::new("core"), false));
        assert!(!set.allows(Path::new("dir/core"), false));
        assert!(!set.allows(Path::new("a/b/c/core"), false));

        assert!(set.allows(Path::new("core.txt"), false));
        assert!(set.allows(Path::new("coredump"), false));
        assert!(set.allows(Path::new("mycore"), false));
    }

    /// Test: --exclude "Thumbs.db"
    /// Exclude Windows thumbnail cache files.
    #[test]
    fn exclude_thumbs_db() {
        let set = FilterSet::from_rules([FilterRule::exclude("Thumbs.db")]).unwrap();

        assert!(!set.allows(Path::new("Thumbs.db"), false));
        assert!(!set.allows(Path::new("photos/Thumbs.db"), false));

        assert!(set.allows(Path::new("thumbs.db"), false)); // Case sensitive
        assert!(set.allows(Path::new("Thumbs"), false));
    }
}

// ============================================================================
// 2. Directory Patterns (dir/)
// ============================================================================

mod directory_patterns {
    use super::*;

    /// Test: --exclude "build/"
    /// Excludes directory named "build" and all its contents.
    #[test]
    fn exclude_directory_and_contents() {
        let set = FilterSet::from_rules([FilterRule::exclude("build/")]).unwrap();

        // Should exclude the directory itself (when is_dir=true)
        assert!(!set.allows(Path::new("build"), true));

        // Should NOT exclude a file named "build"
        assert!(set.allows(Path::new("build"), false));

        // Should exclude all contents of the directory
        assert!(!set.allows(Path::new("build/output.bin"), false));
        assert!(!set.allows(Path::new("build/debug/main"), false));
        assert!(!set.allows(Path::new("build/release/app.exe"), false));

        // Should exclude nested "build" directories
        assert!(!set.allows(Path::new("project/build"), true));
        assert!(!set.allows(Path::new("project/build/out.o"), false));
    }

    /// Test: --exclude "node_modules/"
    /// Common JavaScript/Node.js pattern.
    #[test]
    fn exclude_node_modules() {
        let set = FilterSet::from_rules([FilterRule::exclude("node_modules/")]).unwrap();

        // Directory at any depth
        assert!(!set.allows(Path::new("node_modules"), true));
        assert!(!set.allows(Path::new("packages/app/node_modules"), true));
        assert!(!set.allows(Path::new("mono/packages/lib/node_modules"), true));

        // Contents of node_modules
        assert!(!set.allows(Path::new("node_modules/react/index.js"), false));
        assert!(!set.allows(Path::new("app/node_modules/lodash"), false));

        // File named node_modules (unlikely but valid)
        assert!(set.allows(Path::new("node_modules"), false));
    }

    /// Test: --exclude "__pycache__/"
    /// Python cache directory pattern.
    #[test]
    fn exclude_pycache() {
        let set = FilterSet::from_rules([FilterRule::exclude("__pycache__/")]).unwrap();

        assert!(!set.allows(Path::new("__pycache__"), true));
        assert!(!set.allows(Path::new("__pycache__/module.cpython-39.pyc"), false));
        assert!(!set.allows(Path::new("src/__pycache__"), true));

        assert!(set.allows(Path::new("pycache"), true)); // No underscores
    }

    /// Test: --exclude ".git/"
    /// Exclude git repository directory.
    #[test]
    fn exclude_git_directory() {
        let set = FilterSet::from_rules([FilterRule::exclude(".git/")]).unwrap();

        assert!(!set.allows(Path::new(".git"), true));
        assert!(!set.allows(Path::new(".git/config"), false));
        assert!(!set.allows(Path::new(".git/objects/pack"), false));
        assert!(!set.allows(Path::new("submodule/.git"), true));

        assert!(set.allows(Path::new(".gitignore"), false));
        assert!(set.allows(Path::new(".github"), true));
    }

    /// Test: --exclude "target/"
    /// Rust/Cargo build output directory.
    #[test]
    fn exclude_rust_target() {
        let set = FilterSet::from_rules([FilterRule::exclude("target/")]).unwrap();

        assert!(!set.allows(Path::new("target"), true));
        assert!(!set.allows(Path::new("target/debug/myapp"), false));
        assert!(!set.allows(Path::new("target/release/myapp"), false));
        assert!(!set.allows(Path::new("crates/lib/target"), true));
    }

    /// Test: --exclude "dist/"
    /// Distribution/build output directory.
    #[test]
    fn exclude_dist_directory() {
        let set = FilterSet::from_rules([FilterRule::exclude("dist/")]).unwrap();

        assert!(!set.allows(Path::new("dist"), true));
        assert!(!set.allows(Path::new("dist/bundle.js"), false));
        assert!(!set.allows(Path::new("packages/app/dist"), true));
    }

    /// Test: --exclude "logs/"
    /// Exclude logs directory.
    #[test]
    fn exclude_logs_directory() {
        let set = FilterSet::from_rules([FilterRule::exclude("logs/")]).unwrap();

        assert!(!set.allows(Path::new("logs"), true));
        assert!(!set.allows(Path::new("logs/app.log"), false));
        assert!(!set.allows(Path::new("logs/2024/01/error.log"), false));

        // File named logs (allowed)
        assert!(set.allows(Path::new("logs"), false));
        // logs.txt (allowed - not a directory pattern match)
        assert!(set.allows(Path::new("logs.txt"), false));
    }
}

// ============================================================================
// 3. Anchored Patterns (/root/file)
// ============================================================================

mod anchored_patterns {
    use super::*;

    /// Test: --exclude "/config.ini"
    /// Excludes config.ini only at the transfer root.
    #[test]
    fn anchored_file_at_root() {
        let set = FilterSet::from_rules([FilterRule::exclude("/config.ini")]).unwrap();

        // Should exclude at root
        assert!(!set.allows(Path::new("config.ini"), false));

        // Should NOT exclude at subdirectories
        assert!(set.allows(Path::new("dir/config.ini"), false));
        assert!(set.allows(Path::new("a/b/config.ini"), false));
        assert!(set.allows(Path::new("backup/config.ini"), false));
    }

    /// Test: --exclude "/build/"
    /// Excludes build directory only at root.
    #[test]
    fn anchored_directory_at_root() {
        let set = FilterSet::from_rules([FilterRule::exclude("/build/")]).unwrap();

        // Should exclude build directory at root
        assert!(!set.allows(Path::new("build"), true));
        assert!(!set.allows(Path::new("build/output"), false));

        // Should NOT exclude nested build directories
        assert!(set.allows(Path::new("src/build"), true));
        assert!(set.allows(Path::new("project/build/out"), false));
    }

    /// Test: --exclude "/src/generated/"
    /// Excludes specific path only at root.
    #[test]
    fn anchored_nested_path() {
        let set = FilterSet::from_rules([FilterRule::exclude("/src/generated/")]).unwrap();

        assert!(!set.allows(Path::new("src/generated"), true));
        assert!(!set.allows(Path::new("src/generated/types.rs"), false));

        // Different paths not excluded
        assert!(set.allows(Path::new("lib/src/generated"), true));
        assert!(set.allows(Path::new("other/generated"), true));
    }

    /// Test: --exclude "/README.md"
    /// Exclude README only at root.
    #[test]
    fn anchored_readme() {
        let set = FilterSet::from_rules([FilterRule::exclude("/README.md")]).unwrap();

        assert!(!set.allows(Path::new("README.md"), false));

        assert!(set.allows(Path::new("docs/README.md"), false));
        assert!(set.allows(Path::new("packages/app/README.md"), false));
    }

    /// Test: --exclude "/*.tmp"
    /// Exclude .tmp files only at root.
    #[test]
    fn anchored_wildcard_at_root() {
        let set = FilterSet::from_rules([FilterRule::exclude("/*.tmp")]).unwrap();

        assert!(!set.allows(Path::new("scratch.tmp"), false));
        assert!(!set.allows(Path::new("temp.tmp"), false));

        // Nested .tmp files allowed
        assert!(set.allows(Path::new("dir/scratch.tmp"), false));
        assert!(set.allows(Path::new("cache/temp.tmp"), false));
    }

    /// Test: --exclude "/Makefile"
    /// Exclude Makefile at root only.
    #[test]
    fn anchored_makefile() {
        let set = FilterSet::from_rules([FilterRule::exclude("/Makefile")]).unwrap();

        assert!(!set.allows(Path::new("Makefile"), false));

        assert!(set.allows(Path::new("subproject/Makefile"), false));
        assert!(set.allows(Path::new("tests/Makefile"), false));
    }

    /// Test: --exclude "/vendor/"
    /// Exclude vendor directory at root only.
    #[test]
    fn anchored_vendor() {
        let set = FilterSet::from_rules([FilterRule::exclude("/vendor/")]).unwrap();

        assert!(!set.allows(Path::new("vendor"), true));
        assert!(!set.allows(Path::new("vendor/pkg/lib.go"), false));

        assert!(set.allows(Path::new("submodule/vendor"), true));
        assert!(set.allows(Path::new("deps/vendor/pkg"), false));
    }
}

// ============================================================================
// 4. Double-Star Patterns (**/deep/file)
// ============================================================================

mod double_star_patterns {
    use super::*;

    /// Test: --exclude "**/build"
    /// Matches "build" at any depth.
    #[test]
    fn double_star_prefix() {
        let set = FilterSet::from_rules([FilterRule::exclude("**/build")]).unwrap();

        assert!(!set.allows(Path::new("build"), false));
        assert!(!set.allows(Path::new("src/build"), false));
        assert!(!set.allows(Path::new("a/b/c/build"), false));
        assert!(!set.allows(Path::new("packages/app/build"), false));
    }

    /// Test: --exclude "src/**"
    /// Matches all contents of src/ directory.
    #[test]
    fn double_star_suffix() {
        let set = FilterSet::from_rules([FilterRule::exclude("src/**")]).unwrap();

        assert!(!set.allows(Path::new("src/main.rs"), false));
        assert!(!set.allows(Path::new("src/lib.rs"), false));
        assert!(!set.allows(Path::new("src/module/mod.rs"), false));
        assert!(!set.allows(Path::new("src/a/b/c/deep.rs"), false));

        // The src directory itself is not excluded by src/**
        assert!(set.allows(Path::new("src"), true));
    }

    /// Test: --exclude "src/**/test.rs"
    /// Matches test.rs files anywhere under src/.
    #[test]
    fn double_star_middle() {
        let set = FilterSet::from_rules([FilterRule::exclude("src/**/test.rs")]).unwrap();

        assert!(!set.allows(Path::new("src/test.rs"), false));
        assert!(!set.allows(Path::new("src/module/test.rs"), false));
        assert!(!set.allows(Path::new("src/a/b/c/test.rs"), false));

        // Different filename not matched
        assert!(set.allows(Path::new("src/tests.rs"), false));
        assert!(set.allows(Path::new("src/test_utils.rs"), false));

        // Different base path not matched
        assert!(set.allows(Path::new("lib/test.rs"), false));
    }

    /// Test: --exclude "**/node_modules/**"
    /// Matches all contents of any node_modules directory.
    #[test]
    fn double_star_both_sides() {
        let set = FilterSet::from_rules([FilterRule::exclude("**/node_modules/**")]).unwrap();

        assert!(!set.allows(Path::new("node_modules/react/index.js"), false));
        assert!(!set.allows(Path::new("packages/node_modules/lodash"), false));
        assert!(!set.allows(Path::new("a/b/node_modules/pkg/lib"), false));
    }

    /// Test: --exclude "**/*.log"
    /// Matches .log files at any depth.
    #[test]
    fn double_star_with_extension() {
        let set = FilterSet::from_rules([FilterRule::exclude("**/*.log")]).unwrap();

        assert!(!set.allows(Path::new("app.log"), false));
        assert!(!set.allows(Path::new("logs/error.log"), false));
        assert!(!set.allows(Path::new("var/log/app/debug.log"), false));

        assert!(set.allows(Path::new("log.txt"), false));
        assert!(set.allows(Path::new("logger.rs"), false));
    }

    /// Test: --exclude "**/test/**"
    /// Matches everything inside any test/ directory.
    #[test]
    fn double_star_test_directory() {
        let set = FilterSet::from_rules([FilterRule::exclude("**/test/**")]).unwrap();

        assert!(!set.allows(Path::new("test/unit.rs"), false));
        assert!(!set.allows(Path::new("src/test/fixtures"), false));
        assert!(!set.allows(Path::new("packages/app/test/integration"), false));
    }

    /// Test: --exclude "**/.git/**"
    /// Matches contents of any .git directory.
    #[test]
    fn double_star_git_contents() {
        let set = FilterSet::from_rules([FilterRule::exclude("**/.git/**")]).unwrap();

        assert!(!set.allows(Path::new(".git/config"), false));
        assert!(!set.allows(Path::new(".git/objects/pack"), false));
        assert!(!set.allows(Path::new("submodule/.git/HEAD"), false));
    }

    /// Test: --exclude "packages/*/node_modules/**"
    /// Pattern with single star and double star.
    #[test]
    fn mixed_single_and_double_star() {
        let set =
            FilterSet::from_rules([FilterRule::exclude("packages/*/node_modules/**")]).unwrap();

        assert!(!set.allows(Path::new("packages/app/node_modules/react"), false));
        assert!(!set.allows(Path::new("packages/lib/node_modules/lodash"), false));

        // Root node_modules not matched
        assert!(set.allows(Path::new("node_modules/pkg"), false));
        // Nested packages not matched by single *
        assert!(set.allows(Path::new("packages/scope/app/node_modules"), false));
    }
}

// ============================================================================
// 5. Character Classes ([abc])
// ============================================================================

mod character_classes {
    use super::*;

    /// Test: --exclude "file[123].txt"
    /// Matches file1.txt, file2.txt, file3.txt.
    #[test]
    fn character_class_enumeration() {
        let set = FilterSet::from_rules([FilterRule::exclude("file[123].txt")]).unwrap();

        assert!(!set.allows(Path::new("file1.txt"), false));
        assert!(!set.allows(Path::new("file2.txt"), false));
        assert!(!set.allows(Path::new("file3.txt"), false));

        // Not in class
        assert!(set.allows(Path::new("file4.txt"), false));
        assert!(set.allows(Path::new("file0.txt"), false));
        assert!(set.allows(Path::new("filea.txt"), false));
    }

    /// Test: --exclude "file[a-z].txt"
    /// Matches filea.txt through filez.txt.
    #[test]
    fn character_class_range_lowercase() {
        let set = FilterSet::from_rules([FilterRule::exclude("file[a-z].txt")]).unwrap();

        assert!(!set.allows(Path::new("filea.txt"), false));
        assert!(!set.allows(Path::new("filem.txt"), false));
        assert!(!set.allows(Path::new("filez.txt"), false));

        // Uppercase not in range
        assert!(set.allows(Path::new("fileA.txt"), false));
        assert!(set.allows(Path::new("fileZ.txt"), false));

        // Digit not in range
        assert!(set.allows(Path::new("file1.txt"), false));
    }

    /// Test: --exclude "file[A-Z].txt"
    /// Matches fileA.txt through fileZ.txt.
    #[test]
    fn character_class_range_uppercase() {
        let set = FilterSet::from_rules([FilterRule::exclude("file[A-Z].txt")]).unwrap();

        assert!(!set.allows(Path::new("fileA.txt"), false));
        assert!(!set.allows(Path::new("fileM.txt"), false));
        assert!(!set.allows(Path::new("fileZ.txt"), false));

        // Lowercase not in range
        assert!(set.allows(Path::new("filea.txt"), false));
    }

    /// Test: --exclude "log[0-9].txt"
    /// Matches log0.txt through log9.txt.
    #[test]
    fn character_class_range_digits() {
        let set = FilterSet::from_rules([FilterRule::exclude("log[0-9].txt")]).unwrap();

        assert!(!set.allows(Path::new("log0.txt"), false));
        assert!(!set.allows(Path::new("log5.txt"), false));
        assert!(!set.allows(Path::new("log9.txt"), false));

        // Not a digit
        assert!(set.allows(Path::new("loga.txt"), false));
        assert!(set.allows(Path::new("logX.txt"), false));
    }

    /// Test: --exclude "file[!0-9].txt"
    /// Matches files where the character is NOT a digit.
    #[test]
    fn character_class_negation_exclamation() {
        let set = FilterSet::from_rules([FilterRule::exclude("file[!0-9].txt")]).unwrap();

        // Non-digits excluded
        assert!(!set.allows(Path::new("filea.txt"), false));
        assert!(!set.allows(Path::new("fileX.txt"), false));
        assert!(!set.allows(Path::new("file_.txt"), false));

        // Digits allowed
        assert!(set.allows(Path::new("file0.txt"), false));
        assert!(set.allows(Path::new("file5.txt"), false));
        assert!(set.allows(Path::new("file9.txt"), false));
    }

    /// Test: --exclude "file[^a-z].txt"
    /// Matches files where the character is NOT lowercase.
    #[test]
    fn character_class_negation_caret() {
        let set = FilterSet::from_rules([FilterRule::exclude("file[^a-z].txt")]).unwrap();

        // Non-lowercase excluded
        assert!(!set.allows(Path::new("file1.txt"), false));
        assert!(!set.allows(Path::new("fileA.txt"), false));
        assert!(!set.allows(Path::new("file_.txt"), false));

        // Lowercase allowed
        assert!(set.allows(Path::new("filea.txt"), false));
        assert!(set.allows(Path::new("filez.txt"), false));
    }

    /// Test: --exclude "[a-zA-Z0-9]"
    /// Multiple ranges in one class.
    #[test]
    fn character_class_multiple_ranges() {
        let set = FilterSet::from_rules([FilterRule::exclude("[a-zA-Z0-9]")]).unwrap();

        // Alphanumeric single chars excluded
        assert!(!set.allows(Path::new("a"), false));
        assert!(!set.allows(Path::new("Z"), false));
        assert!(!set.allows(Path::new("5"), false));

        // Non-alphanumeric allowed
        assert!(set.allows(Path::new("_"), false));
        assert!(set.allows(Path::new("-"), false));
    }

    /// Test: --exclude "*.[ch]"
    /// C source and header files.
    #[test]
    fn character_class_c_files() {
        let set = FilterSet::from_rules([FilterRule::exclude("*.[ch]")]).unwrap();

        assert!(!set.allows(Path::new("main.c"), false));
        assert!(!set.allows(Path::new("header.h"), false));
        assert!(!set.allows(Path::new("src/util.c"), false));

        assert!(set.allows(Path::new("main.cpp"), false));
        assert!(set.allows(Path::new("header.hpp"), false));
    }

    /// Test: --exclude "[-_]file.txt"
    /// Hyphen at start of class is literal.
    #[test]
    fn character_class_hyphen_literal() {
        let set = FilterSet::from_rules([FilterRule::exclude("[-_]file.txt")]).unwrap();

        assert!(!set.allows(Path::new("-file.txt"), false));
        assert!(!set.allows(Path::new("_file.txt"), false));

        assert!(set.allows(Path::new("afile.txt"), false));
    }

    /// Test: --exclude "file[ab-].txt"
    /// Hyphen at end of class is literal.
    #[test]
    fn character_class_hyphen_at_end() {
        let set = FilterSet::from_rules([FilterRule::exclude("file[ab-].txt")]).unwrap();

        assert!(!set.allows(Path::new("filea.txt"), false));
        assert!(!set.allows(Path::new("fileb.txt"), false));
        assert!(!set.allows(Path::new("file-.txt"), false));

        assert!(set.allows(Path::new("filec.txt"), false));
    }

    /// Test: --exclude "[]ab]file.txt"
    /// Closing bracket at start of class is literal.
    #[test]
    fn character_class_bracket_literal() {
        let set = FilterSet::from_rules([FilterRule::exclude("[]ab]file.txt")]).unwrap();

        assert!(!set.allows(Path::new("]file.txt"), false));
        assert!(!set.allows(Path::new("afile.txt"), false));
        assert!(!set.allows(Path::new("bfile.txt"), false));

        assert!(set.allows(Path::new("cfile.txt"), false));
    }
}

// ============================================================================
// 6. Negation Patterns (! modifier)
// ============================================================================

mod negation_patterns {
    use super::*;

    /// Test: --exclude with negation: excludes files NOT matching the pattern.
    /// A negated exclude "- ! *.txt" excludes everything except .txt files.
    #[test]
    fn negated_exclude_keeps_matching() {
        let rules = [FilterRule::exclude("*.txt").with_negate(true)];
        let set = FilterSet::from_rules(rules).unwrap();

        // .txt files should be allowed (pattern matches, negated means NOT excluded)
        assert!(set.allows(Path::new("readme.txt"), false));
        assert!(set.allows(Path::new("docs/notes.txt"), false));

        // Non-.txt files should be excluded (pattern doesn't match, negated means excluded)
        assert!(!set.allows(Path::new("readme.md"), false));
        assert!(!set.allows(Path::new("script.sh"), false));
        assert!(!set.allows(Path::new("image.png"), false));
    }

    /// Test: Negated pattern with directory rule.
    #[test]
    fn negated_directory_pattern() {
        let rules = [FilterRule::exclude("cache/").with_negate(true)];
        let set = FilterSet::from_rules(rules).unwrap();

        // "cache" directory matches pattern, negated = allowed
        assert!(set.allows(Path::new("cache"), true));

        // Other directories don't match, negated = excluded
        assert!(!set.allows(Path::new("temp"), true));
        assert!(!set.allows(Path::new("build"), true));
    }

    /// Test: Negated anchored pattern.
    #[test]
    fn negated_anchored_pattern() {
        let rules = [FilterRule::exclude("/important").with_negate(true)];
        let set = FilterSet::from_rules(rules).unwrap();

        // /important matches, negated = allowed
        assert!(set.allows(Path::new("important"), false));

        // Other root paths don't match, negated = excluded
        assert!(!set.allows(Path::new("other"), false));

        // Nested paths don't match anchored pattern, negated = excluded
        assert!(!set.allows(Path::new("dir/important"), false));
    }

    /// Test: Combining negated and regular exclude rules.
    #[test]
    fn negated_combined_with_regular() {
        // First rule: exclude .tmp files
        // Second rule: exclude non-.txt files (negated pattern)
        let rules = [
            FilterRule::exclude("*.tmp"),
            FilterRule::exclude("*.txt").with_negate(true),
        ];
        let set = FilterSet::from_rules(rules).unwrap();

        // .txt files: first rule doesn't match, second rule matches so NOT excluded
        assert!(set.allows(Path::new("notes.txt"), false));

        // .tmp files: first rule matches and excludes
        assert!(!set.allows(Path::new("scratch.tmp"), false));

        // .log files: first rule doesn't match, second doesn't match *.txt so excluded
        assert!(!set.allows(Path::new("error.log"), false));
    }

    /// Test: Negated include rule.
    /// A negated include includes files that do NOT match the pattern.
    #[test]
    fn negated_include_pattern() {
        // Include everything except .bak files, then exclude all
        let rules = [
            FilterRule::include("*.bak").with_negate(true),
            FilterRule::exclude("*"),
        ];
        let set = FilterSet::from_rules(rules).unwrap();

        // Files not matching *.bak are included by negated rule
        assert!(set.allows(Path::new("file.txt"), false));
        assert!(set.allows(Path::new("script.sh"), false));

        // .bak files: negated include doesn't match, falls through to exclude(*)
        assert!(!set.allows(Path::new("backup.bak"), false));
    }

    /// Test: Negated pattern preserves flag state through chaining.
    #[test]
    fn negated_pattern_with_other_modifiers() {
        let rule = FilterRule::exclude("*.log")
            .with_negate(true)
            .with_perishable(true)
            .with_sides(true, false);

        assert!(rule.is_negated());
        assert!(rule.is_perishable());
        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());
    }
}

// ============================================================================
// 7. Multiple --exclude Flags
// ============================================================================

mod multiple_exclude_flags {
    use super::*;

    /// Test: Multiple exclude patterns work together.
    /// --exclude "*.txt" --exclude "*.log"
    #[test]
    fn multiple_extension_excludes() {
        let rules = [FilterRule::exclude("*.txt"), FilterRule::exclude("*.log")];
        let set = FilterSet::from_rules(rules).unwrap();

        // Both patterns should exclude
        assert!(!set.allows(Path::new("readme.txt"), false));
        assert!(!set.allows(Path::new("error.log"), false));

        // Other extensions allowed
        assert!(set.allows(Path::new("main.rs"), false));
        assert!(set.allows(Path::new("data.json"), false));
    }

    /// Test: Multiple directory excludes.
    /// --exclude "build/" --exclude "dist/" --exclude "node_modules/"
    #[test]
    fn multiple_directory_excludes() {
        let rules = [
            FilterRule::exclude("build/"),
            FilterRule::exclude("dist/"),
            FilterRule::exclude("node_modules/"),
        ];
        let set = FilterSet::from_rules(rules).unwrap();

        assert!(!set.allows(Path::new("build"), true));
        assert!(!set.allows(Path::new("dist"), true));
        assert!(!set.allows(Path::new("node_modules"), true));

        assert!(set.allows(Path::new("src"), true));
        assert!(set.allows(Path::new("lib"), true));
    }

    /// Test: Include exception before exclude.
    /// rsync uses first-match-wins semantics.
    /// --include "important.log" --exclude "*.log"
    #[test]
    fn include_exception_before_exclude() {
        let rules = [
            FilterRule::include("important.log"),
            FilterRule::exclude("*.log"),
        ];
        let set = FilterSet::from_rules(rules).unwrap();

        // Specifically included
        assert!(set.allows(Path::new("important.log"), false));

        // Other .log files excluded
        assert!(!set.allows(Path::new("debug.log"), false));
        assert!(!set.allows(Path::new("error.log"), false));
    }

    /// Test: Complex rule ordering.
    /// --include "test_utils.rs" --exclude "test_*.rs" --include "*.rs" --exclude "*"
    #[test]
    fn complex_rule_ordering() {
        let rules = [
            FilterRule::include("test_utils.rs"),
            FilterRule::exclude("test_*.rs"),
            FilterRule::include("*.rs"),
            FilterRule::exclude("*"),
        ];
        let set = FilterSet::from_rules(rules).unwrap();

        // test_utils.rs specifically included (rule 1)
        assert!(set.allows(Path::new("test_utils.rs"), false));

        // Other test_*.rs excluded (rule 2)
        assert!(!set.allows(Path::new("test_main.rs"), false));
        assert!(!set.allows(Path::new("test_lib.rs"), false));

        // Regular .rs files included (rule 3)
        assert!(set.allows(Path::new("main.rs"), false));
        assert!(set.allows(Path::new("lib.rs"), false));

        // Other files excluded (rule 4)
        assert!(!set.allows(Path::new("Cargo.toml"), false));
        assert!(!set.allows(Path::new("README.md"), false));
    }

    /// Test: Directory exception pattern.
    /// --include "cache/important/**" --exclude "cache/"
    #[test]
    fn directory_exception() {
        let rules = [
            FilterRule::include("cache/important/**"),
            FilterRule::exclude("cache/"),
        ];
        let set = FilterSet::from_rules(rules).unwrap();

        // important subdirectory contents are included
        assert!(set.allows(Path::new("cache/important/data.txt"), false));
        assert!(set.allows(Path::new("cache/important/deep/file"), false));

        // Other cache contents excluded
        assert!(!set.allows(Path::new("cache/temp"), false));
        assert!(!set.allows(Path::new("cache/session"), false));
    }

    /// Test: Common gitignore-style pattern set.
    #[test]
    fn gitignore_style_patterns() {
        let rules = [
            FilterRule::exclude("*.log"),
            FilterRule::exclude("*.tmp"),
            FilterRule::exclude("*.swp"),
            FilterRule::exclude("*.swo"),
            FilterRule::exclude("*~"),
            FilterRule::exclude("build/"),
            FilterRule::exclude("dist/"),
            FilterRule::exclude("node_modules/"),
            FilterRule::exclude(".git/"),
            FilterRule::exclude(".DS_Store"),
            FilterRule::exclude("Thumbs.db"),
        ];
        let set = FilterSet::from_rules(rules).unwrap();

        // All patterns should exclude their targets
        assert!(!set.allows(Path::new("debug.log"), false));
        assert!(!set.allows(Path::new("temp.tmp"), false));
        assert!(!set.allows(Path::new(".main.rs.swp"), false));
        assert!(!set.allows(Path::new("file~"), false));
        assert!(!set.allows(Path::new("build"), true));
        assert!(!set.allows(Path::new("node_modules"), true));
        assert!(!set.allows(Path::new(".git"), true));
        assert!(!set.allows(Path::new(".DS_Store"), false));
        assert!(!set.allows(Path::new("Thumbs.db"), false));

        // Normal files allowed
        assert!(set.allows(Path::new("main.rs"), false));
        assert!(set.allows(Path::new("src/lib.rs"), false));
        assert!(set.allows(Path::new("Cargo.toml"), false));
    }

    /// Test: Rust project patterns.
    #[test]
    fn rust_project_patterns() {
        let rules = [
            FilterRule::exclude("/target/"),
            FilterRule::exclude("**/*.rs.bk"),
            FilterRule::exclude("Cargo.lock"),
        ];
        let set = FilterSet::from_rules(rules).unwrap();

        assert!(!set.allows(Path::new("target"), true));
        assert!(!set.allows(Path::new("target/debug/app"), false));
        assert!(!set.allows(Path::new("main.rs.bk"), false));
        assert!(!set.allows(Path::new("src/lib.rs.bk"), false));
        assert!(!set.allows(Path::new("Cargo.lock"), false));

        // Source files allowed
        assert!(set.allows(Path::new("src/main.rs"), false));
        assert!(set.allows(Path::new("Cargo.toml"), false));

        // Nested target directories allowed (anchored pattern)
        assert!(set.allows(Path::new("crates/lib/target"), true));
    }

    /// Test: Many rules (stress test).
    #[test]
    fn many_exclude_rules() {
        let rules: Vec<_> = (0..100)
            .map(|i| FilterRule::exclude(format!("exclude_{i}.txt")))
            .collect();
        let set = FilterSet::from_rules(rules).unwrap();

        // All generated patterns should exclude
        assert!(!set.allows(Path::new("exclude_0.txt"), false));
        assert!(!set.allows(Path::new("exclude_50.txt"), false));
        assert!(!set.allows(Path::new("exclude_99.txt"), false));

        // Patterns not in list should be allowed
        assert!(set.allows(Path::new("exclude_100.txt"), false));
        assert!(set.allows(Path::new("other.txt"), false));
    }
}

// ============================================================================
// 8. Case Sensitivity
// ============================================================================

mod case_sensitivity {
    use super::*;

    /// Test: rsync patterns are case-sensitive by default.
    /// --exclude "README.md" does NOT match "readme.md"
    #[test]
    fn case_sensitive_exact_match() {
        let set = FilterSet::from_rules([FilterRule::exclude("README.md")]).unwrap();

        assert!(!set.allows(Path::new("README.md"), false));

        // Different case should NOT be excluded
        assert!(set.allows(Path::new("readme.md"), false));
        assert!(set.allows(Path::new("Readme.md"), false));
        assert!(set.allows(Path::new("README.MD"), false));
        assert!(set.allows(Path::new("ReadMe.Md"), false));
    }

    /// Test: Case-sensitive wildcard patterns.
    /// --exclude "*.TXT" does NOT match "*.txt"
    #[test]
    fn case_sensitive_extension() {
        let set = FilterSet::from_rules([FilterRule::exclude("*.TXT")]).unwrap();

        assert!(!set.allows(Path::new("FILE.TXT"), false));
        assert!(!set.allows(Path::new("README.TXT"), false));

        // Lowercase extension not matched
        assert!(set.allows(Path::new("file.txt"), false));
        assert!(set.allows(Path::new("readme.txt"), false));
    }

    /// Test: Case-sensitive directory patterns.
    /// --exclude "Build/" does NOT match "build/"
    #[test]
    fn case_sensitive_directory() {
        let set = FilterSet::from_rules([FilterRule::exclude("Build/")]).unwrap();

        assert!(!set.allows(Path::new("Build"), true));
        assert!(!set.allows(Path::new("Build/output"), false));

        // Different case not matched
        assert!(set.allows(Path::new("build"), true));
        assert!(set.allows(Path::new("BUILD"), true));
    }

    /// Test: Case-sensitive character classes.
    /// --exclude "[A-Z]*" only matches uppercase starting files.
    #[test]
    fn case_sensitive_character_class() {
        let set = FilterSet::from_rules([FilterRule::exclude("[A-Z]*")]).unwrap();

        assert!(!set.allows(Path::new("README"), false));
        assert!(!set.allows(Path::new("Makefile"), false));
        assert!(!set.allows(Path::new("Z"), false));

        // Lowercase starting files not matched
        assert!(set.allows(Path::new("readme"), false));
        assert!(set.allows(Path::new("makefile"), false));
    }

    /// Test: Windows-style paths maintain case sensitivity.
    #[test]
    fn case_sensitive_windows_common_files() {
        let set = FilterSet::from_rules([FilterRule::exclude("Thumbs.db")]).unwrap();

        assert!(!set.allows(Path::new("Thumbs.db"), false));

        // Windows is case-insensitive but rsync patterns are case-sensitive
        assert!(set.allows(Path::new("thumbs.db"), false));
        assert!(set.allows(Path::new("THUMBS.DB"), false));
    }

    /// Test: Mixed case patterns.
    #[test]
    fn mixed_case_patterns() {
        let rules = [
            FilterRule::exclude("Makefile"),
            FilterRule::exclude("makefile"),
            FilterRule::exclude("GNUmakefile"),
        ];
        let set = FilterSet::from_rules(rules).unwrap();

        assert!(!set.allows(Path::new("Makefile"), false));
        assert!(!set.allows(Path::new("makefile"), false));
        assert!(!set.allows(Path::new("GNUmakefile"), false));

        // Cases not explicitly listed are allowed
        assert!(set.allows(Path::new("MAKEFILE"), false));
        assert!(set.allows(Path::new("MakeFile"), false));
    }

    /// Test: To match case-insensitively, use character classes.
    /// Demonstrates workaround for case-insensitive matching.
    #[test]
    fn case_insensitive_workaround() {
        // Match README regardless of case using character classes
        let set = FilterSet::from_rules([FilterRule::exclude("[Rr][Ee][Aa][Dd][Mm][Ee].[Mm][Dd]")])
            .unwrap();

        assert!(!set.allows(Path::new("README.md"), false));
        assert!(!set.allows(Path::new("readme.md"), false));
        assert!(!set.allows(Path::new("Readme.MD"), false));
        assert!(!set.allows(Path::new("ReadMe.Md"), false));
    }

    /// Test: Case matters for anchored patterns too.
    #[test]
    fn case_sensitive_anchored() {
        let set = FilterSet::from_rules([FilterRule::exclude("/Config.ini")]).unwrap();

        assert!(!set.allows(Path::new("Config.ini"), false));

        assert!(set.allows(Path::new("config.ini"), false));
        assert!(set.allows(Path::new("CONFIG.INI"), false));
    }
}

// ============================================================================
// Additional Edge Cases
// ============================================================================

mod edge_cases {
    use super::*;

    /// Test: Empty exclude pattern (should still create valid rule).
    #[test]
    fn empty_exclude_pattern() {
        let set = FilterSet::from_rules([FilterRule::exclude("")]).unwrap();
        assert!(!set.is_empty());
    }

    /// Test: Exclude pattern with only slash.
    #[test]
    fn slash_only_pattern() {
        // A pattern of just "/" is anchored and directory-only but empty after normalization
        let set = FilterSet::from_rules([FilterRule::exclude("/")]).unwrap();
        assert!(!set.is_empty());
    }

    /// Test: Question mark wildcard.
    #[test]
    fn question_mark_wildcard() {
        let set = FilterSet::from_rules([FilterRule::exclude("file?.txt")]).unwrap();

        assert!(!set.allows(Path::new("file1.txt"), false));
        assert!(!set.allows(Path::new("fileA.txt"), false));
        assert!(!set.allows(Path::new("file_.txt"), false));

        // Zero chars doesn't match
        assert!(set.allows(Path::new("file.txt"), false));
        // Two chars doesn't match
        assert!(set.allows(Path::new("file12.txt"), false));
    }

    /// Test: Escaped special characters.
    #[test]
    fn escaped_characters() {
        // Literal asterisk
        let set1 = FilterSet::from_rules([FilterRule::exclude("file\\*.txt")]).unwrap();
        assert!(!set1.allows(Path::new("file*.txt"), false));
        assert!(set1.allows(Path::new("file1.txt"), false));

        // Literal question mark
        let set2 = FilterSet::from_rules([FilterRule::exclude("what\\?")]).unwrap();
        assert!(!set2.allows(Path::new("what?"), false));
        assert!(set2.allows(Path::new("whatX"), false));

        // Literal brackets
        let set3 = FilterSet::from_rules([FilterRule::exclude("array\\[0\\]")]).unwrap();
        assert!(!set3.allows(Path::new("array[0]"), false));
        assert!(set3.allows(Path::new("array0"), false));
    }

    /// Test: Very long pattern and path.
    #[test]
    fn long_pattern_and_path() {
        let long_name = "x".repeat(200);
        let pattern = format!("{long_name}*.txt");
        let set = FilterSet::from_rules([FilterRule::exclude(&pattern)]).unwrap();

        let matching = format!("{long_name}foo.txt");
        assert!(!set.allows(Path::new(&matching), false));

        let non_matching = format!("y{}.txt", "x".repeat(199));
        assert!(set.allows(Path::new(&non_matching), false));
    }

    /// Test: Pattern with multiple extensions.
    #[test]
    fn multiple_extensions() {
        let set = FilterSet::from_rules([FilterRule::exclude("*.tar.gz")]).unwrap();

        assert!(!set.allows(Path::new("archive.tar.gz"), false));
        assert!(!set.allows(Path::new("backup.tar.gz"), false));

        assert!(set.allows(Path::new("archive.tar"), false));
        assert!(set.allows(Path::new("file.gz"), false));
    }

    /// Test: Pattern invalid syntax reports error.
    #[test]
    fn invalid_pattern_error() {
        let result = FilterSet::from_rules([FilterRule::exclude("[")]);
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert_eq!(error.pattern(), "[");
    }
}
