# RSS-4: arena/intern crate evaluation for `FileEntry` path storage

Task: RSS-4. Branch: `docs/rss-4-arena-allocator-eval`. Companion:
`docs/audits/rss-3-fileentry-size-breakdown.md` (the audit identifying
the per-entry `PathBuf` + `Arc<Path>` allocations as the dominant 3-11x
peak-RSS contributor). This document evaluates three off-the-shelf
allocator/interner crates so that RSS-5 can lock in a design starting
point.

The goal here is **selection criteria**, not design. RSS-5 will spec
the actual struct shape (`(start: u32, len: u32)` arena indices,
handle types, drop ordering, sender vs receiver split, etc.) on top
of whichever crate wins.

## What we are replacing

From `rss-3-fileentry-size-breakdown.md`, the three heap contributors
in priority order:

1. **`name: PathBuf`** - one heap allocation per entry, ~32 B size
   class for a 20-byte basename; ~32 MiB at 1 M entries (heap) plus
   24 B inline. **One `malloc`/entry, one `free`/entry.**
2. **`Vec<FileEntry>` inline footprint** - 88 B inline vs upstream's
   24 B header; ~61 MiB inline gap at 1 M. Replacing 24 B `PathBuf`
   with an 8 B `(u32, u32)` arena index shrinks the inline footprint
   by 16 B per entry on Unix (~16 MiB at 1 M).
3. **`dirname: Arc<Path>`** - small in bytes (interner-amortised at
   100 unique dirs) but each entry pays 16 B inline + an atomic RC
   bump on every clone/drop. Replacing with a small handle (`u32`
   index into a dirname table) eliminates both the fat pointer and
   the atomic traffic.

The replacement must work for **both** the reader path
(`crates/protocol/src/flist/intern.rs::PathInterner`, single-threaded
decode) and the direct constructor path
(`crates/protocol/src/flist/entry/constructors.rs`, which today does
not intern at all). It must also survive the rayon-parallel consumers
documented in `MEMORY.md` (e.g., `PARALLEL_STAT_THRESHOLD = 64`).

## Candidates

