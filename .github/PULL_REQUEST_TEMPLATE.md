<!--
Title format: <prefix>(<scope>): <imperative summary>
Conventional prefix is load-bearing - a labeler workflow categorizes release
notes from it. Keep titles under 70 characters; put detail in the body.
See CONTRIBUTING.md for the full workflow.
-->

## Summary

- 1-3 bullets describing what changes and why.

## Test plan

- [ ] CI: fmt + clippy, nextest (stable), Windows, macOS, Linux musl.
- [ ] Add targeted local repro steps if applicable: `cargo nextest run -p <crate> -E 'test(<pattern>)'`.

## Coordination

- Related PRs / issues:
- Depends on:
- Blocked by:

## Conventional commit prefix

Pick one; PR title prefix must match the change category:

- `feat:` - new user-visible feature
- `fix:` - bug fix
- `perf:` - performance improvement
- `refactor:` - non-functional restructuring
- `docs:` - documentation only
- `test:` - tests only
- `style:` - formatting / lints, no behaviour change
- `chore:` - tooling, deps, housekeeping
- `ci:` - CI configuration

## Security considerations

Fill only if this PR touches `SECURITY.md`, daemon code, sandbox / `*at`
helpers, auth, or anything in the SEC-1 chain. Otherwise delete this section.

- Threat surface affected:
- Mitigations / tests added:
- Cross-references (`SECURITY.md`, design docs):

## Pre-merge checklist

- [ ] Conventional commit prefix on PR title matches the change category.
- [ ] `cargo fmt --all` run locally.
- [ ] `CHANGELOG.md` updated if user-visible.
- [ ] `SECURITY.md` updated if security-relevant.
- [ ] No references to internal tooling or non-human authoring aids; hyphens, not em-dashes.
