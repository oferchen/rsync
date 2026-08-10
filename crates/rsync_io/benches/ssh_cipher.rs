//! crates/rsync_io/benches/ssh_cipher.rs
//!
//! Throughput comparison: AES-128-GCM vs AES-256-GCM vs ChaCha20-Poly1305 at
//! typical SSH packet sizes (16 KiB, 64 KiB, 1 MiB).
//!
//! The embedded SSH transport selects ciphers via
//! `rsync_io::ssh::embedded::cipher::default_ciphers`, preferring AES-GCM when
//! the CPU advertises hardware AES (AES-NI on x86_64, ARMv8 Crypto Extensions
//! on aarch64) and falling back to ChaCha20-Poly1305 otherwise. This benchmark
//! exercises the raw AEAD primitives `aes-gcm` and `chacha20poly1305` use so
//! the cipher-selection heuristic can be validated against measured throughput.
//!
//! # Hardware coverage on CI
//!
//! - **Linux x86_64 / Linux musl x86_64:** AES-NI is universally present, so
//!   AES-GCM wins by a large margin and the heuristic's "prefer AES-GCM" path
//!   matches reality.
//! - **macOS aarch64 (Apple Silicon):** ARMv8 Cryptography Extensions are
//!   always available, so AES-GCM again wins and the heuristic's preference
//!   is correct.
//! - **Windows x86_64:** Same as Linux x86_64 - AES-NI present.
//!
//! The case the heuristic is designed for - aarch64 without ARM AES crypto
//! extensions (low-end embedded boards, some virtualised guests) - is **not**
//! exercised by any GitHub Actions runner. Reproducing the "ChaCha20 wins"
//! result therefore requires running this benchmark on real hardware such as
//! a Raspberry Pi 3 / Pi Zero 2 or a cloud instance with AES extensions
//! disabled (`-cpu cortex-a53,-aes` under QEMU).
//!
//! # Forcing a pure-software path
//!
//! The `aes-gcm` and `chacha20poly1305` crates dispatch to hardware AES /
//! `vaes` automatically when the target architecture and CPU features allow.
//! There is no portable runtime switch to disable that dispatch. To exercise
//! the pure-software AES path the benchmark must be built and run on a host
//! without AES-NI / ARM AES, or under an emulator that does not expose those
//! features. The benchmark therefore reports whichever code path the crate's
//! own runtime detection selects on the host CPU.
//!
//! Run with:
//!
//! ```text
//! cargo bench -p rsync_io --bench ssh_cipher
//! ```

use std::hint::black_box;
use std::time::Duration;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Aes256Gcm, Key as AesKey, Nonce as AesNonce};
use chacha20poly1305::{ChaCha20Poly1305, Key as ChaChaKey, Nonce as ChaChaNonce};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rand::RngExt;

/// SSH packet sizes spanning the typical traffic distribution: 16 KiB matches
/// the default channel window granularity, 64 KiB is the maximum SSH packet,
/// and 1 MiB models bulk file transfer where the same key streams many
/// packets back-to-back.
const PACKET_SIZES: &[usize] = &[16 * 1024, 64 * 1024, 1024 * 1024];

/// Returns a buffer of `size` random bytes used as the plaintext input for
/// each benchmarked AEAD primitive. Uses the thread-local RNG so the input is
/// incompressible.
fn random_plaintext(size: usize) -> Vec<u8> {
    let mut buf = vec![0u8; size];
    rand::rng().fill(&mut buf[..]);
    buf
}

/// Returns `N` random bytes from the thread-local RNG. Used to build cipher
/// keys and nonces of arbitrary fixed size.
fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    rand::rng().fill(&mut buf[..]);
    buf
}

/// Benchmarks AES-128-GCM encrypt and decrypt at the configured packet sizes.
fn bench_aes_128_gcm(c: &mut Criterion) {
    let key_bytes: [u8; 16] = random_bytes();
    let key_array = AesKey::<Aes128Gcm>::try_from(&key_bytes[..]).expect("aes-128 key length");
    let key = &key_array;
    let cipher = Aes128Gcm::new(key);
    let nonce_bytes: [u8; 12] = random_bytes();
    let nonce_array = AesNonce::try_from(&nonce_bytes[..]).expect("aes nonce length");
    let nonce = &nonce_array;

    let mut encrypt = c.benchmark_group("aes_128_gcm/encrypt");
    for &size in PACKET_SIZES {
        let plaintext = random_plaintext(size);
        encrypt.throughput(Throughput::Bytes(size as u64));
        encrypt.bench_with_input(BenchmarkId::from_parameter(size), &plaintext, |b, pt| {
            b.iter(|| {
                let ct = cipher
                    .encrypt(nonce, black_box(pt.as_slice()))
                    .expect("aes-128-gcm encryption");
                black_box(ct);
            });
        });
    }
    encrypt.finish();

    let mut decrypt = c.benchmark_group("aes_128_gcm/decrypt");
    for &size in PACKET_SIZES {
        let plaintext = random_plaintext(size);
        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_slice())
            .expect("aes-128-gcm encryption for decrypt bench");
        decrypt.throughput(Throughput::Bytes(size as u64));
        decrypt.bench_with_input(BenchmarkId::from_parameter(size), &ciphertext, |b, ct| {
            b.iter(|| {
                let pt = cipher
                    .decrypt(nonce, black_box(ct.as_slice()))
                    .expect("aes-128-gcm decryption");
                black_box(pt);
            });
        });
    }
    decrypt.finish();
}

