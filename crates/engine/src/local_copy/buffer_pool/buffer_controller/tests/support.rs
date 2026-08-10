//! Shared helpers for buffer-controller test submodules.

/// Representative LAN throughput setpoint (100 MB/s).
pub(super) fn lan_setpoint() -> u64 {
    100 * 1024 * 1024
}

/// Synthetic plant model: throughput = min(k * buffer_size, capacity).
///
/// This models the observation that larger buffers reduce syscall
/// overhead and improve throughput up to the link's physical limit.
pub(super) fn linear_plant(buffer_size: usize, k: f64, capacity: f64) -> u64 {
    (k * buffer_size as f64).min(capacity) as u64
}

/// Variance of a slice of `usize` values, returned as f64.
pub(super) fn variance(values: &[usize]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mean = values.iter().sum::<usize>() as f64 / values.len() as f64;
    values
        .iter()
        .map(|&v| (v as f64 - mean).powi(2))
        .sum::<f64>()
        / values.len() as f64
}
