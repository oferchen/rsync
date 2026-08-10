# Beta tag cutting procedure

Step-by-step runbook for cutting an oc-rsync beta tag. Expands on the
project-level 7-step release procedure with beta specific details and
copy-pasteable commands.

Pre-approved framing: `docs/design/br-6-sign-off-check-in-2026-05-21.md`
(recommendation: PROCEED once SEC-1.i + SEC-1.j land on master).

Placeholders used below:

- `vX.Y.Z-beta.N` -> the final beta tag (e.g. `v0.7.0-beta.1`).
- `X.Y.Z-beta.N` -> the same string with the leading `v` stripped (used
  in `Cargo.toml`).

## 1. Pre-flight verification

Run these from a clean checkout of `origin/master` before touching any
files.

```bash
# Latest CI on master is green.
gh run list --branch master --workflow=ci.yml --limit 1

# No critical PRs are pending against master.
gh pr list --state open --base master

# Last sanity look at recent commits.
git log --oneline origin/master | head -10
```

Confirm by inspection:

- `docs/design/br-6-sign-off-check-in-2026-05-21.md` reads
  "PROCEED with the beta tag once SEC-1.i (#4690) and SEC-1.j land on
  master" and both PRs are merged on `origin/master`.
- `SECURITY.md` SEC-1 row reads "MOSTLY FIXED" (or "FIXED" if the
  receiver-wiring follow-ups landed). SEC-2 and SEC-3 must read "FIXED".
- `tools/no_placeholders.sh` is clean on master (no `todo!`,
  `unimplemented!`, `FIXME` on live code paths).

```bash
bash tools/no_placeholders.sh
```

## 2. Version bump

Pick the tag (semver `MAJOR.MINOR.PATCH-beta.N`, e.g. `v0.7.0-beta.1`)
and update every version-bearing file. The release-cross workflow
(`.github/workflows/release-cross.yml`) gates on tag-vs-`Cargo.toml`
equality; the strings MUST match byte-for-byte (minus the leading `v`).

```bash
# Workspace version. There is one workspace `version = "..."` line near
# `[workspace.package]` and one `rust_version = "..."` line under
# `[workspace.metadata.oc_rsync]`. Update both.
$EDITOR Cargo.toml
# Set:
#   version = "X.Y.Z-beta.N"
#   rust_version = "X.Y.Z-beta.N"

# README "## Status" line.
$EDITOR README.md
# Update: **Release:** X.Y.Z-beta.N (beta) - ...

# Promote the [Unreleased] section to the new beta tag with date.
$EDITOR CHANGELOG.md
# Replace:
#   ## [Unreleased]
# With:
#   ## [X.Y.Z-beta.N] - YYYY-MM-DD
# And add a fresh, empty [Unreleased] above it.

# Beta release notes - replace the `v0.7.0-beta` / `vX.Y.0-beta`
# placeholders throughout with the final tag name; pin the commit range
# in the "Performance" section's chart URL.
$EDITOR .github/RELEASE_NOTES_BETA.md
```

Sanity-check the bump:

```bash
grep -E '^version|rust_version' Cargo.toml | head -5
grep -n "Release:" README.md | head -3
head -20 CHANGELOG.md
```

## 3. PR + merge

```bash
git checkout -b release/vX.Y.Z-beta.N
git add Cargo.toml README.md CHANGELOG.md .github/RELEASE_NOTES_BETA.md
git commit -m "chore(release): vX.Y.Z-beta.N"
git push -u origin release/vX.Y.Z-beta.N

gh pr create \
  --title "chore(release): vX.Y.Z-beta.N" \
  --body "Version bump for vX.Y.Z-beta.N. See \`.github/RELEASE_NOTES_BETA.md\` and \`docs/design/br-6-sign-off-check-in-2026-05-21.md\`."

# Wait for required checks (fmt+clippy, nextest stable, Windows stable,
# macOS stable, Linux musl stable).
gh pr checks --watch

# Squash-merge once green.
gh pr merge --squash --delete-branch
```

## 4. Tag + push

```bash
git checkout master
git pull --ff-only origin master

# Sanity: HEAD should be the squash-merged release commit.
git log --oneline -1

git tag vX.Y.Z-beta.N

# Explicit refspec to avoid the master/master ambiguity (a tag and
# branch sharing a name make `git push` ambiguous; see "Known
# pitfalls" below).
git push origin refs/tags/vX.Y.Z-beta.N
```

## 5. GitHub release

```bash
gh release create vX.Y.Z-beta.N \
  --prerelease \
  --title "oc-rsync vX.Y.Z-beta.N" \
  --notes-file .github/RELEASE_NOTES_BETA.md
```

If `.github/RELEASE_NOTES_BETA.md` still has placeholders or you want
the install/toolchain header from the standard template, splice
`.github/RELEASE_TEMPLATE.md` in first:

```bash
{ sed "s/{{VERSION}}/vX.Y.Z-beta.N/g" .github/RELEASE_TEMPLATE.md
  printf '\n'
  cat .github/RELEASE_NOTES_BETA.md
} > /tmp/release-body.md
gh release edit vX.Y.Z-beta.N --notes-file /tmp/release-body.md
```

The `--prerelease` flag is non-negotiable for a beta; without it the
release shows up as the "Latest" badge on the repo landing page.

