//! Benchmarks for daemon mode configuration, module lookup, authentication, and path validation.
//!
//! Run with: `cargo bench -p daemon --bench daemon_bench`

use std::net::IpAddr;
use std::path::Path;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

use daemon::auth::{
    ChallengeGenerator, DaemonAuthDigest, SecretsFile, compute_auth_response,
    verify_client_response,
};
use daemon::rsyncd_config::RsyncdConfig;

/// Generates an rsyncd.conf string with `n` modules.
///
/// Each module has a unique name, path, comment, and typical settings
/// including auth users, host ACLs, and filter rules to exercise a
/// realistic parsing workload.
fn generate_config(module_count: usize) -> String {
    let mut config = String::with_capacity(module_count * 256);

    // Global section
    config.push_str("port = 873\n");
    config.push_str("motd file = /etc/rsyncd.motd\n");
    config.push_str("log file = /var/log/rsyncd.log\n");
    config.push_str("pid file = /var/run/rsyncd.pid\n");
    config.push_str("uid = nobody\n");
    config.push_str("gid = nogroup\n");
    config.push('\n');

    for i in 0..module_count {
        config.push_str(&format!("[module_{i}]\n"));
        config.push_str(&format!("path = /data/module_{i}\n"));
        config.push_str(&format!("comment = Public files for module {i}\n"));
        config.push_str("read only = yes\n");
        config.push_str("list = yes\n");
        config.push_str("max connections = 10\n");
        config.push_str("timeout = 300\n");
        config.push_str("use chroot = yes\n");
        config.push_str(&format!("auth users = user_{i}\n"));
        config.push_str(&format!("secrets file = /etc/rsyncd.secrets.{i}\n"));
        config.push_str(&format!("hosts allow = 192.168.{}.0/24\n", i % 256));
        config.push_str("hosts deny = *\n");
        config.push_str(&format!("exclude = *.tmp .cache_{i}\n"));
        config.push_str(&format!("filter = - /secret_{i}/\n"));
        config.push('\n');
    }

    config
}

/// Generates a secrets file string with `n` user entries.
fn generate_secrets(user_count: usize) -> String {
    let mut content = String::with_capacity(user_count * 40);
    content.push_str("# Daemon secrets file\n");
    for i in 0..user_count {
        content.push_str(&format!("user_{i}:password_{i}_secret\n"));
    }
    content
}

/// Benchmarks parsing rsyncd.conf files with varying module counts.
fn bench_config_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("daemon_config_parse");
    let conf_path = Path::new("bench.conf");

    for module_count in [10, 50, 100] {
        let input = generate_config(module_count);
        group.bench_with_input(
            BenchmarkId::new("modules", module_count),
            &input,
            |b, input| {
                b.iter(|| RsyncdConfig::parse(input, conf_path).unwrap());
            },
        );
    }
    group.finish();
}

/// Benchmarks module lookup by name from parsed configs of varying sizes.
fn bench_module_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("daemon_module_lookup");
    let conf_path = Path::new("bench.conf");

    for module_count in [10, 50, 100] {
        let input = generate_config(module_count);
        let config = RsyncdConfig::parse(&input, conf_path).unwrap();

        // Look up a module near the middle of the list
        let target = format!("module_{}", module_count / 2);
        group.bench_with_input(
            BenchmarkId::new("modules", module_count),
            &target,
            |b, target| {
                b.iter(|| {
                    let result = config.get_module(target);
                    assert!(result.is_some());
                    result
                });
            },
        );
    }

    // Benchmark lookup miss
    let input = generate_config(100);
    let config = RsyncdConfig::parse(&input, conf_path).unwrap();
    group.bench_function("miss_100_modules", |b| {
        b.iter(|| {
            let result = config.get_module("nonexistent_module");
            assert!(result.is_none());
            result
        });
    });

    group.finish();
}

