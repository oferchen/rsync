//! Compile-time constants emitted by the build script.

#[allow(clippy::doc_markdown)]
mod generated_constants {
    include!(concat!(env!("OUT_DIR"), "/workspace_generated.rs"));
}

pub use generated_constants::*;
