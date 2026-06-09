#!/usr/bin/env bash
#
# Shared configuration for the wdgrep benchmark.
#
# Sourced by download-dataset.sh and quick-bench.sh. Override any value from the
# environment, e.g.:
#
#   ENTITY_COUNT=200000 bash download-dataset.sh
#   RUNS=5 bash quick-bench.sh
#

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# --- Dataset ---------------------------------------------------------------

# Upstream full Wikidata dump (gzip-compressed JSON array, ~150 GB compressed).
# We only stream the first ENTITY_COUNT entities off the front of it.
export DUMP_URL="${DUMP_URL:-https://dumps.wikimedia.org/wikidatawiki/entities/latest-all.json.gz}"

# How many entities to slice out of the dump for the benchmark sample.
# 300k keeps the download to a few minutes; bump it for a heavier run.
export ENTITY_COUNT="${ENTITY_COUNT:-300000}"

# Where the prepared sample lives. Everything here is git-ignored.
export DATA_DIR="${DATA_DIR:-$HERE/data}"

# Uncompressed NDJSON sample (backs the optional correctness check) and the gzip
# variant that quick-bench.sh actually times.
export DATASET="${DATASET:-$DATA_DIR/sample-${ENTITY_COUNT}.json}"
export DATASET_GZ="${DATASET_GZ:-$DATASET.gz}"

# --- Tools under test ------------------------------------------------------

# The wdgrep binary. Build it with `cargo build --release` first.
export WDGREP="${WDGREP:-$HERE/../target/release/wdgrep}"

# wikibase-dump-filter, from a global install (`npm install -g
# wikibase-dump-filter`), resolved on PATH. NOT npx — npx re-resolves the package
# on every GNU parallel block and dominates the timing.
export WDF="${WDF:-wikibase-dump-filter}"

# jq binary.
export JQ="${JQ:-jq}"

# --- Workload --------------------------------------------------------------

# The benchmarked claim is fixed to P31:Q5 (humans) inside quick-bench.sh: a fair
# comparison needs a claim all three tools express identically, and wdgrep
# supports syntax the others do not.

# GNU parallel block size for the parallel wikibase-dump-filter row.
export BLOCK="${BLOCK:-100M}"

# --- hyperfine -------------------------------------------------------------

# Warmup runs (page cache warming) before timed runs.
export WARMUP_COUNT="${WARMUP_COUNT:-1}"

# Number of timed runs per command.
export RUNS="${RUNS:-3}"

# Whether to cross-check that every tool keeps the same matches (and that wdgrep
# == wikibase-dump-filter byte-for-byte) before timing. Off by default — it runs
# each tool an extra full pass; set CHECK_DIFF=true to run it.
export CHECK_DIFF="${CHECK_DIFF:-false}"
