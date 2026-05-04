use super::*;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU32;

fn test_addr(ip_last: u8, port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, ip_last)), port)
}

#[test]
fn register_creates_connection() {
    let pool = ConnectionPool::new();
    let id = pool.register(test_addr(100, 1234));

    assert_eq!(pool.count(), 1);
    let info = pool.get(&id).unwrap();
    assert_eq!(info.peer_addr, test_addr(100, 1234));
    assert!(info.active);
    assert!(info.module.is_none());
}

#[test]
fn unregister_removes_connection() {
    let pool = ConnectionPool::new();
    let id = pool.register(test_addr(100, 1234));

    assert_eq!(pool.count(), 1);
    let removed = pool.unregister(&id);
    assert!(removed.is_some());
    assert_eq!(pool.count(), 0);
}

#[test]
fn tracks_per_ip_connections() {
    let pool = ConnectionPool::new();
    let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));

    let id1 = pool.register(SocketAddr::new(ip, 1234));
    let id2 = pool.register(SocketAddr::new(ip, 1235));
    let id3 = pool.register(SocketAddr::new(ip, 1236));

    assert_eq!(pool.connections_from_ip(&ip), 3);

    pool.unregister(&id2);
    assert_eq!(pool.connections_from_ip(&ip), 2);

    pool.unregister(&id1);
    pool.unregister(&id3);
    assert_eq!(pool.connections_from_ip(&ip), 0);
}

#[test]
fn would_exceed_ip_limit_works() {
    let pool = ConnectionPool::new();
    let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    let limit = NonZeroU32::new(3).unwrap();

    assert!(!pool.would_exceed_ip_limit(&ip, limit));

    pool.register(SocketAddr::new(ip, 1000));
    pool.register(SocketAddr::new(ip, 1001));
    assert!(!pool.would_exceed_ip_limit(&ip, limit));

    pool.register(SocketAddr::new(ip, 1002));
    assert!(pool.would_exceed_ip_limit(&ip, limit));
}

#[test]
fn set_module_updates_connection() {
    let pool = ConnectionPool::new();
    let id = pool.register(test_addr(100, 1234));

    assert!(pool.set_module(&id, "documents".to_string()));
    let info = pool.get(&id).unwrap();
    assert_eq!(info.module, Some("documents".to_string()));
}

#[test]
fn add_bytes_accumulates() {
    let pool = ConnectionPool::new();
    let id = pool.register(test_addr(100, 1234));

    pool.add_bytes(&id, 100, 50);
    pool.add_bytes(&id, 200, 100);

    let info = pool.get(&id).unwrap();
    assert_eq!(info.bytes_received, 300);
    assert_eq!(info.bytes_sent, 150);
}

#[test]
fn connections_for_module_filters_correctly() {
    let pool = ConnectionPool::new();
    let id1 = pool.register(test_addr(100, 1234));
    let id2 = pool.register(test_addr(101, 1235));
    let id3 = pool.register(test_addr(102, 1236));

    pool.set_module(&id1, "docs".to_string());
    pool.set_module(&id2, "docs".to_string());
    pool.set_module(&id3, "photos".to_string());

    let docs_connections = pool.connections_for_module("docs");
    assert_eq!(docs_connections.len(), 2);
    assert_eq!(pool.module_connection_count("docs"), 2);
    assert_eq!(pool.module_connection_count("photos"), 1);
    assert_eq!(pool.module_connection_count("other"), 0);
}

#[test]
fn prune_inactive_removes_only_inactive() {
    let pool = ConnectionPool::new();
    let id1 = pool.register(test_addr(100, 1234));
    let id2 = pool.register(test_addr(101, 1235));
    let id3 = pool.register(test_addr(102, 1236));

    pool.set_active(&id2, false);
    pool.set_active(&id3, false);

    let removed = pool.prune_inactive();
    assert_eq!(removed, 2);
    assert_eq!(pool.count(), 1);
    assert!(pool.get(&id1).is_some());
    assert!(pool.get(&id2).is_none());
    assert!(pool.get(&id3).is_none());
}

#[test]
fn unique_ids_across_registrations() {
    let pool = ConnectionPool::new();
    let id1 = pool.register(test_addr(100, 1234));
    let id2 = pool.register(test_addr(101, 1235));
    let id3 = pool.register(test_addr(102, 1236));

    assert_ne!(id1, id2);
    assert_ne!(id2, id3);
    assert_ne!(id1, id3);
}

