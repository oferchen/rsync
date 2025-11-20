mod availability;
mod candidates;
mod diagnostics;
#[cfg(unix)]
mod unix;

pub use self::availability::{
    fallback_binary_available, fallback_binary_is_self, fallback_binary_path,
};
pub use self::candidates::fallback_binary_candidates;
pub use self::diagnostics::describe_missing_fallback_binary;

#[cfg(test)]
mod tests;
