//! crates/daemon/src/daemon/concurrent_tests.rs
//!
//! Integration tests for concurrent session and connection tracking.
//!
//! These tests verify that SessionRegistry and ConnectionPool work correctly
//! under concurrent access patterns typical of a multi-threaded daemon.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use super::connection_pool::{ConnectionPool, IpStats};
use super::session_registry::{SessionRegistry, SessionState};

fn test_addr(ip_last: u8, port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, ip_last)), port)
}

/// Test concurrent registration and immediate unregistration.
#[test]
fn concurrent_register_unregister_stress() {
    let registry = Arc::new(SessionRegistry::new());
    let pool = Arc::new(ConnectionPool::new());
    let barrier = Arc::new(Barrier::new(20));
    let mut handles = vec![];

    for thread_id in 0..20 {
        let registry = Arc::clone(&registry);
        let pool = Arc::clone(&pool);
        let barrier = Arc::clone(&barrier);

        let handle = thread::spawn(move || {
            barrier.wait(); // Synchronize start

            for i in 0..500 {
                let port = (thread_id * 1000 + i) as u16;
                let addr = test_addr((thread_id % 256) as u8, port);

                // Register in both
                let session_id = registry.register(addr, None);
                let conn_id = pool.register(addr);

                // Update state
                registry.set_state(&session_id, SessionState::Transferring);
                registry.set_module(&session_id, format!("module_{thread_id}"));
                pool.set_module(&conn_id, format!("module_{thread_id}"));
                pool.add_bytes(&conn_id, 100, 50);

                // Mark complete and unregister
                registry.set_state(&session_id, SessionState::Completed);
                registry.unregister(&session_id);
                pool.unregister(&conn_id);
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("thread should not panic");
    }

    // All entries should be cleaned up
    assert_eq!(registry.count(), 0);
    assert_eq!(pool.count(), 0);
}

/// Test concurrent reads and writes don't cause data races.
#[test]
fn concurrent_reads_and_writes() {
    let registry = Arc::new(SessionRegistry::new());
    let pool = Arc::new(ConnectionPool::new());

    // Pre-populate with some entries
    let mut session_ids = vec![];
    let mut conn_ids = vec![];
    for i in 0..100 {
        let addr = test_addr(i as u8, 10000 + i as u16);
        session_ids.push(registry.register(addr, None));
        conn_ids.push(pool.register(addr));
    }

    let barrier = Arc::new(Barrier::new(4));
    let mut handles = vec![];

    // Reader thread 1 - query active counts
    {
        let registry = Arc::clone(&registry);
        let pool = Arc::clone(&pool);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..1000 {
                let _ = registry.active_count();
                let _ = pool.active_count();
                let _ = registry.count();
                let _ = pool.count();
            }
        }));
    }

    // Reader thread 2 - query individual entries
    {
        let registry = Arc::clone(&registry);
        let pool = Arc::clone(&pool);
        let session_ids = session_ids.clone();
        let conn_ids = conn_ids.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for i in 0..1000 {
                let idx = i % session_ids.len();
                let _ = registry.get(&session_ids[idx]);
                let _ = pool.get(&conn_ids[idx]);
            }
        }));
    }

    // Writer thread 1 - update states
    {
        let registry = Arc::clone(&registry);
        let pool = Arc::clone(&pool);
        let session_ids = session_ids.clone();
        let conn_ids = conn_ids.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for i in 0..1000 {
                let idx = i % session_ids.len();
                registry.set_state(
                    &session_ids[idx],
                    if i % 2 == 0 {
                        SessionState::Transferring
                    } else {
                        SessionState::Listing
                    },
                );
                pool.add_bytes(&conn_ids[idx], 10, 5);
            }
        }));
    }

    // Writer thread 2 - update modules
    {
        let registry = Arc::clone(&registry);
        let pool = Arc::clone(&pool);
        let session_ids = session_ids.clone();
        let conn_ids = conn_ids.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for i in 0..1000 {
                let idx = i % session_ids.len();
                registry.set_module(&session_ids[idx], format!("mod_{i}"));
                pool.set_module(&conn_ids[idx], format!("mod_{i}"));
            }
        }));
    }

    for handle in handles {
        handle.join().expect("thread should not panic");
    }

    // Verify data is still consistent
    assert_eq!(registry.count(), 100);
    assert_eq!(pool.count(), 100);
}

