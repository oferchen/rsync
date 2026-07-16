//! Benchmarks for daemon mode authentication and secrets-file handling.
//!
//! Run with: `cargo bench -p daemon --bench daemon_benchmark`

use std::net::IpAddr;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

use daemon::auth::{
    ChallengeGenerator, DaemonAuthDigest, SecretsFile, compute_auth_response,
    verify_client_response,
};

/// Generates a secrets file string with `n` user entries.
fn generate_secrets(user_count: usize) -> String {
    let mut content = String::with_capacity(user_count * 40);
    content.push_str("# Daemon secrets file\n");
    for i in 0..user_count {
        content.push_str(&format!("user_{i}:password_{i}_secret\n"));
    }
    content
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

criterion_group!(benches, bench_auth_challenge_response, bench_secrets_file,);
criterion_main!(benches);
