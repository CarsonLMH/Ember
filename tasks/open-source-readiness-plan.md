# Open-source readiness plan

## Outcome

Turn Ember's public repository into a credible, contributor-ready project whose
front page matches the quality of the app. Credibility must come from verifiable
signals: green checks, honest platform and license badges, a privacy-safe product
image, clear contribution paths, documented architecture, and a release story
that does not overpromise.

This work is local until the maintainer explicitly approves a push or GitHub
settings changes. The first public installation path remains source-build-only;
signed and notarized distribution is a later release milestone.

## Audit summary

- The repository is already public, but its README reads like an internal
  operator note and exposes maintainer-only commands.
- Public CI is red because it follows floating Rust stable while the local
  toolchain is older. The repository does not declare its Rust or Node toolchain.
- The root license is sound, but contribution, security, conduct, dependency,
  model-provenance, privacy, architecture, and benchmark documentation is thin
  or absent.
- The design-audit agent workflow defaults to selecting and screenshotting real
  photos from the maintainer's library. That is incompatible with a safe public
  contributor harness.
- No personal photos are tracked. Current tracked prose and fixture names do,
  however, contain personal names and workstation paths that should not be the
  public examples.
- There is no tagged release even though version files say 1.4.0. The changelog
  also mixes later work into the dated 1.4.0 section.
- The public name `Ember` collides with existing software and a current macOS
  photo/video app. Public-facing renaming needs an explicit maintainer decision.

## Decisions

- **Substance before decoration.** Fix CI and privacy before adding the CI badge
  or marketing screenshot.
- **Three badges at launch.** CI, Apple Silicon/macOS, and MIT license. Do not
  add release, coverage, downloads, stars, security, or performance badges until
  those claims have durable evidence.
- **Source build only for now.** Do not instruct people to bypass Gatekeeper.
  A downloadable build waits for signing, notarization, checksums, and a real
  GitHub Release.
- **No private media in public evidence.** Public screenshots and issue reports
  use synthetic or clearly licensed fixtures. Real-photo audits require explicit
  one-run consent and stay local.
- **Do not rewrite history by default.** Clean current files and configure safer
  authorship going forward. History rewriting is a separate, disruptive choice.
- **Keep the dual agent harness.** `AGENTS.md`, `CLAUDE.md`, `.codex/`, and the
  shared design-audit skill are useful public engineering assets once their
  privacy boundary is safe.

## Work sequence

### Phase 1: Truth and privacy gate

1. Pin the supported Rust and Node toolchains and make CI use the repository's
   declared versions.
2. Fix the current Rust/Clippy drift without suppressing a useful lint.
3. Expand ignore rules for secrets, local databases, real photo formats, sidecar
   metadata, signing material, and named local fixtures.
4. Make the design-audit workflow synthetic/public-fixture-first. Require
   explicit consent for a one-run real-photo inspection; prohibit publishing
   the media, paths, EXIF, face labels, or database contents.
5. Replace personal example names and absolute workstation paths in active
   contributor-facing material where doing so does not erase necessary design
   reasoning.
6. Add lightweight dependency checks that distinguish shipped dependencies
   from dev-tool debt. Record rather than conceal any remaining exceptions.

### Checkpoint A: Repository truth

- A clean checkout declares enough toolchain information to reproduce CI.
- Local CI-equivalent checks pass without warning suppressions.
- Public contributor instructions cannot select or disclose private photos by
  default.
- No ignored real-photo, database, credential, or signing artifact can be added
  accidentally through the ordinary workflow.

### Phase 2: Public front door

7. Rewrite `README.md` around the product: concise positioning, honest badges,
   a safe screenshot, why it exists, core guarantees, current status,
   requirements, source setup, architecture, contributing, roadmap, and license.
8. Keep benchmark wording precise: Ember is engineered to a p99 <= 50 ms flip
   budget, while measured claims link to reproducible hardware/fixture results.
9. Remove `scripts/ship.sh` and real-photo gate commands from the public quick
   start. Keep those documented as maintainer workflows, not contributor setup.
10. Add source and license attribution for every public screenshot fixture.

### Checkpoint B: Credible first impression

- A new visitor can understand the product, supported machine, maturity, privacy
  model, and build path without reading internal specs.
- Every badge resolves to a real signal.
- No screenshot or setup step depends on private data.

### Phase 3: Contribution rails

11. Add `CONTRIBUTING.md`, `SECURITY.md`, and `CODE_OF_CONDUCT.md`.
12. Add structured bug and feature forms, issue-template configuration, and a
    pull-request template. Explicitly prohibit attaching real photos, Ember
    databases, face data, or logs containing local paths to public issues.
13. Add Dependabot configuration with a restrained update cadence.
14. Give contributors exact commands for focused checks and explain which
    performance, durability, and real-photo gates remain maintainer-only.

### Phase 4: Durable technical documentation

15. Add a docs index plus architecture, privacy, benchmarks, and model-provenance
    pages. Add third-party notices for models, runtime components, and fixtures.
16. Introduce an `[Unreleased]` changelog section and stop placing new work under
    the dated 1.4.0 heading. Do not invent historical tags.
17. Separate durable contributor docs from review transcripts and completed task
    plans without breaking useful historical references.

### Phase 5: Verification and handoff

18. Run the repository's unit, TypeScript, harness, Storybook, Rust, hidden E2E,
    accessibility, durability, and audit checks that are safe and relevant.
19. Build and install the final local app from the final commit, preserving the
    previous installed app until launch verification succeeds.
20. Present the maintainer with the exact optional GitHub actions still requiring
    approval: push, topics/description, protection rules, security settings,
    Discussions choice, and first tagged release.

## GitHub actions deliberately deferred

These mutate the public repository and therefore are not part of the local
implementation without fresh approval:

- pushing commits;
- changing repository name, description, topics, or homepage;
- enabling Discussions or security features;
- adding branch/ruleset protection;
- creating issues, tags, releases, or downloadable binaries.

## Main risks

| Risk | Consequence | Control |
| --- | --- | --- |
| Cosmetic badges precede working checks | Visitors see red or hollow credibility signals | Land badges only after local CI-equivalent verification; push only with approval. |
| Name change expands into bundle/data migration | Existing local data or automation breaks | Treat public display name separately; do no identifier or data migration without a dedicated plan. |
| Privacy cleanup erases useful technical evidence | Face decisions become harder to understand | Neutralize identities and paths while retaining measurements and design rationale. |
| Dependency cleanup destabilizes the native harness | Verification becomes less reliable | Upgrade in small groups; never use forced audit rewrites; document bounded exceptions. |
| CI claims more than it can reproduce | Green badge gives false confidence | Separate deterministic CI checks from maintainer-only real-photo/performance gates. |
| Public binary is blocked or appears unsafe | Poor first-run trust | Remain source-build-only until signing and notarization are ready. |

## Open maintainer decision

- Public product name: keep `Ember`, or use a more distinctive interim name such
  as `Ember Culler`. This pass will not change bundle identifiers, data paths, or
  code-level names either way.
