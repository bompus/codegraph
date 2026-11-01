#!/bin/bash
# Append a unique one-file change for one Hyperfine sync run. The timestamp is
# the new content, so every repetition of `codegraph sync` sees a real edit;
# the caller restores the file after the arm.
set -u
file=$1; cm=$2
n=$(date +%s%N)
printf '\n%s cg bench probe %s\nvoid_probe_marker_%s = 1\n' "$cm" "$n" "$n" >> "$file"
