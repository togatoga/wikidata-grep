#!/usr/bin/env bash
#
# The wdgrep benchmark. Run it and you get the same comparison the README shows.
#
# It extracts every human (--claim P31:Q5) from the gzip sample, decompressed by
# `pigz -dc` for every tool so the decompressor is never the variable, and times
# four ways of doing it:
#
#   wdgrep                           parallel out of the box (a single process)
#   wikibase-dump-filter             the Node reference, single process
#   jq                               select() on the mainsnak value id
#   wikibase-dump-filter (parallel)  fanned out with GNU parallel, the upstream
#     "Parallelize" recipe: https://github.com/maxlath/wikibase-dump-filter/blob/main/docs/parallelize.md
#
# The claim is fixed to P31:Q5 on purpose: a fair comparison needs a filter all
# three tools express identically, and wdgrep also understands syntax the others
# cannot (parentheses, @deep, scoped qualifiers, ~not).
#
# wikibase-dump-filter is a global install on PATH, never npx — npx re-resolves
# the package on every GNU parallel block and dominates the timing.
#
# Setup (once):
#   cargo build --release                          # from the repo root
#   npm install -g wikibase-dump-filter            # the Node reference (no npx)
#   cd benchmarks && bash download-dataset.sh      # build data/sample-300000.json[.gz]
#
# Usage:
#   bash quick-bench.sh                   # times the prepared .gz sample
#   bash quick-bench.sh path/to/dump.json[.gz]   # gz or plain, auto-detected
#   RUNS=5 bash quick-bench.sh
#   CHECK_DIFF=true bash quick-bench.sh   # also verify every tool agrees first

source "$(dirname "${BASH_SOURCE[0]}")/config.sh"
set -euo pipefail

have() { command -v "$1" >/dev/null 2>&1; }

SRC="${1:-$DATASET_GZ}"
CLAIM="P31:Q5"
JQ_FILTER='select(any(.claims.P31[]?; .mainsnak.datavalue.value.id == "Q5"))'

command -v hyperfine >/dev/null || { echo "hyperfine is required: https://github.com/sharkdp/hyperfine" >&2; exit 1; }
[[ -f "$SRC" ]]     || { echo "sample not found: $SRC  (run: bash download-dataset.sh)" >&2; exit 1; }
[[ -x "$WDGREP" ]]  || { echo "wdgrep not found at $WDGREP  (run: cargo build --release)" >&2; exit 1; }
have "$WDF"        || echo "note: '$WDF' not found — skipping wikibase-dump-filter rows (run: npm install -g wikibase-dump-filter)" >&2

# Pick the reader by sniffing the gzip magic bytes (1f 8b): pigz -dc for a gzip
# file, plain cat for an already-decompressed NDJSON. So you can pass either
# sample-300000.json.gz (what the README times) or sample-300000.json.
if [[ "$(od -An -tx1 -N2 "$SRC" 2>/dev/null | tr -d ' ')" == "1f8b" ]]; then
    have pigz || { echo "pigz is required to read a .gz sample (apt install pigz)" >&2; exit 1; }
    READER="pigz -dc"
else
    READER="cat"
fi

# Give the Node workers more heap, as the upstream guide suggests (a no-op on
# machines whose default V8 old-space is already ~4 GB).
export NODE_OPTIONS="${NODE_OPTIONS:---max_old_space_size=4096}"

echo "input  : $SRC ($(du -h "$SRC" | cut -f1)), read with: $READER"
echo "claim  : $CLAIM"
echo "runs   : $RUNS (warmup $WARMUP_COUNT)"
echo

if [[ "${CHECK_DIFF}" == "true" ]]; then
  echo "### correctness (kept line counts) ###"
  printf '%-26s %s\n' "wdgrep" "$($READER "$SRC" | "$WDGREP" --claim "$CLAIM" --quiet | wc -l)"
  if have "$WDF"; then
    printf '%-26s %s\n' "wikibase-dump-filter" "$($READER "$SRC" | "$WDF" --claim "$CLAIM" --quiet 2>/dev/null | wc -l)"
    if cmp -s <($READER "$SRC" | "$WDGREP" --claim "$CLAIM" --quiet) \
              <($READER "$SRC" | "$WDF" --claim "$CLAIM" --quiet 2>/dev/null); then
      echo "  -> wdgrep and wikibase-dump-filter output is byte-identical"
    else
      echo "  -> WARNING: wdgrep and wikibase-dump-filter output DIFFERS" >&2
    fi
  fi
  have "$JQ" && printf '%-26s %s\n' "jq" "$($READER "$SRC" | "$JQ" -c "$JQ_FILTER" | wc -l)"
  echo
fi

# Build the benchmark commands (label + the exact shell command timed).
names=();  shells=()
names+=("wdgrep")
shells+=("$READER $SRC | $WDGREP --claim $CLAIM --quiet > /dev/null")
if have "$WDF"; then
  names+=("wikibase-dump-filter")
  shells+=("$READER $SRC | $WDF --claim $CLAIM --quiet 2>/dev/null > /dev/null")
fi
if have "$JQ"; then
  names+=("jq")
  shells+=("$READER $SRC | $JQ -c '$JQ_FILTER' > /dev/null")
fi
if have "$WDF" && have parallel; then
  names+=("wikibase-dump-filter (parallel)")
  shells+=("$READER $SRC | parallel --pipe --block $BLOCK --line-buffer '$WDF --claim $CLAIM --quiet' 2>/dev/null > /dev/null")
fi

echo "### commands timed ###"
for i in "${!names[@]}"; do printf '  %s\n      %s\n' "${names[$i]}" "${shells[$i]}"; done
echo

cmds=()
for i in "${!names[@]}"; do cmds+=( -n "${names[$i]}" "${shells[$i]}" ); done

hyperfine --warmup "$WARMUP_COUNT" --runs "$RUNS" \
    --export-markdown results-benchmark.md \
    "${cmds[@]}"
