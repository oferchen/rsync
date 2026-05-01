# `--fuzzy` basis-file search algorithm audit

Tracking issue: oc-rsync task #2051. Compares the oc-rsync `--fuzzy` /
`-y` basis-file search and scoring against upstream rsync 3.4.1
(`generator.c::find_fuzzy()` + `util1.c::fuzzy_distance()`).

## Summary

oc-rsync implements `--fuzzy` and `-yy` end-to-end - CLI parsing, server-arg
propagation (`-y` / `-yy` flag echo), level-1 (destination directory only) and
level-2 (also reference dirs from `--compare-dest` / `--copy-dest` /
`--link-dest`) search semantics, and signature generation off the chosen
basis. The integration shape mirrors upstream: counter-based parsing of
`-y` / `-yy`, reference-dir join with the target's parent directory, and
fall-through ordering "exact dest -> ref dirs -> fuzzy". Verdict:
**feature present, but the scoring algorithm and the wire emission of
`FNAMECMP_FUZZY` diverge from upstream**.

The audit identifies six concrete divergences. The most consequential are:

1. oc-rsync does **not** emit `ITEM_BASIS_TYPE_FOLLOWS` + `FNAMECMP_FUZZY`
   in the receiver's per-file request. Upstream's generator does and
   uses it to disambiguate which basis was selected when several
   (`--link-dest`, `--copy-dest`, fuzzy) are in play.
2. oc-rsync's scoring is a bespoke additive heuristic (extension bonus,
   prefix points, suffix points, size-similarity bonus) where higher
   scores win. Upstream computes a Levenshtein distance with
   ASCII-weighted substitution cost on the full basename plus a 10x
   suffix-distance addition where lower scores win. The two functions
   are not equivalent and will pick different winners on identical
   inputs.
3. oc-rsync does not implement upstream's "exact size + mtime match
   short-circuit" that fires before any distance is computed and
   bypasses scoring entirely.

The `--fuzzy` plumbing - flag parsing, server-arg propagation,
fuzzy_dirlist construction, fall-through ordering, level-2 reference-dir
selection, basis-file signature generation - is wire-compatible. The
scoring + wire-byte emission needs alignment to claim true upstream
fidelity for `-y` / `-yy`.

## Upstream algorithm (rsync 3.4.1)

