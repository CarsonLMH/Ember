## Summary

<!-- What changed, and what user or maintainer problem does it solve? -->

## Scope

<!-- Name the important files or boundaries touched, plus anything deliberately left out. -->

## Verification

<!-- List exact commands and results. Explain every relevant check not run. -->

## Risk and rollback

<!-- Could this affect the flip path, journal ordering, XMP, trash/restore, previews, protocols, face privacy, or filesystem safety? How can it be reverted safely? -->

## Visual evidence

<!-- UI changes: attach only synthetic or clearly licensed public-fixture screenshots and state the fixture's provenance. Never use a real personal photo. Remove this section for non-UI changes. -->

## Checklist

- [ ] I read `SPEC.md`, `AGENTS.md`, and the relevant design or architecture records.
- [ ] The change is narrow; unrelated cleanup is not included.
- [ ] User-visible behavior has a `CHANGELOG.md` entry, or I explained above why the change is not user-visible.
- [ ] I added or updated tests for changed behavior and did not skip tests, weaken assertions, or suppress lint to reach green.
- [ ] I stated whether the flip/folder-open hot path is affected. If it is, the private real-photo gate passed and only sanitized aggregate metrics are reported here.
- [ ] I stated whether durability, XMP, trash/restore, or filesystem behavior is affected, and the relevant durability/file-safety tests pass.
- [ ] No RAF file can be modified by this change.
- [ ] This PR contains no real photos or derivatives, Ember database or face data, XMP, private EXIF, filenames, paths, or private logs.
- [ ] Every screenshot uses synthetic or clearly licensed public imagery with provenance stated above, or this PR contains no screenshots.
- [ ] I listed every relevant check I could not run and why.
