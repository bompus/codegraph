/**
 * Cargo Workspace Resolver Helper
 *
 * Parses a project's root Cargo.toml and member crate manifests to
 * build a crate-name -> member-directory map. Used by the Rust
 * resolver to resolve `use crate_name::...` references that point
 * into workspace member crates.
 */

import picomatch from 'picomatch';
import { ResolutionContext } from '../types';

const GLOB_CHARS = /[*?[\]{}!]/;
const SKIP_DIRS = new Set(['target', 'node_modules', '.git', 'dist', 'build']);
const MAX_GLOB_WALK_DEPTH = 5;

function getSection(content: string, sectionName: string): string | null {
  const lines = content.split('\n');
  let inSection = false;
  const sectionLines: string[] = [];

  for (const line of lines) {
    const trimmed = line.trim();
    if (!inSection) {
      if (trimmed === `[${sectionName}]`) {
        inSection = true;
      }
      continue;
    }

    if (/^\[[^\]]+\]$/.test(trimmed)) {
      break;
    }

    sectionLines.push(line);
  }

  if (!inSection) return null;
  return sectionLines.join('\n');
}

function extractQuotedValues(valueList: string): string[] {
  const values: string[] = [];
  let quote: '"' | "'" | null = null;
  let escaped = false;
  let current = '';

  for (const ch of valueList) {
    if (!quote) {
      if (ch === '"' || ch === "'") {
        quote = ch;
        current = '';
      }
      continue;
    }

    if (escaped) {
      current += ch;
      escaped = false;
      continue;
    }

    if (ch === '\\') {
      escaped = true;
      continue;
    }

    if (ch === quote) {
      values.push(current.trim());
      quote = null;
      current = '';
      continue;
    }

    current += ch;
  }

  return values.filter(Boolean);
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function getArrayValue(section: string, key: string): string | null {
  const keyRegex = new RegExp(`\\b${escapeRegExp(key)}\\b\\s*=`, 'm');
  const keyMatch = keyRegex.exec(section);
  if (!keyMatch) return null;

  let i = keyMatch.index + keyMatch[0].length;
  while (i < section.length && /\s/.test(section.charAt(i))) i++;
  if (section.charAt(i) !== '[') return null;
  i++;

  let inQuote: '"' | "'" | null = null;
  let escaped = false;
  let depth = 1;
  const start = i;

  while (i < section.length) {
    const ch = section.charAt(i);

    if (inQuote) {
      if (escaped) {
        escaped = false;
      } else if (ch === '\\') {
        escaped = true;
      } else if (ch === inQuote) {
        inQuote = null;
      }
      i++;
      continue;
    }

    if (ch === '"' || ch === "'") {
      inQuote = ch;
      i++;
      continue;
    }

    if (ch === '[') {
      depth++;
      i++;
      continue;
    }

    if (ch === ']') {
      depth--;
      if (depth === 0) {
        return section.slice(start, i);
      }
      i++;
      continue;
    }

    i++;
  }

  return null;
}

export function parseWorkspaceMembers(cargoToml: string): string[] {
  const workspaceSection = getSection(cargoToml, 'workspace');
  if (!workspaceSection) return [];
  const membersValue = getArrayValue(workspaceSection, 'members');
  if (!membersValue) return [];
  return extractQuotedValues(membersValue);
}

function parsePackageName(cargoToml: string): string | null {
  const packageSection = getSection(cargoToml, 'package');
  if (!packageSection) return null;
  const packageNameMatch = packageSection.match(/name\s*=\s*["']([^"'\n]+)["']/);
  return packageNameMatch?.[1]?.trim() ?? null;
}

function addCrateAlias(map: Map<string, string>, crateName: string, memberPath: string): void {
  const normalized = crateName.replace(/-/g, '_');
  map.set(crateName, memberPath);
  if (normalized !== crateName) {
    map.set(normalized, memberPath);
  }
}

function cleanPath(memberPath: string): string {
  return memberPath.replace(/\\/g, '/').replace(/\/$/, '');
}

function expandGlobMember(member: string, context: ResolutionContext): string[] {
  if (!context.listDirectories) return [];

  const firstGlobIdx = member.search(GLOB_CHARS);
  const staticPrefix = member
    .slice(0, firstGlobIdx)
    .replace(/[^/]*$/, '')
    .replace(/\/$/, '');

  const matcher = picomatch(member, { dot: false });
  const matches: string[] = [];
  const seen = new Set<string>();

  function walk(dir: string, depth: number): void {
    if (depth > MAX_GLOB_WALK_DEPTH) return;
    const children = context.listDirectories!(dir);
    for (const child of children) {
      if (SKIP_DIRS.has(child) || child.startsWith('.')) continue;
      const rel = dir === '.' ? child : `${dir}/${child}`;
      if (matcher(rel) && !seen.has(rel)) {
        seen.add(rel);
        matches.push(rel);
      }
      walk(rel, depth + 1);
    }
  }

  walk(staticPrefix || '.', 0);
  return matches;
}

function expandMembers(members: string[], context: ResolutionContext): string[] {
  const expanded: string[] = [];
  const seen = new Set<string>();
  for (const member of members) {
    const candidates = GLOB_CHARS.test(member)
      ? expandGlobMember(member, context)
      : [member];
    for (const candidate of candidates) {
      const cleaned = cleanPath(candidate);
      if (seen.has(cleaned)) continue;
      seen.add(cleaned);
      expanded.push(cleaned);
    }
  }
  return expanded;
}

/**
 * What resolution reads from a Cargo manifest: the workspace's members, the
 * package's name and what it depends on by name, package and path. A version
 * bump is none of that (upstream 9d3abb84 / f72c9d2e).
 */
export function cargoManifestFields(text: string): string {
  return JSON.stringify([parseWorkspaceMembers(text), parsePackageName(text), rustManifestDependencies(text)]);
}

interface ManifestDependency {
  key: string;
  pkg: string;
  path?: string;
  workspace: boolean;
  kind: 'normal' | 'dev' | 'build' | 'workspace';
}

/** A manifest's dependencies, normalizing `-` to `_` as `use` paths write them. */
function rustManifestDependencies(text: string): ManifestDependency[] {
  const deps = new Map<string, ManifestDependency>();
  const entry = (kind: ManifestDependency['kind'], name: string): ManifestDependency => {
    const key = name.replace(/-/g, '_');
    let dep = deps.get(`${kind} ${key}`);
    if (!dep) deps.set(`${kind} ${key}`, (dep = { key, pkg: key, workspace: false, kind }));
    return dep;
  };
  const read = (dep: ManifestDependency, fields: string): void => {
    const pkg = /\bpackage\s*=\s*"([^"]+)"/.exec(fields)?.[1];
    if (pkg) dep.pkg = pkg.replace(/-/g, '_');
    const at = /\bpath\s*=\s*"([^"]+)"/.exec(fields)?.[1];
    if (at) dep.path = at.replace(/\\/g, '/');
    if (/\bworkspace\s*=\s*true\b/.test(fields)) dep.workspace = true;
  };
  let kind: ManifestDependency['kind'] | null = null;
  let table: ManifestDependency | null = null;
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.replace(/#.*$/, '').trim();
    const header = /^\[([^\]]+)\]$/.exec(line);
    if (header) {
      const m = /^(?:(workspace)\.|target\..+\.)?(dev-|build-)?dependencies(?:\.([A-Za-z0-9_-]+))?$/.exec(header[1]!.trim());
      const of = !m ? null : m[1] ? 'workspace' : m[2] === 'dev-' ? 'dev' : m[2] === 'build-' ? 'build' : 'normal';
      kind = of !== null && !m![3] ? of : null;
      table = of !== null && m![3] ? entry(of, m![3]) : null;
      continue;
    }
    if (table) {
      read(table, line);
      continue;
    }
    const dep = kind === null ? null : /^"?([A-Za-z0-9_-]+)"?\s*(?:\.\s*([A-Za-z_-]+)\s*)?=\s*(.*)$/.exec(line);
    if (dep) read(entry(kind!, dep[1]!), dep[2] ? `${dep[2]} = ${dep[3]}` : dep[3]!);
  }
  return [...deps.values()];
}

/**
 * Build a map from crate-name aliases to workspace member directory paths.
 * Example: "mytool-core" and "mytool_core" -> "crates/mytool-core"
 *
 * Supports glob members (e.g. `members = ["crates/*"]`) via picomatch
 * when the context exposes `listDirectories`.
 */
export function getCargoWorkspaceCrateMap(context: ResolutionContext): Map<string, string> {
  return context.getCargoCrates?.() ?? readCargoWorkspaceCrateMap(context);
}

/** The crate map as the resolver keeps it, read from the manifests on disk. */
export function readCargoWorkspaceCrateMap(context: ResolutionContext): Map<string, string> {
  const result = new Map<string, string>();
  const rootCargoToml = context.readFile('Cargo.toml');
  if (!rootCargoToml) return result;

  const rawMembers = parseWorkspaceMembers(rootCargoToml);
  const members = expandMembers(rawMembers, context);

  for (const memberPath of members) {
    const memberCargoPath = `${memberPath}/Cargo.toml`;
    const memberCargoToml = context.readFile(memberCargoPath);
    if (!memberCargoToml) continue;

    const packageName = parsePackageName(memberCargoToml);
    if (!packageName) continue;

    addCrateAlias(result, packageName, memberPath);
  }

  return result;
}