Source:
[`RsyncProject/rsync@v3.4.1`](https://github.com/RsyncProject/rsync/tree/v3.4.1).
Mirror archive: `https://download.samba.org/pub/rsync/src/rsync-3.4.1.tar.gz`.

### Flag parsing (`options.c`)

```c
{"fuzzy",           'y', POPT_ARG_NONE,   0, 'y', 0, 0 },
{"no-fuzzy",         0,  POPT_ARG_VAL,    &fuzzy_basis, 0, 0, 0 },
{"no-y",             0,  POPT_ARG_VAL,    &fuzzy_basis, 0, 0, 0 },
...
case 'y':
    fuzzy_basis++;
    break;
...
/* server-args echo: */
if (fuzzy_basis) {
    argstr[x++] = 'y';
    if (fuzzy_basis > 1)
        argstr[x++] = 'y';
}
...
/* post-parse adjustment: */
if (fuzzy_basis > 1)
    fuzzy_basis = basis_dir_cnt + 1;
```

`fuzzy_basis` is therefore (a) a count of `-y` occurrences during parsing
that gets clamped to the number of basis directories + 1 once parsing is
done, and (b) the loop bound for `find_fuzzy()` and the
`fuzzy_dirlist[]` array. A bare `-y` is `fuzzy_basis == 1` (search the
destination dir only). `-yy` becomes `basis_dir_cnt + 1` (destination
dir at index 0 plus one entry per basis dir).

### `fuzzy_dirlist` construction (`generator.c:1474-1489`)

```c
if (statret != 0 && fuzzy_basis) {
    if (need_fuzzy_dirlist) {
        const char *dn = file->dirname ? file->dirname : ".";
        int i;
        strlcpy(fnamecmpbuf, dn, sizeof fnamecmpbuf);
        for (i = 0; i < fuzzy_basis; i++) {
            if (i && pathjoin(fnamecmpbuf, MAXPATHLEN, basis_dir[i-1], dn)
                    >= MAXPATHLEN)
                continue;
            fuzzy_dirlist[i] = get_dirlist(fnamecmpbuf, -1,
                GDL_IGNORE_FILTER_RULES | GDL_PERHAPS_DIR);
            if (fuzzy_dirlist[i] && fuzzy_dirlist[i]->used == 0) {
                flist_free(fuzzy_dirlist[i]);
                fuzzy_dirlist[i] = NULL;
            }
        }
        need_fuzzy_dirlist = 0;
    }
    /* Sets fnamecmp_type to FNAMECMP_FUZZY or above. */
    fuzzy_file = find_fuzzy(file, fuzzy_dirlist, &fnamecmp_type);
    if (fuzzy_file) {
        f_name(fuzzy_file, fnamecmpbuf);
        ...
        sx.st.st_size = F_LENGTH(fuzzy_file);
        statret = 0;
        fnamecmp = fnamecmpbuf;
    }
}
```

Two invariants: index 0 of `fuzzy_dirlist[]` is the file's own destination
parent directory; indices 1..N are formed by joining each
`basis_dir[i-1]` with the relative parent. An empty directory is
materialised as `NULL`.

### `find_fuzzy()` (`generator.c:728-798`)

The function has two phases.

**Phase A - exact size+mtime short-circuit:**

```c
for (i = 0; i < fuzzy_basis; i++) {
    struct file_list *dirlist = dirlist_array[i];
    if (!dirlist) continue;
    for (j = 0; j < dirlist->used; j++) {
        struct file_struct *fp = dirlist->files[j];
        if (!F_IS_ACTIVE(fp)) continue;
        if (!S_ISREG(fp->mode) || !F_LENGTH(fp)
                || fp->flags & FLAG_FILE_SENT) continue;
        if (F_LENGTH(fp) == F_LENGTH(file)
                && same_time(fp->modtime, 0, file->modtime, 0)) {
            *fnamecmp_type_ptr = FNAMECMP_FUZZY + i;
            return fp;
        }
    }
}
```

If any candidate across all basis dirs has the same size **and** mtime as
the target, that file is returned immediately. No distance is computed.
Tie-breaking: first dirlist (lower `i`) wins, then first slot order
within the dirlist. `FLAG_FILE_SENT` excludes files already shipped this
run (avoids picking a basis the sender just wrote).

**Phase B - Levenshtein with suffix bonus:**

```c
uint32 lowest_dist = 25 << 16; /* ignore a distance greater than 25 */
...
for (i = 0; i < fuzzy_basis; i++) {
    ...
    for (j = 0; j < dirlist->used; j++) {
        ...
        name = fp->basename;
        len  = strlen(name);
        suf  = find_filename_suffix(name, len, &suf_len);

        dist = fuzzy_distance(name, len, fname, fname_len, lowest_dist);
        /* Add some extra weight to how well the suffixes match unless
         * we've already disqualified this file based on a heuristic. */
        if (dist < 0xFFFF0000U) {
            dist += fuzzy_distance(suf, suf_len, fname_suf,
                fname_suf_len, 0xFFFF0000U) * 10;
        }
        if (dist <= lowest_dist) {
            lowest_dist = dist;
            lowest_fp = fp;
            *fnamecmp_type_ptr = FNAMECMP_FUZZY + i;
        }
    }
}
```

Key properties of phase B:

- **Lower distance wins.** The hard cut-off is `25 << 16` (encoded as
  fixed-point); anything past this is ignored.
- **Distance metric is Levenshtein** with edit cost weighted by ASCII
  delta (`util1.c::fuzzy_distance`). Each unit of distance is `1 << 16`;
  the lower 16 bits encode an ASCII tie-breaker so `cat` vs `cau`
  differs from `cat` vs `caz`. Heuristic: if `|len1-len2| * UNIT >
  upperlimit` the function bails with `0xFFFFU * UNIT + 1` (treated as
  "ignore").
- **Suffix is weighted 10x.** A second `fuzzy_distance` call on
  `find_filename_suffix(name)` is added at 10x weight. Suffix detection
  ignores `~`, `.bak`, `.old`, `.orig`, `.~N~` and de-prioritises
  all-digit suffixes.
- **Tie-breaker** is `dist <= lowest_dist` (not `<`), so the **last**
  candidate at the minimum distance wins. Combined with iteration order
  (basis-dir index ascending, slot index ascending), this means later
  candidates beat earlier candidates of equal score, including across
  basis dirs.
- **`FNAMECMP_FUZZY + i`** is written through `fnamecmp_type_ptr` so
  the receiver knows which basis-dir tier the chosen fuzzy file came
  from.

### `fuzzy_distance()` (`util1.c:1646-1720`)

Linear-space Levenshtein. Edit cost is `UNIT + |c1 - c2|` for
substitutions, `UNIT + c` for insertion/deletion. Length-difference
heuristic returns `0xFFFFU * UNIT + 1` when the strings differ in length
by more than `upperlimit / UNIT` characters.

### `find_filename_suffix()` (`util1.c:1574-1627`)

Returns the most significant suffix and its length. Ignores leading
dots, `foo~`, `.bak`, `.old`, `.orig`, `.~N~`. Pure-numeric suffixes are
deprioritised - the loop continues past them looking for a non-numeric
extension first, falling back to the numeric one if nothing else
matches.

### Wire emission

After `find_fuzzy()` returns, upstream's generator path emits the basis
type via the iflag `ITEM_BASIS_TYPE_FOLLOWS` (0x0800) plus the
`fnamecmp_type` byte (`FNAMECMP_FUZZY = 0x83`, optionally
`FNAMECMP_FUZZY + i` for level 2). Defined in `rsync.h`.

## oc-rsync algorithm

### Flag parsing (`crates/cli/src/frontend/`)

`crates/cli/src/frontend/command_builder/sections/build_base_command/transfer.rs:208-222`:

```rust
Arg::new("fuzzy")
    .long("fuzzy")
    .short('y')
    .help("Search for basis files with similar names. Specify twice (-yy)
           to also search reference directories.")
    .action(ArgAction::Count)
    .overrides_with("no-fuzzy"),
...
Arg::new("no-fuzzy")
    .long("no-fuzzy")
    ...
    .overrides_with("fuzzy"),
```

`crates/cli/src/frontend/arguments/parser/mod.rs:347-357` lifts the count
into `Option<u8>`:

```rust
let fuzzy = {
    let count = matches.get_count("fuzzy");
    let negated = matches.get_flag("no-fuzzy");
    if negated && count == 0 {
        Some(0u8)
    } else if count > 0 {
        Some(count.min(2))
    } else {
        None
    }
};
```

Note the `min(2)` - any extra `-y` past the second is silently clamped.
Upstream stores the raw count and only converts via
`basis_dir_cnt + 1` post-parse, so `-yyy` upstream would set
`fuzzy_basis = basis_dir_cnt + 1` regardless. In oc-rsync `-yyy` is
equivalent to `-yy`. In practice both are level-2 behaviour because
the receiver only checks `fuzzy_level >= 2`, but the server-args echo
diverges below.

### Server-args propagation (`crates/core/src/client/remote/invocation/builder.rs:524-527`)

```rust
// upstream: options.c:2613 - send 'y' for fuzzy, 'yy' for level 2
for _ in 0..self.config.fuzzy_level() {
    flags.push('y');
}
```

This matches upstream byte-for-byte for `fuzzy_level` of 0, 1, 2.

### `FuzzyMatcher` (`crates/match/src/fuzzy/`)

Public API in `crates/match/src/fuzzy/mod.rs`:

- `FuzzyMatcher::new()` -> level 1, `min_score = 10`.
- `FuzzyMatcher::with_level(level: u8)` -> arbitrary level, default
  `min_score = 10`.
- `FuzzyMatcher::with_fuzzy_basis_dirs(Vec<PathBuf>)` -> additional
  search dirs, used only when `fuzzy_level >= FUZZY_LEVEL_2`
  (`fuzzy/mod.rs:104`, `fuzzy/search.rs:79`).

Search loop (`crates/match/src/fuzzy/search.rs:65-91`):

```rust
pub fn find_fuzzy_basis(&self, target_name: &OsStr, dest_dir: &Path,
                        target_size: u64) -> Option<FuzzyMatch> {
    let target_name_str = target_name.to_string_lossy();
    let mut best_match: Option<FuzzyMatch> = None;

    if let Some(m) = search_directory(dest_dir, &target_name_str,
                                       target_size, self.min_score) {
        update_best_match(&mut best_match, m, self.min_score);
    }

    if self.fuzzy_level >= FUZZY_LEVEL_2 {
        for basis_dir in &self.fuzzy_basis_dirs {
            if let Some(m) = search_directory(basis_dir, &target_name_str,
                                               target_size, self.min_score) {
                update_best_match(&mut best_match, m, self.min_score);
            }
        }
    }

    best_match
}
```

Per-directory scan (`crates/match/src/fuzzy/search.rs:98-138`):

- Skips non-files.
- Skips exact name matches (`candidate_name == target_name`).
- Calls `compute_similarity_score(target, candidate, target_size,
  candidate_size)`.
- Threshold: keeps the best at-or-above `min_score`.
- `update_best_match` is `>=`-strict ("higher score wins, ties keep
  existing"), the **first** candidate at a given score wins, opposite
  to upstream's `dist <= lowest_dist` "last wins".

Scoring (`crates/match/src/fuzzy/scoring.rs:72-108`):

```rust
pub fn compute_similarity_score(target: &str, candidate: &str,
                                target_size: u64, candidate_size: u64) -> u32 {
    let mut score: u32 = 0;
    let (target_base, target_ext) = split_name_extension(target);
    let (candidate_base, candidate_ext) = split_name_extension(candidate);

    if target_ext == candidate_ext && !target_ext.is_empty() {
        score += EXTENSION_MATCH_BONUS;            // +50
    }
    let prefix_len = common_prefix_length(target_base, candidate_base);
    score += prefix_len as u32 * PREFIX_MATCH_POINTS;   // +10/char
    let suffix_len = common_suffix_length(target_base, candidate_base);
    score += suffix_len as u32 * SUFFIX_MATCH_POINTS;   // +8/char

    if target_size > 0 && candidate_size > 0 {
        let size_ratio = if target_size >= candidate_size {
            candidate_size as f64 / target_size as f64
        } else {
            target_size as f64 / candidate_size as f64
        };
        if size_ratio >= 0.5 {
            score += SIZE_SIMILARITY_BONUS;        // +30
        }
    }
    score
}
```

Constants (`crates/match/src/fuzzy/mod.rs:66-91`):

| Constant | Value |
| --- | --- |
| `MIN_FUZZY_SCORE` | 10 |
| `EXTENSION_MATCH_BONUS` | 50 |
| `PREFIX_MATCH_POINTS` | 10 (per char) |
| `SUFFIX_MATCH_POINTS` | 8 (per char) |
| `SIZE_SIMILARITY_BONUS` | 30 (size_ratio >= 0.5) |

`split_name_extension()` (`scoring.rs:127-132`) splits at the **last**
`.`. Hidden files (leading `.`) without a second `.` have empty
extension. There is no "ignore `.bak`/`.old`/`.orig`/`.~N~`/`~`"
filtering and no all-numeric demotion.

### Receiver wiring (`crates/transfer/src/receiver/basis.rs`)

`find_basis_file_with_config` (`basis.rs:226-256`):

```rust
let basis = try_open_file(config.file_path)
    .or_else(|| try_reference_directories(config.relative_path,
                                          config.reference_directories))
    .or_else(|| {
        if config.fuzzy_level > 0 {
            try_fuzzy_match(config.relative_path, config.dest_dir,
                            config.target_size, config.fuzzy_level,
                            config.reference_directories)
        } else {
            None
        }
    });
```

Search order: exact dest -> reference dirs (per-relative-path probe) ->
fuzzy. This matches upstream `recv_generator()`'s pre-fuzzy
basis-dir scan via `try_dests_*()` followed by the
`if (statret != 0 && fuzzy_basis)` block.

`try_fuzzy_match` (`basis.rs:149-177`):

- Joins each reference dir with `relative_path.parent()` and filters
  to those that `is_dir()`. Mirrors upstream's
  `pathjoin(fnamecmpbuf, basis_dir[i-1], dn)` with empty-dir filtering.
- Constructs a `FuzzyMatcher` with the requested level and the
  filtered dirs.
- The destination directory itself is passed separately to
  `find_fuzzy_basis`. Upstream stuffs it at index 0 of `fuzzy_dirlist[]`
  alongside the basis-dir entries; oc-rsync keeps it as a distinct
  argument and prepends it to the search.

### Wire emission

`crates/transfer/src/transfer_ops/request.rs:99-153`
(`send_file_request_xattr`):

- Writes `iflags = ITEM_TRANSFER` (optionally `| ITEM_REPORT_XATTR`).
- Writes `sum_head` (block count, blen, s2length, remainder) followed by
  signature blocks.
- Never sets `ITEM_BASIS_TYPE_FOLLOWS` (0x0800), never writes a
  `fnamecmp_type` byte.

`crates/protocol/src/fnamecmp.rs:97` defines `FnameCmpType::Fuzzy`
(`0x83`) and the encode/decode is exercised by unit tests
(`fnamecmp.rs:228-348`), but no production code path emits the fuzzy
variant. The wire byte is read on the generator side
(`crates/transfer/src/generator/transfer.rs:232-235`,
`crates/transfer/src/generator/item_flags.rs:160-199`) but immediately
discarded (`_fnamecmp_type`).

## Side-by-side comparison

| Aspect | Upstream rsync 3.4.1 | oc-rsync |
| --- | --- | --- |
| Flag form | `-y`, `--fuzzy`, `--no-fuzzy`, `--no-y` | `-y`, `--fuzzy`, `--no-fuzzy` (no `--no-y`) |
| Counter | `fuzzy_basis++`, then post-parse `= basis_dir_cnt + 1` if `> 1` | `Option<u8>` clamped to `min(2)` at parse time |
| Server-args | `y` then `y` again if `>1` | identical |
| Search-dir count | `1 + basis_dir_cnt` for `-yy` | `1 + reference_directories.len()` (filtered to dirs that exist) |
| Search-dir order | `[dest, basis_dir[0], basis_dir[1], ...]` | `[dest, reference_directories[0], ...]` |
| Skip exact name | implicit (different name on disk) | explicit `if candidate_name == target_name { continue }` |
| Skip already-sent | `FLAG_FILE_SENT` excludes files written this run | not tracked |
| Skip empty / non-regular | `S_ISREG(fp->mode) && F_LENGTH(fp)` | `metadata.is_file()` (no zero-length skip) |
| Filter rules | `GDL_IGNORE_FILTER_RULES` | `fs::read_dir` on the directory; no filter chain runs here either |
| Phase A short-circuit | exact size+mtime returns immediately, no scoring | absent - all candidates go through scoring |
| Distance metric | Levenshtein with ASCII tie-breaker; lower wins | Bespoke additive bonus; higher wins |
| Suffix weight | 10x of suffix Levenshtein | 8 points per matching char |
| Suffix detection | `find_filename_suffix` ignores `~`, `.bak`, `.old`, `.orig`, `.~N~`, demotes all-numeric | naive `rfind('.')` split |
| Distance cut-off | `25 << 16` | `MIN_FUZZY_SCORE = 10` (lower bound) |
| Tie-breaker | last-at-minimum wins (`dist <= lowest_dist`, ascending dirlist + slot) | first-at-maximum wins (`>=` keeps existing) |
| Size handling | not used as a primary signal in phase B | binary `>= 0.5` ratio bonus of 30 |
| Wire emission | `ITEM_BASIS_TYPE_FOLLOWS` + `FNAMECMP_FUZZY + i` | not emitted; receiver sends signatures with no basis-type byte |
| Generator decoding | uses `fnamecmp_type` to locate basis on sender side too | reads byte and discards it (`_fnamecmp_type`) |
| `--whole-file` interaction | basis search skipped entirely (`always_checksum`/`whole_file` paths in `recv_generator`) | identical: `BasisFileConfig::whole_file` short-circuits before scoring (`basis.rs:229-231`) |
| `--inplace` interaction | inplace + backup -> `FNAMECMP_BACKUP`; otherwise no fuzzy interaction | no special interaction; basis search proceeds the same |
| `--append` interaction | append uses existing dest size, signature blocks omitted | `RequestConfig::append` skips block emission (`request.rs:136-140`); fuzzy still runs but the resumed file is the dest, not a fuzzy pick |
| `--copy-dest` / `--link-dest` / `--compare-dest` | populate `basis_dir[]`, become `fuzzy_dirlist[1..]` for `-yy` | `reference_directories` populates the matcher's `fuzzy_basis_dirs` for level >= 2 |

## Findings

### F1 - scoring functions are not equivalent (DIVERGENCE)

Upstream picks the candidate with minimum Levenshtein-style edit
distance (with suffix weighted 10x). oc-rsync picks the candidate with
the maximum sum of (extension bonus, prefix points, suffix points, size
bonus). The two functions disagree on common inputs:

- Target `report_2024.csv`, candidates `report_2023.csv` (1 char diff at
  position 9) and `report_2025.pdf` (1 char diff + extension change):
  upstream prefers `report_2023.csv` (low distance, suffix matches),
  oc-rsync also prefers `report_2023.csv` (extension bonus + larger
  common prefix). Aligned by accident.
- Target `data.txt`, candidates `aata.txt` (single substitution at
  position 0) and `xxxx.txt` (four substitutions):
  upstream picks `aata.txt` (distance ~`UNIT + small ASCII delta`),
  oc-rsync picks `aata.txt` (suffix `ata.txt` of length 7 yields 56
  suffix points; vs `.txt` only of length 4 yields 32 suffix points,
  plus extension bonus on both). Aligned.
- Target `foo.txt`, candidates `foo_v2.txt` and `bar.txt` (same size as
  target):
  upstream picks `foo_v2.txt` (lower edit distance: 3 inserts).
  oc-rsync compares: `foo_v2` vs `foo` -> prefix 3, suffix 0; `bar` vs
  `foo` -> prefix 0, suffix 0. Both score 50 (extension) + 30 (size).
  `foo_v2.txt` adds 30 prefix; `bar.txt` adds 0. oc-rsync also picks
  `foo_v2.txt`. Aligned.
- Target `image_001.jpg`, candidates `image_002.jpg` and
  `image_999.jpg` (both at distance 2 from the target by upstream
  metric, both scoring identically by oc-rsync's scheme): tie-breaking
  diverges (see F4).

Net effect: most "obvious" cases agree, but the metrics diverge whenever
ASCII proximity matters (upstream weights `cat` vs `caa` differently
from `cat` vs `caz`) and whenever the suffix-detection heuristics fire
(upstream's `find_filename_suffix` treats `foo~`, `foo.bak`,
`foo.~1~` specially; oc-rsync does not).

### F2 - phase A "exact size+mtime" short-circuit is missing (DIVERGENCE)

Upstream's phase A returns immediately on any candidate with matching
size and mtime. oc-rsync runs every candidate through the additive
scorer. This has two consequences:

- Performance. Upstream avoids two `fuzzy_distance` calls per candidate
  when an obvious twin exists. oc-rsync always does the full pass.
- Selection. When two candidates have identical name-distance but only
  one shares the target's size+mtime, upstream picks the size+mtime
  twin; oc-rsync may pick either. In practice this matters for "user
  renamed the file" scenarios where a sibling has the same content but
  a different name.

### F3 - `FNAMECMP_FUZZY` wire byte is never emitted (DIVERGENCE, wire-protocol)

oc-rsync's receiver sends per-file requests via
`send_file_request_xattr` (`crates/transfer/src/transfer_ops/request.rs:99-153`)
and never sets `ITEM_BASIS_TYPE_FOLLOWS` (0x0800), even when a fuzzy
basis was actually selected and used to compute the signature. Upstream
sets it and writes `FNAMECMP_FUZZY + i` so the sender can locate the
same basis (relevant for stats, debug output, and the
`fuzzy basis selected for X: Y` log line).

`FnameCmpType::Fuzzy` is decoded by the protocol crate
(`crates/protocol/src/fnamecmp.rs:97-149`) and read on the generator
side (`crates/transfer/src/generator/transfer.rs:232`) but only to
advance the byte counter - the value is bound to `_fnamecmp_type` and
ignored. This is a one-way divergence: oc-rsync receivers can ingest
upstream's fuzzy basis-type byte without crashing, but oc-rsync senders
will never emit it.

This particular divergence is **silent** at the byte level - the byte is
optional (gated on `ITEM_BASIS_TYPE_FOLLOWS`) so the wire stays
parseable. But interop tools or log scrapers that rely on
`FNAMECMP_FUZZY + i` to reconstruct which tier a fuzzy basis came from
will see oc-rsync as "no fuzzy match was used", which is incorrect.

### F4 - tie-breaking direction is inverted (MINOR DIVERGENCE)

Upstream uses `dist <= lowest_dist` so the **last** candidate at the
minimum distance wins. oc-rsync's `update_best_match`
(`search.rs:144-153`) uses `existing.score >= candidate.score` to keep
the existing entry, i.e. the **first** candidate at the maximum score
wins. Combined with `fs::read_dir` ordering (filesystem-dependent, not
sorted), oc-rsync's tie-breaker is effectively non-deterministic;
upstream's is "last-in-iteration-order" which is deterministic given
the dirlist sort that `get_dirlist` performs.

### F5 - `find_filename_suffix` heuristics are absent (DIVERGENCE)

Upstream's suffix extraction
(`util1.c:1574-1627`) skips `~`, `.bak`, `.old`, `.orig`, `.~N~`, and
demotes pure-numeric suffixes. oc-rsync's `split_name_extension`
(`scoring.rs:127-132`) is a single `rfind('.')`. Concrete impact:

- Target `report.csv`, candidate `report.csv.bak`: upstream's suffix
  for the candidate is `csv` (skipping `bak`), so the suffix-distance
  vs target's `csv` is 0 and the candidate scores well. oc-rsync
  splits at the last dot so the candidate's "extension" is `bak`,
  which fails the extension-equality test.
- Target `app-1.2.3.tar.gz`, candidate `app-1.2.2.tar.gz`: upstream
  walks past the all-numeric suffix `2` to land on `gz`. oc-rsync
  splits at the last dot only, also landing on `gz`. Aligned in this
  case.

The suffix divergence systematically misranks `.bak` / `.old` /
`.orig` / `~`-suffixed candidates downward.

### F6 - already-sent files can be re-selected (MINOR DIVERGENCE)

Upstream filters `fp->flags & FLAG_FILE_SENT` to avoid picking a basis
that was just transferred this session. oc-rsync has no equivalent
gate. In practice the receiver runs the basis search before the
sender's writes land, so this only matters during retries and
partial-file recovery, where re-selecting a half-written file as basis
would corrupt the delta. Mitigated in oc-rsync by partial-file handling
elsewhere (`partial_dir`, `inplace` checks) but not by the fuzzy search
itself.

### F7 - level-1 / level-2 plumbing matches (MATCH)

`-y` searches dest only; `-yy` adds reference directories joined with
the relative parent. The reference-dir set is exactly the
`--compare-dest` / `--copy-dest` / `--link-dest` aggregate. The
level-2 directories are filtered to existing dirs before the matcher
runs, mirroring upstream's empty-dirlist nullification.

### F8 - `--whole-file` interaction matches (MATCH)

`BasisFileConfig::whole_file` short-circuits the entire basis search
(`basis.rs:229-231`), including fuzzy. Upstream's `recv_generator`
takes the equivalent path when whole-file is set.

### F9 - `--inplace` interaction matches (MATCH)

oc-rsync has no special fuzzy-vs-inplace logic, and neither does
upstream beyond the `FNAMECMP_BACKUP` carve-out (which is a separate
basis type, not a fuzzy interaction).

### F10 - `--append` interaction matches (MATCH)

Append mode skips signature-block emission
(`request.rs:136-140`) but lets the basis search run. Same on upstream:
the generator computes a sum_head off the existing destination and
the sender's `receive_sums` returns early.

### F11 - empty / zero-length files (MINOR DIVERGENCE)

Upstream skips zero-length candidates (`!F_LENGTH(fp)`). oc-rsync
checks only `metadata.is_file()`. A zero-length candidate in
oc-rsync's destination would be considered for fuzzy matching, score
near zero (no useful prefix/suffix overlap with a non-empty target,
size ratio 0.0), and be filtered out by `MIN_FUZZY_SCORE` only if the
target has a non-trivial name. Practically harmless; semantically a
small divergence.

### F12 - `--no-y` long form is missing (MINOR DIVERGENCE)

Upstream defines `--no-y` as an alias for `--no-fuzzy`. oc-rsync only
defines `--no-fuzzy`. A user invoking `--no-y` against oc-rsync gets a
"unknown long option" error. Trivial to add.

## Recommendations

1. **Replace the additive scorer with a Levenshtein metric.** Port
   `util1.c::fuzzy_distance` to the `match` crate (or use the
   `strsim::levenshtein` family with an ASCII-weighted substitution
   cost). Invert the comparison so lower distance wins. Keep the
   public `compute_similarity_score` shape but document that lower is
   better, or rename to `compute_fuzzy_distance`. Rebuild
   `update_best_match` around `<=` to match upstream's tie-breaker.
2. **Add the phase A short-circuit.** Before scoring, scan all
   candidates for an exact `size + mtime` match and return immediately.
   This is both a correctness fix (preferring identical-content twins)
   and a performance win.
3. **Emit `ITEM_BASIS_TYPE_FOLLOWS` and `FNAMECMP_FUZZY + i`.** Plumb
   the chosen basis tier through `BasisFileResult` and have
   `send_file_request_xattr` set the iflag and write the byte. Add a
   golden byte test mirroring an upstream `-y` capture.
4. **Implement upstream's suffix heuristics.** Port
   `find_filename_suffix` (skip `~`, `.bak`, `.old`, `.orig`, `.~N~`,
   demote all-numeric). This keeps `.bak`/`.old`-style backup names
   ranking correctly.
5. **Track already-sent files within the fuzzy search.** Add a
   bitset/HashSet keyed by inode (or by path within the run) to skip
   files the sender just produced. Likely lives on
   `BasisFileConfig` since the receiver already knows which file
   indices have completed.
6. **Drop the `min(2)` cap in CLI parsing** to match upstream's
   "anything > 1 means level 2" semantics, then convert to
   `basis_dir_cnt + 1` when constructing the matcher. This is cosmetic
   for the wire (the server-args echo only writes `y` or `yy`) but
   keeps the internal counter aligned with upstream for any future
   feature that reads the raw count.
7. **Add deterministic dirlist ordering.** Wrap `fs::read_dir` in a
   sort-by-basename so ties resolve deterministically the way
   upstream's `flist_sort_and_clean()` does.
8. **Add `--no-y` as an alias for `--no-fuzzy`.** Trivial CLI parity.

The first three items are the substantive fidelity gaps. Items 4-8
tighten edge cases. None of them change the on-the-wire envelope of the
file request (size, sum_head, blocks); they only change which basis
file is selected and whether the optional `fnamecmp_type` byte is
emitted.
