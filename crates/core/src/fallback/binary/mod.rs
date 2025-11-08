mod availability;
mod candidates;
mod diagnostics;
#[cfg(unix)]
mod unix;

pub use self::availability::fallback_binary_available;
pub use self::candidates::fallback_binary_candidates;
pub use self::diagnostics::describe_missing_fallback_binary;

#[cfg(test)]
mod tests;
