/**
 * Thrown by a `syncFn` to signal that the underlying sync couldn't acquire
 * the cross-process write lock (#449). The watcher treats this as "no
 * progress" — preserves `pendingFiles`, skips `onSyncComplete`, and the
 * `finally` block reschedules. Quiet (debug-only) because a long-running
 * external indexer can hit this every debounce cycle.
 */
export class LockUnavailableError extends Error {
  constructor(message = 'CodeGraph file lock unavailable; another process is writing') {
    super(message);
    this.name = 'LockUnavailableError';
  }
}
