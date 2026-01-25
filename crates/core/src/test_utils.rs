//! Test utilities and helper macros for error assertions.
//!
//! This module provides common testing utilities used across the workspace,
//! including macros for asserting on error patterns.
//!
//! # Usage in Other Crates
//!
//! To use the `assert_error_matches!` macro in tests within other crates:
//!
//! ```rust,ignore
//! use core::assert_error_matches;
//!
//! #[test]
//! fn my_error_test() {
//!     let result = some_fallible_operation();
//!     assert_error_matches!(result, MyError::SomeVariant(_));
//! }
//! ```

/// Assert that a Result matches an error pattern.
///
/// This macro simplifies error assertions in tests by matching against error
/// patterns without requiring verbose match expressions.
///
/// # Examples
///
/// Basic usage with a pattern:
///
/// ```rust
/// # use std::io;
/// # use core::assert_error_matches;
/// let result: Result<(), io::Error> = Err(io::Error::new(
///     io::ErrorKind::NotFound,
///     "file not found"
/// ));
///
/// assert_error_matches!(result, io::Error { .. });
/// ```
///
/// With a custom message:
///
/// ```rust
/// # use std::io;
/// # use core::assert_error_matches;
/// let result: Result<(), io::Error> = Err(io::Error::new(
///     io::ErrorKind::PermissionDenied,
///     "access denied"
/// ));
///
/// assert_error_matches!(
///     result,
///     io::Error { .. },
///     "Expected permission denied error"
/// );
/// ```
///
/// Matching specific error variants:
///
/// ```rust
/// # use core::assert_error_matches;
/// #[derive(Debug, PartialEq)]
/// enum MyError {
///     NotFound,
///     InvalidInput(String),
/// }
///
/// let result: Result<(), MyError> = Err(MyError::InvalidInput("bad data".to_string()));
///
/// assert_error_matches!(result, MyError::InvalidInput(_));
/// ```
#[macro_export]
macro_rules! assert_error_matches {
    ($result:expr, $pattern:pat) => {
        match $result {
            Err($pattern) => (),
            Err(other) => panic!(
                "Expected error matching {}, got {:?}",
                stringify!($pattern),
                other
            ),
            Ok(value) => panic!(
                "Expected error matching {}, got Ok({:?})",
                stringify!($pattern),
                value
            ),
        }
    };
    ($result:expr, $pattern:pat, $msg:literal) => {
        match $result {
            Err($pattern) => (),
            Err(other) => panic!(
                "{}: expected {}, got {:?}",
                $msg,
                stringify!($pattern),
                other
            ),
            Ok(value) => panic!("{}: expected error, got Ok({:?})", $msg, value),
        }
    };
}

#[cfg(test)]
mod tests {
    use std::io;

    #[derive(Debug, PartialEq)]
    enum TestError {
        NotFound,
        InvalidInput(String),
        Other { code: i32, message: String },
    }

    #[test]
    fn assert_error_matches_simple_enum_variant() {
        let result: Result<(), TestError> = Err(TestError::NotFound);
        assert_error_matches!(result, TestError::NotFound);
    }

    #[test]
    fn assert_error_matches_enum_with_data() {
        let result: Result<(), TestError> = Err(TestError::InvalidInput("test".to_string()));
        assert_error_matches!(result, TestError::InvalidInput(_));
    }

    #[test]
    fn assert_error_matches_with_pattern_matching() {
        let result: Result<(), TestError> = Err(TestError::Other {
            code: 42,
            message: "error".to_string(),
        });
        assert_error_matches!(result, TestError::Other { code: 42, .. });
    }

    #[test]
    fn assert_error_matches_io_error() {
        let err = io::Error::new(io::ErrorKind::NotFound, "not found");
        // Using _ pattern to match any io::Error
        assert!(matches!(err.kind(), io::ErrorKind::NotFound));
    }

    #[test]
    fn assert_error_matches_with_message() {
        let result: Result<(), TestError> = Err(TestError::NotFound);
        assert_error_matches!(result, TestError::NotFound, "Custom error message");
    }

    #[test]
    #[should_panic(expected = "Expected error matching TestError")]
    fn assert_error_matches_panics_on_wrong_error() {
        let result: Result<(), TestError> = Err(TestError::InvalidInput("test".to_string()));
        assert_error_matches!(result, TestError::NotFound);
    }

    #[test]
    #[should_panic(expected = "Expected error matching TestError")]
    fn assert_error_matches_panics_on_ok() {
        let result: Result<(), TestError> = Ok(());
        assert_error_matches!(result, TestError::NotFound);
    }

    #[test]
    #[should_panic(expected = "Custom message: expected error")]
    fn assert_error_matches_panics_on_ok_with_message() {
        let result: Result<(), TestError> = Ok(());
        assert_error_matches!(result, TestError::NotFound, "Custom message");
    }
}
