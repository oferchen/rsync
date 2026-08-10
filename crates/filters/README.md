# filters

Ordered include/exclude/protect pattern evaluation implementing the Chain of
Responsibility pattern.

## Purpose

`filters` reproduces rsync's filter grammar governing `--include`/`--exclude`/
`--filter` handling. Rules are evaluated sequentially with first-match-wins
semantics, matching upstream `check_filter()` in `exclude.c`. Two independent
chains are maintained: one for transfer decisions (include/exclude) and one for
deletion protection (protect/risk).

## Key Public Types

- `FilterRule` - parsed rule with action (Include/Exclude/Protect/Risk/Clear/Merge/DirMerge)
- `FilterSet` - compiled rule collection with glob matchers and deduplication
- `FilterChain` - ordered evaluation chain (first match wins)
- `FilterAction` - Include, Exclude, or no-match result
- `EntryFilter` - high-level filter applied to file entries during transfer

## Dependencies (upstream)

`logging`, `globset`

## Dependents (downstream)

`engine`, `transfer`, `core`, `cli`, `batch`

## Design Notes

- Patterns honour anchored matches (leading `/`), directory-only rules
  (trailing `/`), and recursive wildcards (`**`)
- Protect directives prevent matched paths from deletion during `--delete`
- Merge/DirMerge rules load per-directory `.rsync-filter` files
- Property tests validate correctness against edge cases