/// Test that module-based queries are consistent under concurrent access.
#[test]
fn concurrent_module_queries() {
    let registry = Arc::new(SessionRegistry::new());
    let pool = Arc::new(ConnectionPool::new());
    let total_registered = Arc::new(AtomicU64::new(0));

    let barrier = Arc::new(Barrier::new(6));
    let mut handles = vec![];

    // Register threads
    for thread_id in 0..3 {
        let registry = Arc::clone(&registry);
        let pool = Arc::clone(&pool);
        let total = Arc::clone(&total_registered);
        let barrier = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            barrier.wait();
            let module = format!("module_{thread_id}");
            for i in 0..200 {
                let addr = test_addr((thread_id * 50 + i / 4) as u8, (thread_id * 1000 + i) as u16);
                let session_id = registry.register(addr, None);
                let conn_id = pool.register(addr);

                registry.set_module(&session_id, module.clone());
                pool.set_module(&conn_id, module.clone());

                total.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    // Query threads
    for _ in 0..3 {
        let registry = Arc::clone(&registry);
        let pool = Arc::clone(&pool);
        let barrier = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..500 {
                for module_id in 0..3 {
                    let module = format!("module_{module_id}");
                    let _ = registry.sessions_for_module(&module);
                    let _ = pool.connections_for_module(&module);
                    let _ = registry.module_session_count(&module);
                    let _ = pool.module_connection_count(&module);
                }
            }
        }));
    }

    for handle in handles {
        handle.join().expect("thread should not panic");
    }

    // Verify final counts
    assert_eq!(registry.count(), 600);
    assert_eq!(pool.count(), 600);

    // Each module should have 200 entries
    for module_id in 0..3 {
        let module = format!("module_{module_id}");
        assert_eq!(registry.module_session_count(&module), 200);
        assert_eq!(pool.module_connection_count(&module), 200);
    }
}

/// Test per-IP rate limiting under concurrent access.
#[test]
fn concurrent_ip_rate_limiting() {
    let pool = Arc::new(ConnectionPool::new());
    let barrier = Arc::new(Barrier::new(10));
    let mut handles = vec![];

    // All threads register connections from the same IP
    let shared_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

    for thread_id in 0..10 {
        let pool = Arc::clone(&pool);
        let barrier = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            barrier.wait();
            for i in 0..100 {
                let addr = SocketAddr::new(shared_ip, (thread_id * 1000 + i) as u16);
                let id = pool.register(addr);
                pool.add_bytes(&id, 50, 25);
                // Don't unregister - we want to test accumulation
            }
        }));
    }

    for handle in handles {
        handle.join().expect("thread should not panic");
    }

    // All 1000 connections should be from the same IP
    assert_eq!(pool.connections_from_ip(&shared_ip), 1000);
    assert_eq!(pool.count(), 1000);
    assert_eq!(pool.unique_ip_count(), 1);

    // Check IP stats
    let stats = pool.get_ip_stats(&shared_ip).expect("stats should exist");
    assert_eq!(stats.active_connections, 1000);
    assert_eq!(stats.total_connections, 1000);
}

/// Test pruning operations under concurrent access.
#[test]
fn concurrent_pruning() {
    let registry = Arc::new(SessionRegistry::new());
    let pool = Arc::new(ConnectionPool::new());

    // Register entries and mark half as completed/inactive
    let mut session_ids = vec![];
    let mut conn_ids = vec![];
    for i in 0..200 {
        let addr = test_addr(i as u8, 10000 + i as u16);
        let session_id = registry.register(addr, None);
        let conn_id = pool.register(addr);

        if i % 2 == 0 {
            registry.set_state(&session_id, SessionState::Completed);
            pool.set_active(&conn_id, false);
        }

        session_ids.push(session_id);
        conn_ids.push(conn_id);
    }

    let barrier = Arc::new(Barrier::new(4));
    let mut handles = vec![];

    // Prune threads
    {
        let registry = Arc::clone(&registry);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..50 {
                let _ = registry.prune_inactive();
                thread::yield_now();
            }
        }));
    }

    {
        let pool = Arc::clone(&pool);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..50 {
                let _ = pool.prune_inactive();
                thread::yield_now();
            }
        }));
    }

    // Query threads during pruning
    {
        let registry = Arc::clone(&registry);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..500 {
                let _ = registry.active_sessions();
                let _ = registry.count();
            }
        }));
    }

    {
        let pool = Arc::clone(&pool);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..500 {
                let _ = pool.active_connections();
                let _ = pool.count();
            }
        }));
    }

    for handle in handles {
        handle.join().expect("thread should not panic");
    }

    // Only active entries should remain (half were marked inactive)
    assert_eq!(registry.count(), 100);
    assert_eq!(pool.count(), 100);

    // All remaining should be active
    assert_eq!(registry.active_count(), 100);
    assert_eq!(pool.active_count(), 100);
}

