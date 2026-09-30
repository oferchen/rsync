//! Spike harness: plain READ vs fixed, watermark and arena registered-buffer
//! pools, each read through the same consumer. Linux io_uring only.
//!
//! ```text
//! iou_wm gen <root>
//! iou_wm run <A|B|B8M|C|D|E|Ef> <workload dir> <none|hash> [--max=auto|0|SIZE] [--k=K] [--digests=PATH]
//! iou_wm resize-bench
//! ```

#[cfg(target_os = "linux")]
mod bench {
    use std::fs::{self, File};
    use std::hint::black_box;
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use checksums::RollingChecksum;
    use checksums::strong::Xxh3_128;
    use fast_io::io_uring::per_thread_ring::{
        init_thread_sparse_buffers, resize_thread_buffers, thread_buffer_snapshot,
    };
    use fast_io::io_uring::registered_buffers::{ArenaFile, ArenaRing};
    use fast_io::{IoUringConfig, IoUringReader};

    const SLOT: usize = 64 << 10;
    const PLAIN_BUF: usize = 256 << 10;
    const MAX_SLOTS: usize = 1024;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn range(&mut self, lo: u64, hi: u64) -> u64 {
            lo + self.next() % (hi - lo)
        }
    }

    fn write_file(path: &Path, len: usize, seed: u64) {
        let mut rng = Rng(seed | 1);
        let mut f = File::create(path).unwrap();
        let mut block = vec![0u8; (1 << 20).min(len.max(1))];
        let mut left = len;
        while left > 0 {
            let n = left.min(block.len());
            for w in block[..n].chunks_mut(8) {
                let v = rng.next().to_le_bytes();
                w.copy_from_slice(&v[..w.len()]);
            }
            f.write_all(&block[..n]).unwrap();
            left -= n;
        }
    }

    fn write_workload(dir: &Path, sizes: &[usize], seed: u64) {
        fs::create_dir_all(dir).unwrap();
        let mut manifest = String::new();
        for (i, &len) in sizes.iter().enumerate() {
            let name = format!("f{i:05}");
            write_file(
                &dir.join(&name),
                len,
                seed.wrapping_mul(1_000_003) + i as u64,
            );
            manifest.push_str(&format!("{name}\t{len}\n"));
        }
        fs::write(dir.join("manifest.txt"), manifest).unwrap();
    }

    pub fn generate(root: &Path) {
        const K: usize = 1 << 10;
        const M: usize = 1 << 20;
        write_workload(&root.join("W1"), &[16 * K; 20_000], 1);
        write_workload(&root.join("W2"), &[256 * M; 8], 2);
        let mut rng = Rng(0x5eed_0003);
        let mut w3: Vec<usize> = (0..360)
            .map(|_| rng.range(1, 64 * K as u64) as usize)
            .collect();
        w3.extend((0..36).map(|_| rng.range(M as u64, 16 * M as u64 + 1) as usize));
        w3.extend([256 * M, 384 * M, 512 * M, 768 * M]);
        for i in (1..w3.len()).rev() {
            w3.swap(i, rng.range(0, i as u64 + 1) as usize);
        }
        write_workload(&root.join("W3"), &w3, 3);
        let w4: Vec<usize> = (0..40)
            .flat_map(|_| std::iter::once(32 * M).chain(std::iter::repeat_n(4 * K, 50)))
            .collect();
        write_workload(&root.join("W4"), &w4, 4);
    }

    struct Consumer {
        hash: bool,
        rolling: RollingChecksum,
        strong: Xxh3_128,
    }
    impl Consumer {
        fn new(hash: bool) -> Self {
            Self {
                hash,
                rolling: RollingChecksum::new(),
                strong: Xxh3_128::new(0),
            }
        }
        #[inline]
        fn eat(&mut self, b: &[u8]) {
            if self.hash {
                self.rolling.update(b);
                self.strong.update(b);
            } else {
                black_box(b);
            }
        }
        fn finish(self) -> Option<(u32, [u8; 16])> {
            self.hash
                .then(|| (self.rolling.value(), self.strong.finalize()))
        }
    }

    fn memlock_bytes() -> usize {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit writes into the provided struct.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut lim) },
            0
        );
        lim.rlim_cur as usize
    }

    /// Ceiling in bytes; `None` means no limit (`--max=0`).
    fn parse_max(arg: &str, rings: usize) -> Option<usize> {
        match arg {
            // One slot of headroom: the ring's own SQ/CQ pages are charged to
            // the same per-user RLIMIT_MEMLOCK budget as registered buffers.
            "auto" => Some((memlock_bytes() / rings).saturating_sub(SLOT)),
            "0" => None,
            s => {
                let (num, mul) = match s.as_bytes().last() {
                    Some(b'K' | b'k') => (&s[..s.len() - 1], 1 << 10),
                    Some(b'M' | b'm') => (&s[..s.len() - 1], 1 << 20),
                    Some(b'G' | b'g') => (&s[..s.len() - 1], 1 << 30),
                    _ => (s, 1),
                };
                Some(num.parse::<usize>().unwrap() * mul)
            }
        }
    }

    #[derive(Default)]
    struct Counters {
        grows: u64,
        shrinks: u64,
        resize_failures: u64,
        fallbacks: u64,
        peak_registered: usize,
    }

    /// Reads `path` through the thread's registered group with leases,
    /// falling back to plain reads when no lease is available.
    fn read_leased(r: &mut IoUringReader, buf: &mut [u8], c: &mut Consumer, n: &mut Counters) {
        loop {
            match r.read_lease(usize::MAX).unwrap() {
                Some(lease) if lease.is_empty() => return,
                Some(lease) => lease.chunks().for_each(|ch| c.eat(ch)),
                None => {
                    n.fallbacks += 1;
                    let got = r.read(buf).unwrap();
                    if got == 0 {
                        return;
                    }
                    c.eat(&buf[..got]);
                }
            }
        }
    }

    pub fn run(args: &[String]) {
        let arm = args[0].as_str();
        let dir = PathBuf::from(&args[1]);
        let hash = args[2] == "hash";
        let opt = |key: &str| {
            args.iter()
                .find_map(|a| a.strip_prefix(&format!("--{key}=")).map(str::to_owned))
        };
        let max_arg = opt("max").unwrap_or_else(|| "auto".into());
        let ceiling = parse_max(&max_arg, 1);
        let k: u32 = opt("k").map_or(64, |v| v.parse().unwrap());
        let ceiling_slots = ceiling.map_or(MAX_SLOTS, |b| (b / SLOT).clamp(1, MAX_SLOTS));
        let entries: Vec<(PathBuf, usize)> = fs::read_to_string(dir.join("manifest.txt"))
            .unwrap()
            .lines()
            .map(|l| {
                let (name, len) = l.split_once('\t').unwrap();
                (dir.join(name), len.parse().unwrap())
            })
            .collect();
        let slots_for = |len: usize| len.div_ceil(SLOT).clamp(1, ceiling_slots);

        let mut n = Counters::default();
        let mut digests = Vec::with_capacity(if hash { entries.len() } else { 0 });
        let mut buf = vec![0u8; PLAIN_BUF];
        let t = Instant::now();

        let (register_calls, read_fixed) = if arm.starts_with('E') {
            let mut ring =
                ArenaRing::new(ceiling_slots * SLOT, SLOT, 64, arm == "Ef").expect("arena");
            n.peak_registered = ring.len();
            for (path, len) in &entries {
                let mut c = Consumer::new(hash);
                let file = ring.open(path).unwrap();
                if let ArenaFile::Plain(f) = &file {
                    // Same per-file fstat as IoUringReader::open in arms A-D.
                    black_box(f.metadata().unwrap().len());
                }
                let mut off = 0usize;
                while off < *len {
                    let lease = ring.read_lease(&file, off as u64, len - off).unwrap();
                    if lease.is_empty() {
                        break;
                    }
                    off += lease.len();
                    c.eat(lease);
                }
                digests.extend(c.finish());
            }
            let s = ring.stats();
            (s.register_calls, s.read_fixed_sqes)
        } else {
            let watermark = matches!(arm, "C" | "D");
            let config = IoUringConfig {
                register_buffers: arm != "A",
                registered_buffer_count: if arm == "B8M" { ceiling_slots } else { 8 },
                ..IoUringConfig::default()
            };
            let mut cur = entries.first().map_or(1, |(_, l)| slots_for(*l));
            if watermark {
                init_thread_sparse_buffers(SLOT, ceiling_slots, cur).expect("sparse group");
            }
            let mut streak = 0u32;
            for (path, len) in &entries {
                if watermark {
                    let need = slots_for(*len);
                    let target = if need > cur {
                        streak = 0;
                        need
                    } else if need < cur {
                        streak += 1;
                        if arm == "C" || streak >= k {
                            streak = 0;
                            need
                        } else {
                            cur
                        }
                    } else {
                        streak = 0;
                        cur
                    };
                    if target != cur {
                        match resize_thread_buffers(target) {
                            Ok(()) if target > cur => n.grows += 1,
                            Ok(()) => n.shrinks += 1,
                            Err(_) => n.resize_failures += 1,
                        }
                        if let Some(s) = thread_buffer_snapshot() {
                            cur = s.count;
                        }
                    }
                }
                let mut c = Consumer::new(hash);
                let mut r = IoUringReader::open(path, &config).unwrap();
                assert_eq!(
                    r.registered_buffer_status().is_enabled(),
                    arm != "A",
                    "arm {arm}: {:?}",
                    r.registered_buffer_status()
                );
                if arm == "A" {
                    loop {
                        let got = r.read(&mut buf).unwrap();
                        if got == 0 {
                            break;
                        }
                        c.eat(&buf[..got]);
                    }
                } else {
                    read_leased(&mut r, &mut buf, &mut c, &mut n);
                }
                if let Some(s) = thread_buffer_snapshot() {
                    n.peak_registered = n.peak_registered.max(s.count * s.buffer_size);
                }
                digests.extend(c.finish());
            }
            thread_buffer_snapshot().map_or((0, 0), |s| {
                (
                    s.register_calls,
                    s.stats.total_acquires - s.stats.total_misses,
                )
            })
        };
        let wall = t.elapsed().as_secs_f64();
        let bytes: usize = entries.iter().map(|(_, l)| l).sum();
        let workload = dir.file_name().unwrap().to_string_lossy();
        let consumer = if hash { "hash" } else { "none" };
        println!(
            "RESULT arm={arm} workload={workload} consumer={consumer} max={max_arg} k={k} \
             wall_s={wall:.4} files={} bytes={bytes} grows={} shrinks={} resize_failures={} \
             register_calls={register_calls} peak_registered={} fallbacks={} read_fixed={read_fixed}",
            entries.len(),
            n.grows,
            n.shrinks,
            n.resize_failures,
            n.peak_registered,
            n.fallbacks,
        );
        if let Some(path) = opt("digests") {
            let text: String = digests
                .iter()
                .map(|(r, s)| {
                    let hex: String = s.iter().map(|b| format!("{b:02x}")).collect();
                    format!("{r:08x} {hex}\n")
                })
                .collect();
            fs::write(path, text).unwrap();
        }
    }

    fn median_p90(mut v: Vec<f64>) -> (f64, f64) {
        v.sort_by(f64::total_cmp);
        (v[v.len() / 2], v[v.len() * 9 / 10])
    }

    fn bench_mechanism(sparse: bool, small: usize, large: usize) -> String {
        std::thread::spawn(move || {
            if sparse {
                init_thread_sparse_buffers(SLOT, large, small).expect("sparse group");
            } else {
                let config = IoUringConfig {
                    register_buffers: true,
                    registered_buffer_count: small,
                    ..IoUringConfig::default()
                };
                let r = IoUringReader::open("/proc/self/exe", &config).unwrap();
                assert!(r.registered_buffer_count().is_some(), "dense group refused");
            }
            let (mut grow, mut shrink) = (Vec::new(), Vec::new());
            let mut failures = Vec::new();
            for i in 0..200 {
                let t = Instant::now();
                if let Err(e) = resize_thread_buffers(large) {
                    failures.push(format!("#{i}:{e}"));
                    continue;
                }
                grow.push(t.elapsed().as_secs_f64() * 1e6);
                let t = Instant::now();
                resize_thread_buffers(small).expect("shrink");
                shrink.push(t.elapsed().as_secs_f64() * 1e6);
            }
            let calls = thread_buffer_snapshot().unwrap().register_calls;
            let fails = failures.len();
            let first = failures.first().cloned().unwrap_or_default();
            if grow.is_empty() {
                return format!(
                    "RESIZE slots={small}<->{large} grow_failures={fails} first={first}"
                );
            }
            let (g50, g90) = median_p90(grow);
            let (s50, s90) = median_p90(shrink);
            format!(
                "RESIZE mech={} slots={small}<->{large} bytes={}KiB<->{}KiB \
                 grow_us_p50={g50:.1} grow_us_p90={g90:.1} shrink_us_p50={s50:.1} \
                 shrink_us_p90={s90:.1} register_calls={calls} grow_failures={fails} first={first}",
                if sparse {
                    "sparse_update"
                } else {
                    "unregister_register"
                },
                (small * SLOT) >> 10,
                (large * SLOT) >> 10,
            )
        })
        .join()
        .unwrap()
    }

    pub fn resize_bench() {
        println!("memlock_bytes={}", memlock_bytes());
        // 127 slots is the largest pool the 8 MiB budget admits next to the
        // ring's own pages.
        for (small, large) in [(1, 2), (1, 8), (1, 32), (1, 127), (64, 127)] {
            for sparse in [true, false] {
                // A closed ring releases its pinned pages asynchronously, so
                // let the previous thread's ring drain out of the budget.
                std::thread::sleep(std::time::Duration::from_secs(1));
                println!("{}", bench_mechanism(sparse, small, large));
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("gen") => bench::generate(std::path::Path::new(&args[1])),
        Some("run") => bench::run(&args[1..]),
        Some("resize-bench") => bench::resize_bench(),
        _ => eprintln!("usage: iou_wm gen <root> | run <arm> <dir> <none|hash> | resize-bench"),
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("iou_wm needs Linux io_uring");
}
