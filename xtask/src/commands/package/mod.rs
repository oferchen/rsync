mod args;
mod build;
mod tarball;

pub use args::{PackageOptions, parse_args};
pub use build::execute;

pub(crate) const AMD64_TARBALL_TARGET: &str = "x86_64-unknown-linux-gnu";
pub(crate) const AMD64_TARBALL_ARCH: &str = "amd64";

#[cfg(test)]
mod tests;
