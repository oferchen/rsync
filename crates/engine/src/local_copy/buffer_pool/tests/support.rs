//! Shared fixtures for buffer-pool test submodules.

use super::super::BufferAllocator;
use std::sync::atomic::AtomicUsize;

/// A test-only allocator that counts allocations and deallocations.
#[derive(Debug)]
pub(super) struct TrackingAllocator {
    alloc_count: AtomicUsize,
    dealloc_count: AtomicUsize,
}

impl TrackingAllocator {
    pub(super) fn new() -> Self {
        Self {
            alloc_count: AtomicUsize::new(0),
            dealloc_count: AtomicUsize::new(0),
        }
    }

    pub(super) fn alloc_count(&self) -> usize {
        self.alloc_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(super) fn dealloc_count(&self) -> usize {
        self.dealloc_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl BufferAllocator for TrackingAllocator {
    fn allocate(&self, size: usize) -> Vec<u8> {
        self.alloc_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        vec![0u8; size]
    }

    fn deallocate(&self, _buffer: Vec<u8>) {
        self.dealloc_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A counting allocator that tracks allocations for adaptive resizing tests.
#[derive(Debug)]
pub(super) struct AdaptiveTrackingAllocator {
    alloc_count: AtomicUsize,
    dealloc_count: AtomicUsize,
}

impl AdaptiveTrackingAllocator {
    pub(super) fn new() -> Self {
        Self {
            alloc_count: AtomicUsize::new(0),
            dealloc_count: AtomicUsize::new(0),
        }
    }

    pub(super) fn alloc_count(&self) -> usize {
        self.alloc_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(super) fn dealloc_count(&self) -> usize {
        self.dealloc_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl BufferAllocator for AdaptiveTrackingAllocator {
    fn allocate(&self, size: usize) -> Vec<u8> {
        self.alloc_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        vec![0u8; size]
    }

    fn deallocate(&self, _buffer: Vec<u8>) {
        self.dealloc_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}
