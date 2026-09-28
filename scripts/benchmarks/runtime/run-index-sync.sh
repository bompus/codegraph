#!/bin/bash
# usage: run-index-sync.sh <list-file>
# Each list line: <corpus-path> <rel-file-to-edit> <comment-prefix: // or #>.
# Waits for an idle host before each corpus.
HERE=$(cd "$(dirname "$0")" && pwd); . "$HERE/arms.sh"
while read -r corpus file cm; do
  [ -z "$corpus" ] || [ "${corpus:0:1}" = "#" ] && continue
  idle; echo "== $corpus $(date +%T) load=$(cut -d' ' -f1 /proc/loadavg)"
  bash "$HERE/index-sync.sh" "$corpus" "$file" "$cm"
done < "$1"
echo "== done $(date +%T)"