## 6. Post-tag CI verification

Both `release-cross.yml` and `benchmark.yml` trigger off the tag push.

```bash
# Watch the release workflow.
gh run list --workflow=release-cross.yml --limit 1
gh run watch  # paste the run ID from the line above

# Confirm: validate-version passed, all platform builds uploaded
# artifacts, Homebrew formula PR was opened.
gh release view vX.Y.Z-beta.N
gh pr list --repo oferchen/rsync --state open --search "Homebrew vX.Y.Z-beta.N"

# Benchmark workflow appends the chart PNG + headline numbers to the
# release body. Confirm it ran exactly once on this tag.
gh run list --workflow=benchmark.yml --limit 5
```

CAUTION on the tag pattern. Both workflows trigger on
`v[0-9]*.[0-9]*.[0-9]*` (and the legacy `-rust` variant). A
`vX.Y.Z-beta.N` tag DOES match the first glob because `[0-9]*` is shell
glob style, not regex, and the trailing `-beta.N` falls under the `*`
in the third segment. Verify the workflow actually fired:

```bash
gh run list --workflow=release-cross.yml --event push --limit 5
```

If neither workflow fired, retag with the workflow `workflow_dispatch`
trigger as a fallback:

```bash
gh workflow run release-cross.yml --ref vX.Y.Z-beta.N
gh workflow run benchmark.yml --ref vX.Y.Z-beta.N \
  -f target_tag=vX.Y.Z-beta.N
```

## 7. Beta-specific caveats (call these out in the release announcement)

Lifted from `docs/design/br-6-sign-off-check-in-2026-05-21.md` and
already in `.github/RELEASE_NOTES_BETA.md`. Re-state them in any
external announcement copy (mailing list, blog post, GitHub Discussions
thread) so operators do not have to dig:

- Windows IOCP path ships and is exercised by the Windows CI matrix; the
  hardware-bench profile is deferred to a post-beta capture (WPG-1
  closure, #4688). Treat Windows IOCP throughput numbers as
  informational.
- `IORING_OP_SEND_ZC` is opt-in via the `iouring-send-zc` Cargo feature.
  Default builds use plain SEND even on Linux 5.16+. The IUS-4
  default-on decision (#4687) waits on IUS-3 offline bench numbers.
- parallel-receive-delta is default-on (PIP-5, #4666). Operators hitting
  a regression can opt out via the legacy sequential path during the
  beta soak.
- Linux 5.13+ is required for the optional `landlock` Cargo feature
  (additive defense-in-depth on top of the SEC-1 `*at` chain).
  Daemons on older kernels run with the `*at` chain alone, which is
  itself sufficient against CVE-2026-29518 / CVE-2026-43619.

## 8. Rollback procedure

Use ONLY if a critical bug surfaces in the first hours and a `beta.N+1`
respin is unavoidable.

```bash
# Delete the GitHub release first so the tag deletion does not orphan
# release artifacts mid-air.
gh release delete vX.Y.Z-beta.N --yes

# Delete the tag on origin. Branch protection allows tag force-push
# and tag deletion.
git push --force origin :refs/tags/vX.Y.Z-beta.N

# Delete locally too.
git tag -d vX.Y.Z-beta.N

# Branch off master for the patch.
git checkout master
git pull --ff-only origin master
git checkout -b hotfix/vX.Y.Z-beta.N.1
```

Then repeat steps 2 through 5 with the new tag name
(`vX.Y.Z-beta.(N+1)`, e.g. `v0.7.0-beta.2`). Never reuse a deleted tag
name; downstream caches (Homebrew, Docker layer cache, GitHub release
CDN) treat tag identity as immutable.

## 9. Known pitfalls

- **Ambiguous refs.** If a tag and branch share a name (e.g.,
  `master`), `git push` is ambiguous. Use `refs/heads/master` or
  `refs/tags/vX.Y.Z-beta.N` explicitly. Applied in step 4
  (`git push origin refs/tags/vX.Y.Z-beta.N`) and step 8
  (`git push --force origin :refs/tags/vX.Y.Z-beta.N`).
- **Benchmark appends to the release body.** `benchmark.yml` appends
  the chart and headline numbers to the release body on every tag
  push. Multiple pushes against the same tag produce duplicate
  sections. To recover a clean release: `gh release delete
  vX.Y.Z-beta.N`, recreate, then let CI run exactly once. Applied in
  step 6 (do not retag without first running the rollback in step 8)
  and step 8 (delete the GitHub release before the tag so the
  benchmark workflow on the respin starts from a clean body).
- **Force-push with branch protection.** Branch protection allows
  force-push (needed for retagging) and tag deletion. Step 8 uses the
  deletion form (`:refs/tags/...`) rather than the force-push form;
  either works, but deletion is cleaner because the respin creates a
  new tag name (`beta.N+1`), not an overwrite of `beta.N`.

## References

- BR-6 sign-off check-in: `docs/design/br-6-sign-off-check-in-2026-05-21.md`
- Beta release notes scaffold: `.github/RELEASE_NOTES_BETA.md`
- Release body header template: `.github/RELEASE_TEMPLATE.md`
- Release CI workflow: `.github/workflows/release-cross.yml`
- Benchmark CI workflow: `.github/workflows/benchmark.yml`
- Auto-categorization config: `.github/release.yml`
