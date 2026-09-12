import { spawnSync } from 'node:child_process';
import { readFileSync, statSync } from 'node:fs';
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

const claude = read('CLAUDE.md');
const codexConfig = read('.codex/config.toml');
const codexSkill = read('.agents/skills/ember-design-audit/SKILL.md');
const claudeMcp = JSON.parse(read('.mcp.json'));

check(
  statSync(resolve(root, 'AGENTS.md')).size <= 32 * 1024,
  'AGENTS.md stays within Codex\'s default 32 KiB project-instruction budget',
);
check(
  claude.trimStart().startsWith('@AGENTS.md'),
  'CLAUDE.md imports the shared AGENTS.md guidance',
);
check(
  codexSkill.includes('../../../.claude/skills/design-audit/SKILL.md'),
  'Codex design-audit entry point delegates to the canonical Claude workflow',
);
check(
  statSync(resolve(root, '.claude/skills/design-audit/SKILL.md')).isFile(),
  'canonical design-audit workflow exists',
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
  /\[mcp_servers\.tauri\][\s\S]*?command = "npx"[\s\S]*?@hypothesi\/tauri-mcp-server/.test(
    codexConfig,
  ),
  'Codex Tauri bridge uses the same server package as Claude',
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