/// Benchmarks AES-256-GCM encrypt and decrypt at the configured packet sizes.
fn bench_aes_256_gcm(c: &mut Criterion) {
    let key_bytes: [u8; 32] = random_bytes();
    let key_array = AesKey::<Aes256Gcm>::try_from(&key_bytes[..]).expect("aes-256 key length");
    let key = &key_array;
    let cipher = Aes256Gcm::new(key);
    let nonce_bytes: [u8; 12] = random_bytes();
    let nonce_array = AesNonce::try_from(&nonce_bytes[..]).expect("aes nonce length");
    let nonce = &nonce_array;

    let mut encrypt = c.benchmark_group("aes_256_gcm/encrypt");
    for &size in PACKET_SIZES {
        let plaintext = random_plaintext(size);
        encrypt.throughput(Throughput::Bytes(size as u64));
        encrypt.bench_with_input(BenchmarkId::from_parameter(size), &plaintext, |b, pt| {
            b.iter(|| {
                let ct = cipher
                    .encrypt(nonce, black_box(pt.as_slice()))
                    .expect("aes-256-gcm encryption");
                black_box(ct);
            });
        });
    }
    encrypt.finish();

    let mut decrypt = c.benchmark_group("aes_256_gcm/decrypt");
    for &size in PACKET_SIZES {
        let plaintext = random_plaintext(size);
        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_slice())
            .expect("aes-256-gcm encryption for decrypt bench");
        decrypt.throughput(Throughput::Bytes(size as u64));
        decrypt.bench_with_input(BenchmarkId::from_parameter(size), &ciphertext, |b, ct| {
            b.iter(|| {
                let pt = cipher
                    .decrypt(nonce, black_box(ct.as_slice()))
                    .expect("aes-256-gcm decryption");
                black_box(pt);
            });
        });
    }
    decrypt.finish();
}

/// Benchmarks ChaCha20-Poly1305 encrypt and decrypt at the configured packet
/// sizes. This is the cipher the SSH layer prefers when the CPU lacks hardware
/// AES.
fn bench_chacha20_poly1305(c: &mut Criterion) {
    let key_bytes: [u8; 32] = random_bytes();
    let key_array = ChaChaKey::try_from(&key_bytes[..]).expect("chacha key length");
    let key = &key_array;
    let cipher = ChaCha20Poly1305::new(key);
    let nonce_bytes: [u8; 12] = random_bytes();
    let nonce_array = ChaChaNonce::try_from(&nonce_bytes[..]).expect("chacha nonce length");
    let nonce = &nonce_array;

    let mut encrypt = c.benchmark_group("chacha20_poly1305/encrypt");
    for &size in PACKET_SIZES {
        let plaintext = random_plaintext(size);
        encrypt.throughput(Throughput::Bytes(size as u64));
        encrypt.bench_with_input(BenchmarkId::from_parameter(size), &plaintext, |b, pt| {
            b.iter(|| {
                let ct = cipher
                    .encrypt(nonce, black_box(pt.as_slice()))
                    .expect("chacha20-poly1305 encryption");
                black_box(ct);
            });
        });
    }
    encrypt.finish();

    let mut decrypt = c.benchmark_group("chacha20_poly1305/decrypt");
    for &size in PACKET_SIZES {
        let plaintext = random_plaintext(size);
        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_slice())
            .expect("chacha20-poly1305 encryption for decrypt bench");
        decrypt.throughput(Throughput::Bytes(size as u64));
        decrypt.bench_with_input(BenchmarkId::from_parameter(size), &ciphertext, |b, ct| {
            b.iter(|| {
                let pt = cipher
                    .decrypt(nonce, black_box(ct.as_slice()))
                    .expect("chacha20-poly1305 decryption");
                black_box(pt);
            });
        });
    }
    decrypt.finish();
}

criterion_group! {
    name = ssh_cipher_benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(3));
    targets = bench_aes_128_gcm, bench_aes_256_gcm, bench_chacha20_poly1305
}
criterion_main!(ssh_cipher_benches);
