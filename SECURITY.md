# Security Policy

## Supported versions

Ember is currently source-build-only on Apple Silicon. Until signed releases
exist, security fixes target the current `main` branch.

| Version | Supported |
| --- | --- |
| Current `main` | Yes |
| Older commits, forks, and third-party binaries | No |

Do not trust an executable presented as an official public Ember build. The
project does not currently publish one.

## Dependency status

CI audits the npm production dependency graph and verifies registry signatures.
As of 2026-09-12, `npm audit --omit=dev` reports zero vulnerabilities and
`npm audit signatures` verifies all 878 installed packages (201 with registry
attestations).

The full npm graph is not clean: the same lockfile currently reports 24
vulnerable dev-tool package entries (18 high, 6 moderate), concentrated in the
optional Tauri MCP bridge, WebdriverIO/Tauri test harness, and Vitest/Storybook
chains. These tools are outside the npm production dependency graph, but they
still matter on contributor and CI machines. Some upstream fixes require
incompatible changes and the MCP chain has no complete current resolution, so
the project records this debt instead of using `npm audit fix --force` or
pretending it does not exist. Re-run both audit commands for current results;
Dependabot proposes bounded updates for review.

## Report a vulnerability privately

Do not open a public issue, pull request, discussion, or comment for a suspected
vulnerability.

The preferred channel is GitHub's
[private vulnerability reporting form](https://github.com/CarsonLMH/Ember/security/advisories/new).
It creates a private security advisory visible only to the reporter and the
repository's security team. Enabling private vulnerability reporting is a
pending maintainer repository-setting action; use the fallback below if the
form is not available yet.

If that form is unavailable, use a private contact method listed on the
[maintainer's GitHub profile](https://github.com/CarsonLMH) and initially send
only a request to establish a private reporting channel. Do not include the
vulnerability, proof of concept, affected component, or attachments in a
public message. If no private contact method is available, a public issue may
ask only for private security contact; it must contain no technical details.

## What to include

Once a private channel is established, include:

- the affected commit or source version;
- a clear description of the impact and the conditions required to trigger it;
- minimal reproduction steps using synthetic or clearly licensed public data;
- any mitigation you have already tested; and
- whether you believe active exploitation or data exposure has occurred.

Never send real photos, previews, face crops, face names or embeddings, an
Ember database, XMP sidecars, private EXIF, filenames, paths, or private logs.
Redact machine and user identifiers even in a private report unless they are
strictly necessary and the maintainer has asked for them.

Relevant security boundaries include journal durability, pair-atomic trash and
restore, JPEG/RAF metadata safety, local protocol access, path handling,
on-device face-data isolation and deletion, and the dependency/build chain.

## Disclosure

Please allow time to reproduce and fix the issue before public disclosure. The
maintainer will coordinate scope, remediation, and disclosure timing with the
reporter. Ember is a small project and does not promise a fixed response SLA,
but responsible reports will be handled as promptly as practical.
