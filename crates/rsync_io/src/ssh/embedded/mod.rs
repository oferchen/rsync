//! Embedded SSH client support.
//!
//! This module provides components for the built-in SSH client that does not
//! depend on an external `ssh` binary. Cipher selection is hardware-aware,
//! preferring AES-GCM on CPUs with AES acceleration.

pub mod cipher;