/// Test that byte counter updates are thread-safe.
#[test]
fn concurrent_byte_updates() {
    let pool = Arc::new(ConnectionPool::new());
    let addr = test_addr(1, 12345);
    let conn_id = pool.register(addr);

    let barrier = Arc::new(Barrier::new(10));
    let mut handles = vec![];

    // 10 threads each add 1000 bytes received and 500 sent
    for _ in 0..10 {
        let pool = Arc::clone(&pool);
        let conn_id = conn_id;
        let barrier = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..1000 {
                pool.add_bytes(&conn_id, 1, 0);
            }
            for _ in 0..500 {
                pool.add_bytes(&conn_id, 0, 1);
            }
        }));
    }

    for handle in handles {
        handle.join().expect("thread should not panic");
    }

    let info = pool.get(&conn_id).expect("connection should exist");
    assert_eq!(info.bytes_received, 10_000); // 10 threads * 1000 bytes
    assert_eq!(info.bytes_sent, 5_000); // 10 threads * 500 bytes
}

/// Test aggregate statistics computation under concurrent updates.
#[test]
fn concurrent_aggregate_stats() {
    let pool = Arc::new(ConnectionPool::new());
    let registry = Arc::new(SessionRegistry::new());

    let barrier = Arc::new(Barrier::new(8));
    let mut handles = vec![];

    // 4 threads registering connections
    for thread_id in 0..4 {
        let pool = Arc::clone(&pool);
        let registry = Arc::clone(&registry);
        let barrier = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            barrier.wait();
            for i in 0..100 {
                let addr = test_addr((thread_id * 64 + i / 4) as u8, (thread_id * 1000 + i) as u16);
                let conn_id = pool.register(addr);
                let session_id = registry.register(addr, None);

                pool.add_bytes(&conn_id, 100, 50);
                registry.add_bytes(&session_id, 100, 50);
            }
        }));
    }

    // 4 threads querying stats
    for _ in 0..4 {
        let pool = Arc::clone(&pool);
        let registry = Arc::clone(&registry);
        let barrier = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..500 {
                let stats = pool.aggregate_stats();
                // Stats should be internally consistent
                assert!(stats.active_connections <= stats.total_connections);

                let _ = registry.active_count();
                let _ = registry.count();
            }
        }));
    }

    for handle in handles {
        handle.join().expect("thread should not panic");
    }

    // Final verification
    let stats = pool.aggregate_stats();
    assert_eq!(stats.total_connections, 400); // 4 threads * 100 connections
    assert_eq!(stats.active_connections, 400);
    assert_eq!(stats.total_bytes_received, 40_000); // 400 * 100 bytes
    assert_eq!(stats.total_bytes_sent, 20_000); // 400 * 50 bytes
}

/// Test edge case: concurrent registration with same address.
#[test]
fn concurrent_same_address_registration() {
    let pool = Arc::new(ConnectionPool::new());
    let addr = test_addr(100, 12345);

    let barrier = Arc::new(Barrier::new(10));
    let mut handles = vec![];
    let registered_count = Arc::new(AtomicU64::new(0));

    // Multiple threads try to register the exact same address
    for _ in 0..10 {
        let pool = Arc::clone(&pool);
        let barrier = Arc::clone(&barrier);
        let count = Arc::clone(&registered_count);

        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..100 {
                let _id = pool.register(addr);
                count.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for handle in handles {
        handle.join().expect("thread should not panic");
    }

    // All registrations should succeed (each gets unique ID)
    assert_eq!(pool.count(), 1000);
    assert_eq!(registered_count.load(Ordering::Relaxed), 1000);

    // All from same IP
    assert_eq!(pool.connections_from_ip(&addr.ip()), 1000);
}
