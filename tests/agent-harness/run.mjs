import { spawnSync } from 'node:child_process';
import { lstatSync, readFileSync, readlinkSync, realpathSync, statSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
let failures = 0;

function pass(message) {
  console.log(`PASS  ${message}`);
}

function fail(message) {
  failures += 1;
  console.error(`FAIL  ${message}`);
}

function check(condition, message) {
  if (condition) pass(message);
  else fail(message);
}

function read(relativePath) {
  return readFileSync(resolve(root, relativePath), 'utf8');
}

function compact(value) {
  return value.replaceAll(/\s+/g, ' ');
}

const claude = read('CLAUDE.md');
const codexConfig = read('.codex/config.toml');
const designAudit = read('.claude/skills/design-audit/SKILL.md');
const designAuditSurfaces = read('.claude/skills/design-audit/surfaces.md');
const compactDesignAudit = compact(designAudit);
const compactDesignAuditSurfaces = compact(designAuditSurfaces);
const claudeMcp = JSON.parse(read('.mcp.json'));
const packageJson = JSON.parse(read('package.json'));
const gateConfig = JSON.parse(read('src-tauri/tauri.gate.conf.json'));
const gateScript = read('scripts/gate.sh');
const e2eConfig = compact(read('e2e/wdio.conf.ts'));
const readmeFixtures = read('scripts/readme-fixtures.mjs');
const activePublicExamples = [
  '.claude/skills/design-audit/SKILL.md',
  '.claude/skills/design-audit/surfaces.md',
  'docs/FACES_DEVIATIONS.md',
  'src-tauri/src/facestore.rs',
  'src-tauri/src/lib.rs',
  'src/components/FaceBadges.tsx',
  'src/components/PeoplePanel.tsx',
].map(read).join('\n');

check(
  statSync(resolve(root, 'AGENTS.md')).size <= 32 * 1024,
  'AGENTS.md stays within Codex\'s default 32 KiB project-instruction budget',
);
check(
  claude.trimStart().startsWith('@AGENTS.md'),
  'CLAUDE.md imports the shared AGENTS.md guidance',
);
check(
  lstatSync(resolve(root, '.agents/skills/ember-design-audit')).isSymbolicLink() &&
    readlinkSync(resolve(root, '.agents/skills/ember-design-audit')) ===
      '../../.claude/skills/design-audit' &&
    realpathSync(resolve(root, '.agents/skills/ember-design-audit')) ===
      resolve(root, '.claude/skills/design-audit'),
  'Codex design-audit entry point is a relative symlink to the canonical workflow',
);
check(
  statSync(resolve(root, '.claude/skills/design-audit/SKILL.md')).isFile(),
  'canonical design-audit workflow exists',
);
check(
  compactDesignAudit.includes('Synthetic or clearly licensed public fixtures are the default.') &&
    compactDesignAudit.includes('Consent expires when this audit run ends.') &&
    compactDesignAudit.includes('Real-photo evidence stays local and is never published.') &&
    compactDesignAudit.includes(
      'Redact personal paths, EXIF values, face or person labels, and database contents',
    ),
  'design audits default to public-safe evidence and require one-run real-photo consent',
);
check(
  designAudit.includes('scripts/e2e-fixtures.mjs') &&
    designAudit.includes('"identifier":"com.cleung.ember.design-audit"') &&
    designAudit.indexOf('### 2. Create the default synthetic fixture') <
      designAudit.indexOf('### 3. Optional real-photo inspection'),
  'design-audit procedure creates an isolated synthetic fixture before private-data mode',
);
check(
  compactDesignAudit.includes('Do not publish it with the Artifact tool') &&
    compactDesignAudit.includes('Do not save private evidence or identifiers to memory'),
  'real-photo audit reports cannot enter public artifacts or durable agent memory',
);
check(
  compactDesignAuditSurfaces.includes('Real-data lookup is an opt-in fallback, never setup.') &&
    compactDesignAuditSurfaces.includes(
      'Do not inspect the user keymap or database in synthetic mode.',
    ),
  'surface lookup guidance cannot select private data by default',
);
check(
  !/\bnati\b/i.test(activePublicExamples) &&
    !/\/Users\/[A-Za-z0-9._-]+/.test(activePublicExamples),
  'active contributor-facing examples omit maintainer names and workstation paths',
);

const gateWindow = gateConfig.app?.windows?.find((window) => window.label === 'main');
check(
  gateWindow?.create === false &&
    gateWindow?.visible === false &&
    gateWindow?.focus === false &&
    gateWindow?.backgroundThrottling === 'disabled',
  'real-photo gates use a strict off-screen window configuration',
);
check(
  gateScript.includes('src-tauri/tauri.gate.conf.json'),
  'real-photo gate script uses the strict off-screen configuration',
);
check(
  packageJson.scripts?.['docs:screenshot']?.includes('EMBER_E2E_README=1') &&
    e2eConfig.includes(
      "const VISIBLE = !README && (PERF || process.env.EMBER_E2E_VISIBLE === '1');",
    ) &&
    readmeFixtures.includes("'e2e', 'fixtures', 'readme'") &&
    !readmeFixtures.includes('process.argv'),
  'README screenshots use a fixed private-data-free fixture and can never reveal the test window',
);

const claudeServers = Object.keys(claudeMcp.mcpServers ?? {})
  .map((name) => name.replaceAll('-', '_'))
  .sort();
const codexServers = [...codexConfig.matchAll(/^\[mcp_servers\.([A-Za-z0-9_-]+)\]$/gm)]
  .map((match) => match[1].replaceAll('-', '_'))
  .sort();

check(
  JSON.stringify(codexServers) === JSON.stringify(claudeServers),
  `Claude and Codex expose the same MCP servers (${claudeServers.join(', ')})`,
);
check(
  claudeMcp.mcpServers?.tauri?.command === './node_modules/.bin/mcp-server-tauri' &&
    Array.isArray(claudeMcp.mcpServers?.tauri?.args) &&
    claudeMcp.mcpServers.tauri.args.length === 0 &&
    /\[mcp_servers\.tauri\][\s\S]*?command = "\.\/node_modules\/\.bin\/mcp-server-tauri"[\s\S]*?args = \[\]/.test(
      codexConfig,
    ),
  'Claude and Codex use the project-local Tauri MCP binary without mutable fetches',
);
check(
  /\[mcp_servers\.ember_storybook\][\s\S]*?url = "http:\/\/127\.0\.0\.1:6006\/mcp"/.test(
    codexConfig,
  ),
  'Codex Storybook endpoint matches the project server',
);

const codex = spawnSync('codex', ['mcp', 'list'], {
  cwd: root,
  encoding: 'utf8',
});
check(codex.status === 0, 'Codex accepts the project MCP configuration');
if (codex.status === 0) {
  const output = codex.stdout.replaceAll('-', '_');
  check(output.includes('tauri'), 'Codex discovers the Tauri MCP server');
  check(output.includes('ember_storybook'), 'Codex discovers the Storybook MCP server');
} else if (codex.stderr) {
  console.error(codex.stderr.trim());
}

const whitespace = spawnSync('git', ['diff', '--check'], {
  cwd: root,
  encoding: 'utf8',
});
check(whitespace.status === 0, 'tracked changes pass git diff --check');
if (whitespace.status !== 0 && whitespace.stdout) {
  console.error(whitespace.stdout.trim());
}

if (failures > 0) {
  console.error(`\n${failures} agent-harness check(s) failed.`);
  process.exit(1);
}

console.log('\nAgent harness is aligned for Claude Code and Codex.');
