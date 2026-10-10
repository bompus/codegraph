# Session reader contract

Synthetic transcripts and `expected.json`: the prose entries that every session reader must index
identically, whatever else it indexes. CodeGraph's readers (`src/sessions/`) are checked against it by
`__tests__/sessions-reader-contract.test.ts`. A private downstream session indexer keeps a copy of these
files with a recorded SHA-256 of each and checks that its own readers return every expected entry (it may return more).

The shared subset is: a user or assistant turn with a timestamp whose text blocks (joined with a newline) hold at
least 20 characters; one Claude entry per JSONL line; a truncated trailing line is skipped; tool calls and results,
thinking and developer messages are not prose. Codex sessions are the main thread (`id` equals `session_id`).

Not part of the contract, because the readers differ on purpose: entries shorter than 20 characters, meta entries,
compaction summaries, Codex injected instruction blobs, subagent transcripts, tool outputs and titles.

Changing a file here changes the contract: update the copy and its recorded hash in the other indexer in the same
change, or say why it does not apply.
