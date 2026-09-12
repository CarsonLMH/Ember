# Open-source readiness checklist

## 1. Truth and privacy gate

- [x] Declare and document the supported Rust toolchain.
- [x] Declare Node 22 and the package-manager version.
- [x] Make CI consume those declarations and pass current Clippy without a lint
      suppression.
- [x] Extend `.gitignore` for secrets, credentials, databases, signing files,
      RAF/RAW photos, XMP sidecars, and local fixture folders.
- [x] Make the shared design-audit skill synthetic/public-fixture-first.
- [x] Require explicit, one-run consent for any real-photo audit and prohibit
      publishing private media or metadata.
- [x] Neutralize personal example names and absolute maintainer paths in active
      public contributor material.
- [x] Add production dependency audit coverage and document unresolved dev-tool
      audit findings without forced upgrades.

### Checkpoint A

- [x] Phase-1 toolchain and CI-equivalent checks passed locally at that
      checkpoint; final combined verification remains in section 5.
- [x] Privacy-focused harness checks pass.
- [x] `git check-ignore` proves representative private artifacts are excluded.

## 2. Public front door

- [ ] Resolve the public-name decision with the maintainer.
- [x] Rewrite `README.md` for product comprehension and contributor onboarding.
- [x] Add only CI, platform, and license badges.
- [x] Add a synthetic or public-domain product screenshot with attribution.
- [x] State maturity, Apple Silicon requirement, source-build status, and current
      distribution limits plainly.
- [x] Link to architecture, privacy, benchmark, model, contribution, security,
      roadmap/spec, changelog, and license material.
- [x] Keep maintainer-only shipping and real-photo commands out of quick start.

### Checkpoint B

- [x] README links and badge targets resolve.
- [x] A clean-checkout setup path is internally consistent.
- [x] No public visual or example exposes private photos, EXIF, real-library
      face labels, or workstation paths.

## 3. Contribution rails

- [x] Add `CONTRIBUTING.md` with setup, testing tiers, change rules, and PR flow.
- [x] Add `SECURITY.md` with a private reporting route and supported-version
      policy.
- [x] Add an adopted code of conduct.
- [x] Add structured bug and feature issue forms plus template configuration.
- [x] Add a PR template covering tests, changelog, performance/durability impact,
      privacy, and synthetic-only screenshots.
- [x] Add restrained Dependabot configuration.

## 4. Durable project documentation

- [x] Add `docs/README.md` as the documentation map.
- [x] Add `docs/ARCHITECTURE.md`.
- [x] Add `docs/PRIVACY.md`.
- [x] Add `docs/BENCHMARKS.md` with reproducible evidence and claim boundaries.
- [x] Add `docs/MODELS.md` and `THIRD_PARTY_NOTICES.md`.
- [x] Add `[Unreleased]` to `CHANGELOG.md` and reconcile new work placement.
- [x] Separate durable docs from completed implementation/review records where
      that can be done without losing references.

## 5. Verification and local delivery

- [x] Frontend unit tests, TypeScript build, harness, and Storybook build pass.
- [x] Rust tests, formatting, and Clippy pass on the declared toolchain.
- [x] Hidden native E2E, accessibility, and durability checks pass.
- [x] Relevant dependency and privacy checks pass or have explicit exceptions.
- [x] Final working tree is clean with small conventional commits.
- [x] Final app is built and installed locally; build stamp matches the final
      commit and the previous app remains recoverable through verification.
- [x] Remaining GitHub mutations are listed for explicit maintainer approval.