/// Benchmarks challenge generation and auth response computation across digest algorithms.
fn bench_auth_challenge_response(c: &mut Criterion) {
    let mut group = c.benchmark_group("daemon_auth_challenge_response");
    let peer_ip: IpAddr = "192.168.1.100".parse().unwrap();
    let password = b"benchmark_secret_password";

    // Benchmark challenge generation for modern and legacy protocols
    group.bench_function("challenge_gen_md5", |b| {
        b.iter(|| ChallengeGenerator::generate(peer_ip, Some(32)));
    });

    group.bench_function("challenge_gen_md4", |b| {
        b.iter(|| ChallengeGenerator::generate(peer_ip, Some(29)));
    });

    // Benchmark response computation for each digest algorithm
    let challenge = ChallengeGenerator::generate(peer_ip, Some(32));
    let digests = [
        ("sha512", DaemonAuthDigest::Sha512),
        ("sha256", DaemonAuthDigest::Sha256),
        ("sha1", DaemonAuthDigest::Sha1),
        ("md5", DaemonAuthDigest::Md5),
        ("md4", DaemonAuthDigest::Md4),
    ];

    for (name, digest) in digests {
        group.bench_function(BenchmarkId::new("compute", name), |b| {
            b.iter(|| compute_auth_response(password, &challenge, digest));
        });
    }

    // Benchmark full verify round-trip (compute + constant-time compare)
    let response_md5 = compute_auth_response(password, &challenge, DaemonAuthDigest::Md5);
    group.bench_function("verify_md5", |b| {
        b.iter(|| verify_client_response(password, &challenge, &response_md5, Some(32)));
    });

    let response_sha512 = compute_auth_response(password, &challenge, DaemonAuthDigest::Sha512);
    group.bench_function("verify_sha512", |b| {
        b.iter(|| verify_client_response(password, &challenge, &response_sha512, Some(32)));
    });

    group.finish();
}

/// Benchmarks secrets file parsing and user lookup with varying entry counts.
fn bench_secrets_file(c: &mut Criterion) {
    let mut group = c.benchmark_group("daemon_secrets_file");

    for user_count in [10, 100, 1000] {
        let content = generate_secrets(user_count);

        group.bench_with_input(
            BenchmarkId::new("parse", user_count),
            &content,
            |b, content| {
                b.iter(|| SecretsFile::parse(content).unwrap());
            },
        );

        let secrets = SecretsFile::parse(&content).unwrap();
        let target = format!("user_{}", user_count / 2);
        group.bench_with_input(
            BenchmarkId::new("lookup_hit", user_count),
            &target,
            |b, target| {
                b.iter(|| {
                    let result = secrets.lookup(target);
                    assert!(result.is_some());
                    result
                });
            },
        );

        group.bench_function(BenchmarkId::new("lookup_miss", user_count), |b| {
            b.iter(|| {
                let result = secrets.lookup("nonexistent_user");
                assert!(result.is_none());
                result
            });
        });
    }

    group.finish();
}

/// Benchmarks module identifier sanitization with clean and adversarial inputs.
fn bench_path_validation(c: &mut Criterion) {
    let mut group = c.benchmark_group("daemon_path_validation");
    let conf_path = Path::new("bench.conf");

    // Benchmark config parsing with adversarial module names containing special characters
    let adversarial_configs = [
        (
            "clean_names",
            "[mymodule]\npath = /data/mymodule\ncomment = Clean module\n",
        ),
        (
            "long_comment",
            &format!(
                "[longmod]\npath = /data/longmod\ncomment = {}\n",
                "A".repeat(4096)
            ),
        ),
        ("many_filters", &{
            let mut s = String::from("[filtered]\npath = /data/filtered\n");
            for i in 0..50 {
                s.push_str(&format!("filter = - /secret_{i}/**\n"));
                s.push_str(&format!("exclude = *.bak_{i}\n"));
                s.push_str(&format!("include = *.keep_{i}\n"));
            }
            s
        }),
        ("deep_host_acls", &{
            let mut s = String::from("[acl_heavy]\npath = /data/acl\n");
            s.push_str("hosts allow = ");
            let hosts: Vec<String> = (0..100)
                .map(|i| format!("10.{}.{}.0/24", i / 256, i % 256))
                .collect();
            s.push_str(&hosts.join(", "));
            s.push('\n');
            s
        }),
    ];

    for (name, input) in adversarial_configs {
        group.bench_function(BenchmarkId::new("parse", name), |b| {
            b.iter(|| RsyncdConfig::parse(input, conf_path).unwrap());
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_config_parse,
    bench_module_lookup,
    bench_auth_challenge_response,
    bench_secrets_file,
    bench_path_validation,
);
criterion_main!(benches);