- **`bumpalo`** (https://crates.io/crates/bumpalo) - bump allocator.
- **`typed-arena`** (https://crates.io/crates/typed-arena) -
  single-typed arena.
- **`lasso`** (https://crates.io/crates/lasso) - dedup'd string
  interner with a thread-safe variant. (Stand-in for
  `string-interner` / `internment`; selected over `string-interner`
  because `lasso` ships a tested `ThreadedRodeo`, and over
  `internment` because `internment` leaks by default and is
  process-global.)

## Comparison table

| Criterion | bumpalo | typed-arena | lasso |
|---|---|---|---|
| Zero-copy lookup | `&'arena str` / `&'arena Path` returned at alloc time, no handle indirection | `&'arena T` returned at alloc time | `Spur` (`u32` handle) -> `&str` via `Rodeo::resolve()`; single hash-table indirection on resolve |
| Eliminates per-entry `name` malloc | Yes; one bump-pointer add per `alloc_slice_copy` | Yes; one bump-pointer add per `alloc` | Yes for **unique** basenames; **dedupes** repeated basenames (huge win when same filename appears in many dirs, e.g., `Cargo.toml`, `__init__.py`, `.gitignore`) |
| Eliminates per-entry `dirname` Arc malloc | Yes if dirnames are interned via a side `HashMap<Path, &'arena Path>`; otherwise dirnames get re-copied per entry | Same as bumpalo: needs an explicit interning map | Yes natively; `get_or_intern(dir)` is the primary API |
| `&str` / `&Path` returned has lifetime | `'arena` (tied to `&Bump`) | `'arena` (tied to `&Arena<T>`) | `'rodeo` from `resolve(&Spur)`; handle is `Copy + 'static` |
| Per-entry inline footprint after migration | 16 B (two `&'arena str` slices) or 8 B (offset/len pair) | Same as bumpalo (still a fat slice) | **8 B** for two `Spur` handles (4 B each); biggest inline-footprint win |
| Sync (cross-thread sharing of arena) | `Bump: !Sync`. Two patterns: per-thread bumps merged via reference, or `bumpalo-herd::Herd` (`Sync`) hands out per-thread bumps. Allocated `&'a T` is `Send + Sync` when `T: Send + Sync` | `Arena<T>: !Sync`. No first-party `Sync` herd. Need to wrap in `Mutex` or use one arena per worker | `Rodeo: !Sync` (insert side). **`ThreadedRodeo: Sync`** (sharded, lock-free reads, locked writes). Read-only `RodeoReader`/`RodeoResolver` are `Sync` |
| Compatible with `rayon::par_iter` | With caveats: receiver-side build is sequential decode (`Rodeo`/`Bump` fine); rayon consumers only **read** paths, so a frozen `&Bump` is `Send + Sync` if `T: Sync` | With caveats: same as bumpalo, but no herd helper; need explicit per-thread arenas | Cleanest: build with `ThreadedRodeo`, freeze to `RodeoReader` for parallel consumers; handles are `Copy + Send + Sync` |
| Drops cleanly at flist teardown | Yes: `drop(Bump)` frees all chunks in O(chunks). Allocated `T` must not have `Drop` impls (or `bump.alloc()` is forbidden for `T: Drop`); strings/paths/bytes are `Copy`-effectively, so trivially safe | Yes: `drop(Arena<T>)` walks chunks and calls `Drop` on each `T` (this is the typed-arena distinguishing feature). Slower than `Bump` for plain bytes | Yes: `drop(Rodeo)` frees the backing strings in one pass. Handles outliving the rodeo are a use-after-free guard (compile-time via lifetime on `resolve`) |
| Iteration order (matters for upstream wire-order replay) | Insertion order preserved within chunks; cross-chunk order not guaranteed without extra bookkeeping | Per-arena insertion order via `Arena::into_vec()` | `Rodeo::strings()` iterates in insertion order (interned-once order) |
| Dependency footprint | `bumpalo = "3"` - zero required deps; one optional feature (`allocator_api`, nightly) | `typed-arena = "2"` - zero deps | `lasso = "0.7"` - zero required deps; optional `hashbrown`, `ahash`, `serde`, `multi-threaded` features. **`multi-threaded` feature pulls `dashmap` + `parking_lot`** |
| MSRV | 1.71 (bumpalo 3.x) - under our 1.88 pin | 1.71 - under our 1.88 pin | 1.70 - under our 1.88 pin |
| Used by (risk signal) | rustc (`rustc_arena` is the upstream of bumpalo's design; `bumpalo` itself in wasmtime, gimli, salsa, cargo-rustdoc, cranelift) | rustc internals predate it; SpiderMonkey-Rust, lalrpop, polonius. Less actively maintained: last release 2.0.2 in 2023, but stable surface | Bevy ecosystem (`bevy_asset`), `polars`, `rust-analyzer` (similar pattern with their own arena, but lasso is the off-the-shelf equivalent). `multi-threaded` path is exercised by polars at scale |
| API surface complexity | Small (4 main methods: `alloc`, `alloc_slice_copy`, `alloc_str`, `reset`) | Smallest (`alloc`, `into_vec`) | Largest: `Spur`/`MiniSpur`/`LargeSpur`/`MicroSpur` handle types, `Rodeo`/`ThreadedRodeo`/`RodeoReader`/`RodeoResolver` lifecycle |
| Path-equality semantics | Pointer equality is NOT identity (two distinct allocs of "src/lib" are distinct `&Path`); need explicit interning layer for dirname dedup | Same as bumpalo | Pointer equality IS identity (handle equality = string equality); free dirname dedup |
| Per-entry RSS reduction estimate (1 M-file fixture, vanilla files) | ~32 MiB (name heap) + ~16 MiB (inline 24 B PathBuf -> 16 B `&'arena str`) + chunk waste 1-5 MiB = **~43 MiB saved heap, drops 88 B -> ~72 B inline** | Same as bumpalo: ~43 MiB heap saved | ~32 MiB (name heap, slightly more with dedup of common basenames) + ~16 MiB inline (or **~24 MiB if we use both `Spur` handles**) + dedup wins on repeated names = **~48-56 MiB saved heap, drops 88 B -> ~64 B inline** |
| Estimated RSS gap reduction vs the 3-11x baseline | ~2.0-2.4x remaining (vs ~3x at 100 K from RSS-3) | Same as bumpalo | ~1.8-2.2x remaining; **best on workloads with high basename repetition** (monorepos, node_modules, vendored deps) |

## Per-crate API shape

### bumpalo

```rust
use bumpalo::Bump;
use std::path::Path;

pub struct Flist {
    arena: Bump,
    entries: Vec<FileEntry>,
}

pub struct FileEntry {
    name: &'static str,     // really 'arena, transmuted via self-ref pattern
    dirname: &'static str,  // same
    // ... other 8-byte fields
}

impl Flist {
    fn push(&mut self, basename: &str, dirname: &Path) {
        // Allocate basename into the arena - one bump-pointer add.
        let name: &str = self.arena.alloc_str(basename);
        // Caller is expected to intern dirnames via a side HashMap<&Path, &'arena Path>
        // so identical dirnames share storage.
        let dirname_str: &str = self.arena.alloc_str(dirname.to_str().unwrap());
        self.entries.push(FileEntry { name, dirname: dirname_str, /* ... */ });
    }
}
// Drop: `drop(arena)` frees ALL chunks in O(chunks).
// `Bump` is !Sync; for parallel consumers, freeze the Flist behind &Flist
// (the &'arena str refs are Send + Sync because str is Send + Sync).
```

### typed-arena

```rust
use typed_arena::Arena;
use std::path::Path;

pub struct Flist {
    name_arena: Arena<u8>,
    dir_arena: Arena<u8>,
    entries: Vec<FileEntry>,
}

pub struct FileEntry {
    name: &'static str,
    dirname: &'static str,
    // ...
}

impl Flist {
    fn push(&mut self, basename: &str, dirname: &Path) {
        let name_bytes: &[u8] = self.name_arena.alloc_extend(basename.bytes());
        let name: &str = std::str::from_utf8(name_bytes).unwrap();
        // Same caveat as bumpalo: no built-in dedup. Need a HashMap<&Path, &'arena str>
        // to share repeated dirnames.
        let dir_bytes: &[u8] = self.dir_arena.alloc_extend(dirname.as_os_str().as_encoded_bytes().iter().copied());
        let dirname: &str = std::str::from_utf8(dir_bytes).unwrap();
        self.entries.push(FileEntry { name, dirname, /* ... */ });
    }
}
// Drop: walks every chunk and runs Drop per T. For T = u8 this is a no-op
// per element but still a per-chunk walk. Slower than bumpalo for plain bytes.
```

### lasso

```rust
use lasso::{Spur, Rodeo, RodeoReader};
use std::path::Path;

pub struct FlistBuilder {
    paths: Rodeo,                // single-threaded build phase
    entries: Vec<FileEntry>,
}

pub struct Flist {
    paths: RodeoReader,          // frozen, Sync, lock-free reads
    entries: Vec<FileEntry>,
}

pub struct FileEntry {
    name: Spur,                  // 4 B handle
    dirname: Spur,               // 4 B handle - 8 B total replaces 40 B
    // ...
}

impl FlistBuilder {
    fn push(&mut self, basename: &str, dirname: &Path) {
        let name = self.paths.get_or_intern(basename);
        let dirname = self.paths.get_or_intern(dirname.to_str().unwrap());
        self.entries.push(FileEntry { name, dirname, /* ... */ });
    }
    fn freeze(self) -> Flist {
        Flist { paths: self.paths.into_reader(), entries: self.entries }
    }
}

impl Flist {
    fn name(&self, e: &FileEntry) -> &str { self.paths.resolve(&e.name) }
    fn dirname(&self, e: &FileEntry) -> &str { self.paths.resolve(&e.dirname) }
}
// Drop: drop(Rodeo) frees the backing string arena in one pass.
// Spur is Copy + Send + Sync + 'static. Path comparisons reduce to u32 ==.
```

## Recommendation: **`lasso`** as the RSS-5 design starting point

Reasoning, against the alternatives:

1. **Smallest inline footprint.** `lasso` is the only candidate that
   replaces the **24 B `PathBuf` + 16 B `Arc<Path>` = 40 B** of inline
   fields with **two 4 B `Spur` handles = 8 B**. That is a 32 B
   inline-per-entry win on top of the heap win, which directly
   attacks the #2 contributor from RSS-3 (Vec<FileEntry> inline
   footprint multiplier, ~61 MiB at 1 M). `bumpalo` and `typed-arena`
   keep the fat `&str` (16 B) and need an extra layer to dedup
   dirnames; their best case is 16 B inline per path field.
2. **Free dirname dedup with no side table.** RSS-3 #3 is
   `dirname: Arc<Path>` and its atomic-RC traffic. `lasso`'s handle
   equality *is* string equality, so dirname dedup is free and the
   atomic clone on every `FileEntry::clone()` becomes a `Copy` of a
   4 B `u32`. With `bumpalo`/`typed-arena` we would have to bolt on a
   `HashMap<&Path, &'arena Path>` interner just to recover what
   `lasso` does natively, while still paying 16 B fat-pointer cost.
3. **Cleanest rayon story.** The build-then-freeze split
   (`Rodeo` -> `RodeoReader`) matches our existing
   `PathInterner` -> sequential-decode then parallel-consumers
   pattern. `RodeoReader` is `Sync` with lock-free reads, which is
   exactly what `PARALLEL_STAT_THRESHOLD = 64` consumers need.
   `bumpalo` requires a self-referential transmute or
   `bumpalo-herd`; `typed-arena` requires per-worker arenas.
4. **Repeats-friendly.** Real workloads (monorepos, vendored deps,
   `__init__.py`, `.gitignore`, `Cargo.toml`) have heavy basename
   repetition. `lasso` deduplicates **basenames too**, not just
   dirnames; `bumpalo`/`typed-arena` re-store every basename. The
   estimated extra 5-13 MiB win at 1 M from basename dedup is
   workload-dependent but always non-negative.
5. **Drop ordering matches our needs.** Flist teardown is a single
   `drop(Flist)` and lasso frees the backing arena in one pass.
   Handles are `Copy + 'static` so we don't fight the borrow checker
   when entries move between `Vec`s (e.g., during incremental
   segmentation, sort, hardlink resolution).

The one cost: an extra `Rodeo::resolve()` call wherever we need
`&str`/`&Path` for I/O. That is a single hashtable lookup (or, with
`RodeoReader`, an indexed array read). The audit's measurement
methodology in RSS-3 makes clear the bottleneck is allocator traffic,
not hash lookups, so the trade is sound. RSS-5 will spec whether to
keep a per-`FileEntry` `&'a str` cache during the active receive
window if profiling shows the resolve cost matters.

## Open questions for RSS-5

1. **Single rodeo vs two rodeos?** Names and dirnames have different
   distributions (names are unique-heavy, dirnames repeat-heavy). One
   rodeo is simpler; two lets us pick different `Spur` widths
   (`MicroSpur` for dirnames, `Spur` for names) and avoid a shared
   hash collision on identically-named files in different dirs vs
   their parent dir.
2. **Sender vs receiver path coverage.** RSS-3 notes the CLI sender
   constructors **do not** intern today. RSS-5 must spec whether the
   sender builds a `Rodeo` from scratch per-flist or whether the
   walker pre-interns into a shared rodeo to avoid double-work.
3. **Symlink target storage.** `link_target` lives in `extras`. Same
   rodeo or a separate one? (Symlink targets repeat much less than
   dirnames.)
4. **Wire-replay ordering.** Upstream's flist replays in *send*
   order. `lasso` insertion order is preserved, but if we re-use
   handles across multiple flists in a session (e.g., recursive
   incremental segments) we need to confirm we don't accidentally
   emit a handle whose interning happened on a later segment.
5. **Bench harness reuse.** `crates/protocol/benches/file_entry_memory.rs`
   asserts <= 96 B inline. RSS-5 should propose the new inline target
   (~64 B Unix with two `Spur`s replacing the 40 B path fields) and
   wire it into that bench.
6. **Migration sequencing.** Replacing `PathBuf`/`Arc<Path>` is not
   a single PR. RSS-5 should sequence: (a) add `Rodeo` to
   `FileListReader` and have it produce both legacy `Arc<Path>` and
   new `Spur` in parallel, (b) flip readers to `Spur`-only, (c)
   migrate the sender constructors, (d) drop the legacy fields. Each
   step must keep `size_of::<FileEntry>` monotonically shrinking and
   keep `tests/golden/` wire-format tests green.
7. **`lasso` feature flags.** Default features pull `hashbrown` +
   `ahash` (already in our tree via other crates). The
   `multi-threaded` feature pulls `dashmap` + `parking_lot` - both
   already present. So the net new-dependency cost is **zero**
   provided RSS-5 confirms the workspace already resolves these
   transitively (a `cargo tree` check, deferred to RSS-5).
8. **Path encoding on Windows.** `Spur` resolves to `&str`. Windows
   paths are `WTF-8`-encoded inside `PathBuf`; the rodeo would need
   to store either `&[u8]` (lasso supports custom key types via
   `Key + Interner` traits) or we accept the lossy `to_string_lossy`
   round-trip. RSS-5 should pick: custom byte-key rodeo, or
   `bumpalo`-style raw bytes for the Windows path only.
