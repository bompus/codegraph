/**
 * The cheap first half of `codegraph prompt-hook`: payload parsing and the
 * gates that need no index open.
 *
 * The hook runs on every prompt and most prompts are no-ops, so the CLI entry
 * calls {@link precheckPromptHook} before it loads commander, the engine or any
 * command code, and exits at once when the hook has nothing to add. Keep this
 * module's imports light.
 */
import * as fs from 'fs';
import {
  extractCodeTokens,
  hasStructuralKeyword,
  isAgentMessage,
  isHostNotification,
  planFrontload,
  type FrontloadPlan,
} from '../directory';
import { extractProseCandidates } from '../search/identifier-segments';
import { looksLikeChangeQuestion } from '../mcp/explore-changes';
import { getTelemetry } from '../telemetry';

/** A prompt that passed the cheap gates, with what they found. */
export interface PromptHookInput {
  prompt: string;
  keyworded: boolean;
  changeShaped: boolean;
  codeTokens: string[];
  proseWords: string[];
  plan: FrontloadPlan;
}

/**
 * Gate telemetry: how often each tier fires vs. no-ops — counter names only,
 * NEVER prompt content (see TELEMETRY.md). This is the data that turns "is the
 * gate any good" from vibes into a measured recall rate.
 */
export function recordPromptHookGate(outcome: string): void {
  try { getTelemetry().recordUsage('cli_command', `prompt-hook-gate-${outcome}`, true); } catch { /* never break the hook */ }
}

/**
 * Kill-switch, or no piped payload (invoked by hand). The kill-switch lets a
 * user disable the nudge without uninstalling or editing settings.json (CI,
 * low-power machines, personal preference).
 */
export function promptHookDisabled(): boolean {
  return process.env.CODEGRAPH_NO_PROMPT_HOOK === '1' || process.env.CODEGRAPH_PROMPT_HOOK === '0' || !!process.stdin.isTTY;
}

/**
 * Parse the `{prompt, cwd}` payload and run the gates that need no index open.
 * Null means the hook has nothing to add.
 *
 * Gate, tiered by confidence (#994, #1126):
 *   HIGH   — a structural keyword (any covered language), or a code-shaped
 *            token verified in the index → full explore injection.
 *   MEDIUM — no keyword/token, but prose words match indexed symbol-name
 *            SEGMENTS ("state machine" → OrderStateMachine, in any
 *            language): inject a short list of the matching symbols and
 *            let the AGENT write the explore query — the graph-derived
 *            tier, no vocabulary involved.
 *   silent — nothing verified. Every other prompt ("fix this typo")
 *            stays a zero-cost no-op.
 * Keywords fire on their own; a token or prose word is only a CANDIDATE
 * verified against the graph by the command, so a tech brand ("JavaScript")
 * that merely looks like code doesn't inject spurious context.
 */
export function gatePromptHook(raw: string): PromptHookInput | null {
  let input: { prompt?: string; cwd?: string } = {};
  try { input = JSON.parse(raw); } catch { return null; }
  const prompt = String(input.prompt || '');

  // Host notifications and subagent hand-backs (#2184) are not user prompts.
  if (isHostNotification(prompt) || isAgentMessage(prompt)) { recordPromptHookGate('noop-notification'); return null; }
  const keyworded = hasStructuralKeyword(prompt);
  // "review my changes", `main..HEAD`: explore answers these from the diff,
  // so a confirmed change question gets the full injection like a keyword.
  const changeShaped = !keyworded && looksLikeChangeQuestion(prompt);
  const codeTokens = keyworded ? [] : extractCodeTokens(prompt);
  const proseWords = keyworded ? [] : extractProseCandidates(prompt);
  if (!keyworded && !changeShaped && codeTokens.length === 0 && proseWords.length === 0) { recordPromptHookGate('noop-shape'); return null; }

  // Decide what to inject, shaped by WHERE the index(es) are: the nearest
  // indexed ancestor of cwd, or — when cwd is an un-indexed workspace root
  // whose indexed project(s) live in sub-dirs (the monorepo case, #964) —
  // the sub-project the prompt points at, plus a `projectPath` nudge for any
  // others. Without the down-scan the hook injected nothing at a monorepo
  // root (it only walked up), so the validated adoption lever never fired
  // exactly where the agent most needs it.
  const plan = planFrontload(String(input.cwd || process.cwd()), prompt);
  // Nothing reachable — the agent's normal tools apply.
  if (!plan.exploreRoot && plan.nudgeProjects.length === 0) { recordPromptHookGate('noop-no-index'); return null; }
  return { prompt, keyworded, changeShaped, codeTokens, proseWords, plan };
}

/**
 * Run the cheap gates before the CLI loads. Null: nothing to add, exit now.
 * Undefined: stdin could not be read synchronously (a non-blocking pipe), so
 * the command reads it asynchronously after the CLI loads.
 */
export function precheckPromptHook(): PromptHookInput | null | undefined {
  try {
    if (promptHookDisabled()) return null;
    let raw: string;
    try { raw = fs.readFileSync(0, 'utf8'); } catch { return undefined; }
    return gatePromptHook(raw);
  } catch {
    // Degradable by contract: never surface an error to the prompt pipeline.
    return null;
  }
}