#[test]
fn aggregate_stats_computes_correctly() {
    let pool = ConnectionPool::new();
    let id1 = pool.register(test_addr(100, 1234));
    let id2 = pool.register(test_addr(100, 1235));
    let id3 = pool.register(test_addr(101, 1236));

    pool.add_bytes(&id1, 100, 50);
    pool.add_bytes(&id2, 200, 100);
    pool.add_bytes(&id3, 300, 150);
    pool.set_active(&id2, false);

    let stats = pool.aggregate_stats();
    assert_eq!(stats.total_connections, 3);
    assert_eq!(stats.active_connections, 2);
    assert_eq!(stats.unique_ips, 2);
    assert_eq!(stats.total_bytes_received, 600);
    assert_eq!(stats.total_bytes_sent, 300);
}

#[test]
fn concurrent_access_is_safe() {
    use std::sync::Arc;
    use std::thread;

    let pool = Arc::new(ConnectionPool::new());
    let mut handles = vec![];

    for i in 0..10 {
        let pool = Arc::clone(&pool);
        let handle = thread::spawn(move || {
            for j in 0..100 {
                let port = (i * 1000 + j) as u16;
                let id = pool.register(test_addr((i % 256) as u8, port));
                pool.set_module(&id, format!("module_{i}"));
                pool.add_bytes(&id, 100, 50);
                pool.set_active(&id, false);
                pool.unregister(&id);
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(pool.count(), 0);
}

#[test]
fn ip_stats_accumulate_after_disconnect() {
    let pool = ConnectionPool::new();
    let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));

    let id1 = pool.register(SocketAddr::new(ip, 1000));
    pool.add_bytes(&id1, 100, 50);
    pool.unregister(&id1);

    let id2 = pool.register(SocketAddr::new(ip, 1001));
    pool.add_bytes(&id2, 200, 100);
    pool.unregister(&id2);

    let stats = pool.get_ip_stats(&ip).unwrap();
    assert_eq!(stats.active_connections, 0);
    assert_eq!(stats.total_connections, 2);
    assert_eq!(stats.bytes_received, 300);
    assert_eq!(stats.bytes_sent, 150);
}

#[test]
fn connection_pool_new_is_empty() {
    let pool = ConnectionPool::new();
    assert_eq!(pool.count(), 0);
    assert_eq!(pool.active_count(), 0);
}

#[test]
fn connection_pool_with_capacity() {
    let pool = ConnectionPool::with_capacity(100);
    assert_eq!(pool.count(), 0);
}

#[test]
fn connection_pool_default() {
    let pool = ConnectionPool::default();
    assert_eq!(pool.count(), 0);
}

#[test]
fn connection_pool_debug() {
    let pool = ConnectionPool::new();
    let debug = format!("{pool:?}");
    assert!(debug.contains("ConnectionPool"));
}

#[test]
fn connection_id_as_u64() {
    let pool = ConnectionPool::new();
    let id = pool.register(test_addr(100, 1234));
    assert!(id.as_u64() > 0);
}

#[test]
fn connection_id_display() {
    let pool = ConnectionPool::new();
    let id = pool.register(test_addr(100, 1234));
    let display = format!("{id}");
    assert!(!display.is_empty());
}

#[test]
fn connection_id_hash() {
    use std::collections::HashSet;
    let pool = ConnectionPool::new();
    let id1 = pool.register(test_addr(100, 1234));
    let id2 = pool.register(test_addr(101, 1235));
    let mut set = HashSet::new();
    set.insert(id1);
    set.insert(id2);
    assert_eq!(set.len(), 2);
}

#[test]
fn connection_info_duration() {
    let pool = ConnectionPool::new();
    let id = pool.register(test_addr(100, 1234));
    let info = pool.get(&id).unwrap();
    assert!(info.duration().as_secs() < 10);
}

#[test]
fn connection_info_clone() {
    let pool = ConnectionPool::new();
    let id = pool.register(test_addr(100, 1234));
    let info = pool.get(&id).unwrap();
    let cloned = info.clone();
    assert_eq!(info.id, cloned.id);
    assert_eq!(info.peer_addr, cloned.peer_addr);
}

#[test]
fn connection_info_debug() {
    let pool = ConnectionPool::new();
    let id = pool.register(test_addr(100, 1234));
    let info = pool.get(&id).unwrap();
    let debug = format!("{info:?}");
    assert!(debug.contains("ConnectionInfo"));
}

#[test]
fn ip_stats_default() {
    let stats = IpStats::default();
    assert_eq!(stats.active_connections, 0);
    assert_eq!(stats.total_connections, 0);
    assert_eq!(stats.bytes_received, 0);
    assert_eq!(stats.bytes_sent, 0);
}

#[test]
fn ip_stats_clone() {
    let stats = IpStats::default();
    let cloned = stats.clone();
    assert_eq!(stats.active_connections, cloned.active_connections);
}

#[test]
fn ip_stats_debug() {
    let stats = IpStats::default();
    let debug = format!("{stats:?}");
    assert!(debug.contains("IpStats"));
}

#[test]
fn aggregate_stats_default() {
    let stats = AggregateStats::default();
    assert_eq!(stats.total_connections, 0);
    assert_eq!(stats.active_connections, 0);
    assert_eq!(stats.unique_ips, 0);
    assert_eq!(stats.total_bytes_received, 0);
    assert_eq!(stats.total_bytes_sent, 0);
}

#[test]
fn aggregate_stats_clone() {
    let stats = AggregateStats::default();
    let cloned = stats.clone();
    assert_eq!(stats.total_connections, cloned.total_connections);
}

#[test]
fn aggregate_stats_debug() {
    let stats = AggregateStats::default();
    let debug = format!("{stats:?}");
    assert!(debug.contains("AggregateStats"));
}

#[test]
fn get_nonexistent_connection() {
    let pool = ConnectionPool::new();
    let fake_id = ConnectionId(999);
    assert!(pool.get(&fake_id).is_none());
}

#[test]
fn unregister_nonexistent_connection() {
    let pool = ConnectionPool::new();
    let fake_id = ConnectionId(999);
    assert!(pool.unregister(&fake_id).is_none());
}

#[test]
fn set_module_nonexistent_connection() {
    let pool = ConnectionPool::new();
    let fake_id = ConnectionId(999);
    assert!(!pool.set_module(&fake_id, "test".to_string()));
}

#[test]
fn set_active_nonexistent_connection() {
    let pool = ConnectionPool::new();
    let fake_id = ConnectionId(999);
    assert!(!pool.set_active(&fake_id, false));
}

#[test]
fn add_bytes_nonexistent_connection() {
    let pool = ConnectionPool::new();
    let fake_id = ConnectionId(999);
    assert!(!pool.add_bytes(&fake_id, 100, 50));
}

#[test]
fn connections_from_ip_unknown_ip() {
    let pool = ConnectionPool::new();
    let ip = IpAddr::V4(Ipv4Addr::new(10, 10, 10, 10));
    assert_eq!(pool.connections_from_ip(&ip), 0);
}

#[test]
fn get_ip_stats_unknown_ip() {
    let pool = ConnectionPool::new();
    let ip = IpAddr::V4(Ipv4Addr::new(10, 10, 10, 10));
    assert!(pool.get_ip_stats(&ip).is_none());
}

#[test]
fn active_connections_list() {
    let pool = ConnectionPool::new();
    let id1 = pool.register(test_addr(100, 1234));
    let id2 = pool.register(test_addr(101, 1235));
    pool.set_active(&id2, false);

    let active = pool.active_connections();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, id1);
}

#[test]
fn all_connections_list() {
    let pool = ConnectionPool::new();
    let _id1 = pool.register(test_addr(100, 1234));
    let id2 = pool.register(test_addr(101, 1235));
    pool.set_active(&id2, false);

    let all = pool.all_connections();
    assert_eq!(all.len(), 2);
}

#[test]
fn connections_from_addr() {
    let pool = ConnectionPool::new();
    let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
    pool.register(SocketAddr::new(ip, 1234));
    pool.register(SocketAddr::new(ip, 1235));
    pool.register(test_addr(101, 1236));

    let from_ip = pool.connections_from_addr(&ip);
    assert_eq!(from_ip.len(), 2);
}

#[test]
fn prune_stale_ip_stats() {
    let pool = ConnectionPool::new();
    let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
    let id = pool.register(SocketAddr::new(ip, 1234));
    pool.unregister(&id);

    assert!(pool.get_ip_stats(&ip).is_some());

    let pruned = pool.prune_stale_ip_stats();
    assert_eq!(pruned, 1);

    assert!(pool.get_ip_stats(&ip).is_none());
}

#[test]
fn unique_ip_count() {
    let pool = ConnectionPool::new();
    pool.register(test_addr(100, 1234));
    pool.register(test_addr(100, 1235));
    pool.register(test_addr(101, 1236));

    assert_eq!(pool.unique_ip_count(), 2);
}

#[test]
fn active_count() {
    let pool = ConnectionPool::new();
    let _id1 = pool.register(test_addr(100, 1234));
    let id2 = pool.register(test_addr(101, 1235));
    pool.set_active(&id2, false);

    assert_eq!(pool.active_count(), 1);
    assert_eq!(pool.count(), 2);
}
