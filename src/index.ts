/**
 * CodeGraph — the public library entry.
 *
 * The `CodeGraph` class lives in `./codegraph`; this module re-exports it with
 * the rest of the public surface. Internal read-only paths (query workers, the
 * MCP engine) import `./codegraph` directly so they do not load the extraction
 * and resolution stacks this entry re-exports.
 */

// Re-export types for consumers
export * from './types';
// Storage building blocks for embedded/SDK consumers that drive the graph
// directly (open a DB, run prepared queries) rather than through the CodeGraph
// facade. Exposed from the package entry so they no longer require deep imports
// into dist/ (issue #354).
export { getDatabasePath, DatabaseConnection } from './db';
export { QueryBuilder } from './db/queries';
export {
  getCodeGraphDir,
  isInitialized,
  findNearestCodeGraphRoot,
  CODEGRAPH_DIR,
} from './directory';
export { IndexProgress, IndexResult, SyncResult } from './extraction';
export { detectLanguage, isLanguageSupported, isGrammarLoaded, getSupportedLanguages, initGrammars, loadGrammarsForLanguages, loadAllGrammars } from './extraction';
export { ResolutionResult } from './resolution';
export {
  CodeGraphError,
  FileError,
  ParseError,
  DatabaseError,
  SearchError,
  VectorError,
  ConfigError,
  Logger,
  setLogger,
  getLogger,
  silentLogger,
  defaultLogger,
} from './errors';
export { Mutex, FileLock, processInBatches, debounce, throttle, MemoryMonitor } from './utils';
export { FileWatcher, WatchOptions, PendingFile, LockUnavailableError } from './sync';
export { MCPServer } from './mcp';
import * as extractionStack from './extraction';
import * as resolutionStack from './resolution';
import * as syncStack from './sync';
import { provideStacks } from './codegraph';
provideStacks({ extraction: extractionStack, resolution: resolutionStack, sync: syncStack });

export { CodeGraph } from './codegraph';
export type { InitOptions, OpenOptions, IndexOptions } from './codegraph';
import { CodeGraph } from './codegraph';
export default CodeGraph;
