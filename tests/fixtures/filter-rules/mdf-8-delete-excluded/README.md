# MDF-8.a delete-excluded fixture

Closes the rows .1 / .4 / .5 x MDF-8 cells from FIL-AUD-2
(`docs/design/fil-aud-exclude-vs-mdf-matrix.md`). A single
`--delete --delete-excluded` invocation through the MDF-8 wire-diff harness
exercises all three UTS-DD-exclude root causes simultaneously:

- UTS-DD-exclude.1 - per-directory scope fall-through on the Deletion side
  (sibling scopes must not leak descendant matchers).
- UTS-DD-exclude.4 - implicit `FILTRULE_SENDER_SIDE` flip on per-token
  merge rules under `--delete-excluded`.
- UTS-DD-exclude.5 - no synthetic `**/`-prefix companion for patterns that
  already contain `**`.

Drive the fixture via the harness:

```sh
bash scripts/mdf_8_filter_diff_harness.sh \
    --fixture tests/fixtures/filter-rules/mdf-8-delete-excluded/source \
    --delete-excluded
```

The harness writes a JSON summary at `$OUTPUT/summary.json` with
`delete_excluded: true` so the FIL-AUD-5 close-out can grep for the
fixture's presence in CI artifacts.

See `docs/design/fil-aud-3-mdf-gap-tests-spec.md` section 2.7 for the
full specification.
