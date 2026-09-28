/**
 * Node runtime flags for CodeGraph's own re-spawns and launchers.
 *
 * Until Phase 5 of kernel-only-extraction-plan.md this module (then named
 * wasm-runtime-flags) also carried the `--liftoff-only` relaunch that kept
 * tree-sitter's wasm grammars off V8's turboshaft tier (issues #293/#298).
 * The wasm path is gone, so the relaunch and the Node 25 block that existed
 * for it are gone too; what remains is the host-PPID handoff env.
 */

/**
 * The env var a launching process uses to tell a child which pid is the MCP
 * host, so the child's PPID watchdog follows the host rather than the launcher.
 */
export const HOST_PPID_ENV = 'CODEGRAPH_HOST_PPID';
