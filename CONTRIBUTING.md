# Contributing to Ember

Thanks for taking an interest in Ember. It is a deliberately focused macOS
photo-culling tool: speed and verdict durability come before feature breadth.
Contributions that protect that focus are welcome.

## Before you start

- Read [SPEC.md](SPEC.md) for confirmed product behavior and [AGENTS.md](AGENTS.md)
  for the engineering constraints.
- Search existing issues before opening a new one.
- Use the feature-request form before investing in a substantial new workflow.
  A small, well-reproduced bug fix can go straight to a pull request.
- Keep changes narrow. Refactors, generated rewrites, and unrelated cleanup
  should not ride along with a feature or fix.

## Privacy: public contributions use public-safe data

Ember works with personal photo libraries and biometric face data. Public
issues, pull requests, commits, CI artifacts, and screenshots must never
contain:

- real personal photos or derivatives such as previews, thumbnails, face chips,
  crops, or screenshots showing them;
- an Ember SQLite database, Application Support data, caches, face names, or
  embeddings;
- XMP sidecars or copied EXIF metadata from a private library;
- private filenames, folder paths, home-directory paths, volume names, or other
  identifying metadata; or
- private app, terminal, crash, gate, or agent logs.

Use synthetic data or a clearly licensed public fixture. Replace paths and
filenames with neutral placeholders, and inspect every attachment before
posting it. Sanitized aggregate timings such as p99 latency are fine; the
fixture, raw logs, and source paths are not.

If a report may describe a vulnerability, follow [SECURITY.md](SECURITY.md)
instead of opening a public issue.

## Local source build

Ember is currently source-build-only. There is no signed or notarized public
binary. The supported development machine is an Apple Silicon Mac.

Prerequisites:

- macOS on Apple Silicon with Xcode Command Line Tools;
- Node.js 22 (the CI version is in `.node-version`);
- npm at the version declared by `packageManager` in `package.json`;
- rustup (the compiler and components are pinned in `rust-toolchain.toml`); and
- ExifTool (`brew install exiftool`).

Set up the repository:

```sh
git clone https://github.com/CarsonLMH/Ember.git
cd Ember
corepack enable npm
npm ci
npm run tauri dev
```

`npm run tauri build` creates an unsigned local app bundle. Do not present that
bundle as an official Ember release.

## Engineering rules that matter most

- The preloaded culling loop must remain at or below 50 ms p99 with zero
  steady-state cache-miss serves; cold folder open must remain at or below one
  second.
- A verdict is journaled before the UI acknowledges it.
- Never modify a RAF file. RAF metadata belongs in an `.xmp` sidecar.
- JPEG metadata writes must be atomic and keep the encoded image payload
  byte-identical.
- Pair trash must not silently leave a half-moved pair: rollback a partial
  move, and report both recovery paths if macOS also refuses the rollback.
- Keep full-resolution decoding out of the normal flip path.

The complete architecture and face-data invariants live in [AGENTS.md](AGENTS.md).
If a proposed change conflicts with them, discuss the conflict before coding.

## Testing tiers

Run the smallest relevant tier while developing, then all affected tiers before
opening a pull request. CI runs the deterministic source checks.

### Source checks

```sh
npm run test:harness
npm test
npm run build
npm run storybook:build
cd src-tauri
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

### Hidden synthetic app checks

These use an isolated synthetic database and do not activate an app window:

```sh
npm run e2e:build
npm run e2e
```

### Explicitly visible checks

`npm run e2e:visual` and `npm run e2e:perf` can take focus. Run them only when
the change needs them and after `npm run e2e:build`. Visual baselines may be
updated only after inspecting the actual output and deciding the change is
correct.

### Private real-photo release gates

Performance, persistence, filesystem, preview/protocol, and hot-path changes
also need the local real-photo gates:

```sh
./scripts/checks.sh /absolute/path/to/a/private-disposable-fixture
```

This command interrupts matching development processes. It requires a fixture
you are authorized to use and must never be pointed at a production photo
library. Do not publish the fixture, raw output, or its path. Contributors who
cannot run this tier should say so in the pull request; the maintainer will run
it before accepting an affected change.

## Pull requests

- Use a short branch and conventional commits such as `fix:`, `feat:`, `test:`,
  or `docs:`.
- Add a line to `CHANGELOG.md` for every user-visible change.
- Add or update tests for behavior changes. Do not weaken assertions, suppress
  lint, or skip a failing check to reach green.
- Explain risk, rollback, and every check not run.
- Keep screenshots synthetic or clearly licensed and state the fixture's
  provenance.

The maintainer performs final acceptance on real Fujifilm photos. A pull
request can be technically complete before that step, but it is not considered
shipped until the real workflow is accepted.

By contributing, you agree that your contribution is licensed under the
[MIT License](LICENSE) and that you will follow the
[Code of Conduct](CODE_OF_CONDUCT.md).
