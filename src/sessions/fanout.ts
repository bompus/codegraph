/**
 * Experimental: one session search over several projects.
 *
 * Each project keeps its own `.codegraph/sessions-v2.db`; this runs the ordinary
 * per-project query on every listed project and merges the answers. Nothing is
 * stored across projects, and a query here refreshes the other projects' own
 * session indexes exactly as a query inside them would. BM25 ranks are only
 * comparable within one index, so the merge interleaves by per-project rank.
 *
 * Self-contained so the experiment can be removed by deleting this file and its
 * callers (the `projects` argument of `codegraph_sessions`, the `--project`
 * option of `codegraph sessions`).
 */
import * as path from 'path';
import {
  FULL_PASSAGE_BUDGET,
  NoSessionsError,
  querySessions,
  withinPassageBudget,
  type SessionHit,
  type SessionSearchOptions,
} from './index';

/** At most this many projects are searched in one call. */
export const MAX_PROJECTS = 5;

export interface ProjectHit extends SessionHit {
  /** Root of the project whose index holds this hit. */
  project: string;
}

export interface ProjectSummary {
  root: string;
  name: string;
  /** Hits this project contributed to the merged list. */
  hits: number;
  /** Transcripts its index covers. */
  files: number;
  /** Why this project was not searched. */
  skipped?: string;
}

export interface CrossProjectResult {
  projects: ProjectSummary[];
  hits: ProjectHit[];
  /** True when some project had no passage holding every word. */
  fallback?: boolean;
  fullCut?: number;
  contextCut?: number;
  /** Listed projects beyond {@link MAX_PROJECTS} that were not searched. */
  dropped?: number;
}

/** A listed project, or the reason it cannot be searched. */
export type ProjectTarget = { root: string } | { path: string; skipped: string };

/**
 * Searches each target and merges: the best hit of every project, then the next
 * best of every project, until `limit`. A project that cannot answer is listed
 * with the reason and never fails the call.
 */
export function querySessionsAcross(
  targets: readonly ProjectTarget[],
  query: string,
  opts: SessionSearchOptions = {},
): CrossProjectResult {
  const limit = opts.limit ?? 10;
  const seen = new Set<string>();
  const listed: ProjectTarget[] = [];
  for (const t of targets) {
    const key = 'root' in t ? t.root : t.path;
    if (seen.has(key)) continue;
    seen.add(key);
    listed.push(t);
  }
  const projects: ProjectSummary[] = [];
  const perProject: ProjectHit[][] = [];
  let fallback = false;
  for (const target of listed.slice(0, MAX_PROJECTS)) {
    if (!('root' in target)) {
      projects.push({ root: target.path, name: path.basename(target.path), hits: 0, files: 0, skipped: target.skipped });
      continue;
    }
    try {
      const r = querySessions(target.root, query, opts);
      if (r.fallback) fallback = true;
      projects.push({ root: target.root, name: path.basename(target.root), hits: 0, files: r.index.files });
      perProject.push(r.hits.map((h) => ({ ...h, project: target.root })));
    } catch (err) {
      const skipped = err instanceof NoSessionsError ? 'no session transcripts, or sessions are turned off' : `search failed: ${err instanceof Error ? err.message : String(err)}`;
      projects.push({ root: target.root, name: path.basename(target.root), hits: 0, files: 0, skipped });
    }
  }
  const hits: ProjectHit[] = [];
  for (let rank = 0; hits.length < limit; rank++) {
    let any = false;
    for (const list of perProject) {
      const hit = list[rank];
      if (!hit) continue;
      any = true;
      if (hits.length < limit) hits.push(hit);
    }
    if (!any) break;
  }
  for (const h of hits) {
    const summary = projects.find((p) => p.root === h.project);
    if (summary) summary.hits++;
  }
  const out: CrossProjectResult = { projects, hits };
  if (fallback) out.fallback = true;
  if (listed.length > MAX_PROJECTS) out.dropped = listed.length - MAX_PROJECTS;
  if (opts.full || opts.context) {
    // Each project applied its own byte budget; one budget covers the merged list.
    const { fullCut, contextCut } = withinPassageBudget(hits);
    out.fullCut = fullCut;
    if (contextCut > 0) out.contextCut = contextCut;
  }
  return out;
}

/** The text the CLI and the MCP tool print for a cross-project search. */
export function formatCrossProjectHits(query: string, result: CrossProjectResult): string {
  const { hits, projects } = result;
  const searched = projects.filter((p) => !p.skipped);
  const lines = [
    `Sessions matching "${query}" across ${searched.length} project${searched.length === 1 ? '' : 's'} (experimental) — ${hits.length} hit${hits.length === 1 ? '' : 's'}:`,
    ...projects.map((p) => (p.skipped ? `- ${p.name} (${p.root}): skipped, ${p.skipped}` : `- ${p.name} (${p.root}): ${p.hits} hit${p.hits === 1 ? '' : 's'} shown, ${p.files} transcript${p.files === 1 ? '' : 's'} indexed`)),
  ];
  if (result.dropped) lines.push(`Only the first ${MAX_PROJECTS} projects are searched; ${result.dropped} more were left out.`);
  lines.push('Hits are interleaved by each project\'s own ranking, best of every project first, because scores are not comparable across projects.', '');
  if (hits.length === 0) {
    lines.push('No transcript prose in these projects holds any of these words. A stem ("merge" also finds "merged") or a different term may find it.');
    return lines.join('\n');
  }
  if (result.fallback) lines.push('Some projects had no passage holding every word; their hits match some of the words.', '');
  for (const h of hits) {
    const title = h.title ? ` · ${h.title}` : '';
    lines.push(`## ${path.basename(h.project)} · ${h.session}${title}`);
    lines.push(`${h.role} · ${h.ts} · ${h.file}${h.partial ? ' · some words' : ''}`);
    if (h.text === undefined) lines.push(h.snippet.replace(/\s+/g, ' ').trim());
    else if (h.before || h.after) {
      const quote = (p: { text: string }) => p.text.split('\n').map((l) => `> ${l}`);
      const label = (what: string, p: { role: string; ts: string }) => `${what} · ${p.role} · ${p.ts}`;
      for (const p of h.before ?? []) lines.push(label('before', p), ...quote(p), '');
      lines.push('match', ...quote(h as { text: string }));
      for (const p of h.after ?? []) lines.push('', label('after', p), ...quote(p));
    } else lines.push(...h.text.split('\n').map((l) => `> ${l}`));
    lines.push('');
  }
  if (result.fullCut) {
    lines.push(`Whole passages are limited to ${FULL_PASSAGE_BUDGET.toLocaleString('en-US')} bytes in all, across projects: the last ${result.fullCut} hit${result.fullCut === 1 ? '' : 's'} show only the matching snippet.`, '');
  }
  if (result.contextCut) {
    lines.push(`The byte budget left out ${result.contextCut} neighbouring passage${result.contextCut === 1 ? '' : 's'}; the nearest ones are shown.`, '');
  }
  lines.push('A hit names its project and session id; the path after the timestamp is the transcript to read when the snippet is not enough.');
  return lines.join('\n');
}
