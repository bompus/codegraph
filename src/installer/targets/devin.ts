/**
 * Devin CLI / Devin Desktop target.
 *
 *   - MCP server entry to `~/.config/devin/mcp_config.json` (global;
 *     `%APPDATA%\devin\mcp_config.json` on Windows) or
 *     `./.devin/mcp_config.json` (local). Devin also honors a
 *     gitignored `.devin/mcp_config.local.json` personal override, but
 *     the shared project file is the local-install analog of
 *     `.cursor/mcp.json` — same tradeoff the other targets make.
 *   - Instructions to `~/.config/devin/AGENTS.md` (global — Devin's
 *     documented global-rules file) or `./AGENTS.md` (local — Devin
 *     reads project AGENTS.md by default).
 *
 * The MCP entry deliberately omits `type`: Devin's mcp_config schema
 * is `additionalProperties: false` (command / args / env / disabled /
 * url / headers / oauth — transport is inferred: a `command` means
 * stdio), so the shared `{type: 'stdio'}` shape would be invalid.
 *
 * No `--path` injection: Devin runs the session's agent with the
 * workspace as its working directory and passes the workspace roots
 * at MCP initialize, so the server's `process.cwd()` / rootUri
 * resolution finds `.codegraph/` on its own — the same reason Codex
 * and Claude need nothing extra.
 *
 * `mcp_config.json` is read at session start, so a new session is
 * required for the entry to connect — surfaced as an install note.
 *
 * No permissions surface managed here — Devin gates MCP tools through
 * its own `permissions` config (`mcp__codegraph__*` allow patterns);
 * `autoAllow` / `promptHook` are ignored.
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import {
  AgentTarget,
  DetectionResult,
  InstallOptions,
  Location,
  WriteResult,
} from './types';
import {
  getMcpServerConfig,
  jsonDeepEqual,
  readJsonFile,
  removeMarkedSection,
  upsertInstructionsEntry,
  writeJsonFile,
} from './shared';
import {
  CODEGRAPH_SECTION_END,
  CODEGRAPH_SECTION_START,
} from '../instructions-template';

/**
 * Devin's user config root, resolved exactly as documented:
 * `%APPDATA%\devin` on Windows, `~/.config/devin` everywhere else.
 * `XDG_CONFIG_HOME` is NOT honored — Devin's own docs and CLI help
 * name `~/.config/devin` literally (the env var appears only in its
 * shell-integration code), so writing anywhere else would be a
 * silent no-op.
 */
function userConfigDir(): string {
  if (process.platform === 'win32') {
    const appData = process.env.APPDATA && process.env.APPDATA.trim().length > 0
      ? process.env.APPDATA
      : path.join(os.homedir(), 'AppData', 'Roaming');
    return path.join(appData, 'devin');
  }
  return path.join(os.homedir(), '.config', 'devin');
}

function projectConfigDir(): string {
  return path.join(process.cwd(), '.devin');
}

function mcpConfigPath(loc: Location): string {
  return loc === 'global'
    ? path.join(userConfigDir(), 'mcp_config.json')
    : path.join(projectConfigDir(), 'mcp_config.json');
}

function instructionsPath(loc: Location): string {
  return loc === 'global'
    ? path.join(userConfigDir(), 'AGENTS.md')
    : path.join(process.cwd(), 'AGENTS.md');
}

class DevinTarget implements AgentTarget {
  readonly id = 'devin' as const;
  readonly displayName = 'Devin';
  readonly docsUrl = 'https://devin.ai';

  supportsLocation(_loc: Location): boolean {
    return true;
  }

  detect(loc: Location): DetectionResult {
    const file = mcpConfigPath(loc);
    const config = readJsonFile(file);
    const alreadyConfigured = !!config.mcpServers?.codegraph;
    // "Installed" heuristic: Devin's user config dir exists once the
    // CLI or Desktop has run; `~/.devin-server(-next)` is the Desktop
    // server's data dir on Linux/WSL. Local: the project counts once
    // it has a `.devin/` dir of its own.
    const installed = loc === 'global'
      ? fs.existsSync(userConfigDir()) ||
        fs.existsSync(path.join(os.homedir(), '.devin-server')) ||
        fs.existsSync(path.join(os.homedir(), '.devin-server-next'))
      : fs.existsSync(projectConfigDir());
    return { installed, alreadyConfigured, configPath: file };
  }

  install(loc: Location, _opts: InstallOptions): WriteResult {
    const files: WriteResult['files'] = [];

    files.push(writeMcpEntry(loc));
    // AGENTS.md gets the short marker-fenced CodeGraph block (#704):
    // subagents and non-MCP harnesses read AGENTS.md but never the MCP
    // initialize instructions. Upsert self-heals a stale pre-#529 block.
    files.push(upsertInstructionsEntry(instructionsPath(loc)));

    return {
      files,
      notes: ['Start a new Devin session for MCP changes to take effect.'],
    };
  }

  uninstall(loc: Location): WriteResult {
    const files: WriteResult['files'] = [];

    const file = mcpConfigPath(loc);
    const config = readJsonFile(file);
    if (config.mcpServers?.codegraph) {
      delete config.mcpServers.codegraph;
      if (Object.keys(config.mcpServers).length === 0) {
        delete config.mcpServers;
      }
      writeJsonFile(file, config);
      files.push({ path: file, action: 'removed' });
    } else {
      files.push({ path: file, action: 'not-found' });
    }

    files.push(removeInstructionsEntry(loc));

    return { files };
  }

  printConfig(loc: Location): string {
    const target = mcpConfigPath(loc);
    const snippet = JSON.stringify({ mcpServers: { codegraph: buildDevinMcpConfig() } }, null, 2);
    return `# Add to ${target}\n\n${snippet}\n`;
  }

  describePaths(loc: Location): string[] {
    return [mcpConfigPath(loc), instructionsPath(loc)];
  }
}

/**
 * Devin's documented stdio-server shape: `{command, args}` with the
 * transport inferred — no `type` key (the mcp_config schema is
 * `additionalProperties: false`, so an extra field would be invalid).
 */
function buildDevinMcpConfig(): { command: string; args: string[] } {
  const mcp = getMcpServerConfig();
  return { command: mcp.command, args: mcp.args };
}

function writeMcpEntry(loc: Location): WriteResult['files'][number] {
  const file = mcpConfigPath(loc);
  const existing = readJsonFile(file);
  const before = existing.mcpServers?.codegraph;
  const after = buildDevinMcpConfig();

  if (jsonDeepEqual(before, after)) {
    return { path: file, action: 'unchanged' };
  }
  const action: 'created' | 'updated' =
    before ? 'updated' : (fs.existsSync(file) ? 'updated' : 'created');
  if (!existing.mcpServers) existing.mcpServers = {};
  existing.mcpServers.codegraph = after;
  writeJsonFile(file, existing);
  return { path: file, action };
}

/**
 * Strip the marker-delimited CodeGraph block from this location's
 * AGENTS.md if a prior install wrote one. Used by both install
 * (self-heal on upgrade) and uninstall — see issue #529.
 */
function removeInstructionsEntry(loc: Location): WriteResult['files'][number] {
  const file = instructionsPath(loc);
  const action = removeMarkedSection(file, CODEGRAPH_SECTION_START, CODEGRAPH_SECTION_END);
  return { path: file, action };
}

export const devinTarget: AgentTarget = new DevinTarget();
