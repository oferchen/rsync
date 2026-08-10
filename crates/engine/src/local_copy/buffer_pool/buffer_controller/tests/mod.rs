//! Test submodules for the PID buffer-size controller.
//!
//! The controller's behavioural surface is broad (basic PID term semantics,
//! convergence under varied workloads, and statistical properties) so the
//! tests are partitioned by concern to keep each file reviewable and below
//! the workspace LoC cap.

mod support;

mod basic;
mod convergence_advanced;
mod convergence_basic;
mod convergence_extended;
mod property;
