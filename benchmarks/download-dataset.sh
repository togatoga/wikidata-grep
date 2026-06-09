#!/usr/bin/env bash
#
# Build the benchmark dataset: stream the first ENTITY_COUNT (default 300,000)
# entities off the front of the live Wikidata dump and materialise it in two
# forms:
#
#   data/sample-<N>.json       raw NDJSON  (backs the byte-exact correctness check)
#   data/sample-<N>.json.gz    gzip        (what quick-bench.sh times)
#
# We never download the whole ~150 GB dump: curl streams the gzip, we decompress
# on the fly, take the first N entity lines, and SIGPIPE stops the transfer as
# soon as `head` has enough. Expect to pull only a few GB of compressed data.
#
# The upstream dump is a JSON *array*: line 1 is "[", then one entity object per
# line each ending in ",", and a final "]". We drop the leading "[" and strip
# the trailing comma from every line so the sample is clean NDJSON that wdgrep,
# wikibase-dump-filter, AND jq can all read directly (jq needs no `sed` wrapper).
#
# Usage:
#   bash download-dataset.sh
#   ENTITY_COUNT=200000 bash download-dataset.sh     # smaller sample
#   FORCE=1 bash download-dataset.sh                 # rebuild even if present

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/config.sh"

have() { command -v "$1" >/dev/null 2>&1; }
have curl  || { echo "curl is required" >&2; exit 1; }
have gzip  || { echo "gzip is required" >&2; exit 1; }

mkdir -p "$DATA_DIR"

# --- raw NDJSON ------------------------------------------------------------

if [[ -f "$DATASET" && "${FORCE:-0}" != "1" ]]; then
    echo "raw sample already exists: $DATASET"
    echo "  $(wc -l < "$DATASET") entities, $(du -h "$DATASET" | cut -f1)  (set FORCE=1 to re-download)"
else
    echo "streaming first $ENTITY_COUNT entities from:"
    echo "  $DUMP_URL"
    echo "into: $DATASET"
    echo "(this transfers a few GB; SIGPIPE stops it once enough lines are read)"
    echo

    tmp="$DATASET.partial"
    # Pipeline:
    #   tail -n +2   : drop the leading "[" array-open line
    #   head -n N    : take exactly N entity lines (then CLOSES the pipe)
    #   sed 's/,$//' : strip the trailing "," that separates array elements
    #   awk          : count entities and print a live counter to stderr
    #
    # `head` closing the pipe early makes curl/gzip/tail exit with SIGPIPE (141).
    # That is expected, so we drop pipefail for this pipeline and validate the
    # result by line count instead (a real failure shows up as 0 lines below).
    set +o pipefail
    curl -fsSL "$DUMP_URL" \
        | gzip -dc \
        | tail -n +2 \
        | head -n "$ENTITY_COUNT" \
        | sed 's/,[[:space:]]*$//' \
        | awk -v total="$ENTITY_COUNT" '
            { print }
            NR % 50000 == 0 {
                printf "\r  fetched %d / %d entities...", NR, total > "/dev/stderr"
                fflush()
            }
            END {
                printf "\r  fetched %d entities (target %d)            \n", NR, total > "/dev/stderr"
            }
          ' \
        > "$tmp"
    set -o pipefail

    got="$(wc -l < "$tmp")"
    if [[ "$got" -eq 0 ]]; then
        echo "ERROR: downloaded 0 entities — check network / URL:" >&2
        echo "  $DUMP_URL" >&2
        rm -f "$tmp"
        exit 1
    fi
    mv "$tmp" "$DATASET"

    echo "==> wrote $got entities -> $DATASET ($(du -h "$DATASET" | cut -f1))"
    if [[ "$got" -lt "$ENTITY_COUNT" ]]; then
        echo "WARNING: only $got entities available (wanted $ENTITY_COUNT)" >&2
    fi
fi

# --- gzip ------------------------------------------------------------------

if [[ -f "$DATASET_GZ" && "${FORCE:-0}" != "1" ]]; then
    echo "gz sample already exists: $DATASET_GZ ($(du -h "$DATASET_GZ" | cut -f1))"
else
    echo "compressing -> $DATASET_GZ"
    gzip -c "$DATASET" > "$DATASET_GZ"
    echo "wrote $DATASET_GZ ($(du -h "$DATASET_GZ" | cut -f1))"
fi

echo
N="$(wc -l < "$DATASET")"
echo "dataset ready: $N entities in 2 forms"
printf '  %-44s %s\n' "$DATASET"     "$(du -h "$DATASET"     | cut -f1)"
printf '  %-44s %s\n' "$DATASET_GZ"  "$(du -h "$DATASET_GZ"  | cut -f1)"
