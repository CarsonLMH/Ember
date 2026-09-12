# Open-source readiness checklist

## 1. Truth and privacy gate

- [ ] Declare and document the supported Rust toolchain.
- [ ] Declare Node 22 and the package-manager version.
- [ ] Make CI consume those declarations and pass current Clippy without a lint
      suppression.
- [ ] Extend `.gitignore` for secrets, credentials, databases, signing files,
      RAF/RAW photos, XMP sidecars, and local fixture folders.
- [ ] Make the shared design-audit skill synthetic/public-fixture-first.
- [ ] Require explicit, one-run consent for any real-photo audit and prohibit
      publishing private media or metadata.
- [ ] Neutralize personal example names and absolute maintainer paths in active
      public contributor material.
- [ ] Add production dependency audit coverage and document unresolved dev-tool
      audit findings without forced upgrades.

### Checkpoint A

- [ ] Toolchain and CI-equivalent checks pass locally.
- [ ] Privacy-focused harness checks pass.
- [ ] `git check-ignore` proves representative private artifacts are excluded.

## 2. Public front door

- [ ] Resolve the public-name decision with the maintainer.
- [ ] Rewrite `README.md` for product comprehension and contributor onboarding.
- [ ] Add only CI, platform, and license badges.
- [ ] Add a synthetic or public-domain product screenshot with attribution.
- [ ] State maturity, Apple Silicon requirement, source-build status, and current
      distribution limits plainly.
- [ ] Link to architecture, privacy, benchmark, model, contribution, security,
      roadmap/spec, changelog, and license material.
- [ ] Keep maintainer-only shipping and real-photo commands out of quick start.

### Checkpoint B

- [ ] README links and badge targets resolve.
- [ ] A clean-checkout setup path is internally consistent.
- [ ] No public visual or example contains personal media or identifying data.

## 3. Contribution rails

- [ ] Add `CONTRIBUTING.md` with setup, testing tiers, change rules, and PR flow.
- [ ] Add `SECURITY.md` with a private reporting route and supported-version
      policy.
- [ ] Add an adopted code of conduct.
- [ ] Add structured bug and feature issue forms plus template configuration.
- [ ] Add a PR template covering tests, changelog, performance/durability impact,
      privacy, and synthetic-only screenshots.
- [ ] Add restrained Dependabot configuration.

## 4. Durable project documentation

- [ ] Add `docs/README.md` as the documentation map.
- [ ] Add `docs/ARCHITECTURE.md`.
- [ ] Add `docs/PRIVACY.md`.
- [ ] Add `docs/BENCHMARKS.md` with reproducible evidence and claim boundaries.
- [ ] Add `docs/MODELS.md` and `THIRD_PARTY_NOTICES.md`.
- [ ] Add `[Unreleased]` to `CHANGELOG.md` and reconcile new work placement.
- [ ] Separate durable docs from completed implementation/review records where
      that can be done without losing references.

## 5. Verification and local delivery

- [ ] Frontend unit tests, TypeScript build, harness, and Storybook build pass.
- [ ] Rust tests, formatting, and Clippy pass on the declared toolchain.
- [ ] Hidden native E2E, accessibility, and durability checks pass.
- [ ] Relevant dependency and privacy checks pass or have explicit exceptions.
- [ ] Final working tree is clean with small conventional commits.
- [ ] Final app is built and installed locally; build stamp matches the final
      commit and the previous app remains recoverable through verification.
- [ ] Remaining GitHub mutations are listed for explicit maintainer approval.

