# wdgrep benchmark

One script, [`quick-bench.sh`](quick-bench.sh), that reproduces the comparison in
the top-level README: **wdgrep** vs **jq** vs
[**wikibase-dump-filter**](https://codeberg.org/maxlath/wikibase-dump-filter)
(single process and GNU-parallel), on a real Wikidata dump sample, timed with
[`hyperfine`](https://github.com/sharkdp/hyperfine).

Every tool reads the **same gzip sample** decompressed by `pigz -dc`, so the
decompressor is never the variable — only the filtering tool is. The filter is
fixed to `--claim P31:Q5` (humans): a fair comparison needs a claim all three
tools express identically, and wdgrep also supports syntax the others cannot
(parentheses, `@`deep, scoped qualifiers, `~`not).

## Setup

1. Install [hyperfine](https://github.com/sharkdp/hyperfine) and `pigz`.
   Optional: `jq`, Node + npm, and GNU `parallel` (for the parallel row).
2. Build the release binary:
   ```sh
   cargo build --release        # from the repo root
   ```
3. Install `wikibase-dump-filter` globally (**not** npx — npx re-resolves the
   package on every GNU parallel block and dominates the timing):
   ```sh
   npm install -g wikibase-dump-filter
   ```
4. Build the dataset (300,000 entities, raw `.json` + `.gz`):
   ```sh
   cd benchmarks
   bash download-dataset.sh
   ```
   This streams the first 300,000 entities off the front of the live
   [`latest-all.json.gz`](https://dumps.wikimedia.org/wikidatawiki/entities/latest-all.json.gz)
   dump (a few GB of transfer — **not** the full ~150 GB; `head` + SIGPIPE stop
   the download early), strips the JSON-array wrapper into clean NDJSON, and
   writes both forms under `data/` (git-ignored). The `.gz` is what gets timed;
   the raw `.json` backs the optional correctness check.

## Running

```sh
bash quick-bench.sh
```

It times these four commands, each reading the same `.gz` through `pigz -dc`:

```sh
pigz -dc sample.json.gz | wdgrep --claim P31:Q5 --quiet
pigz -dc sample.json.gz | wikibase-dump-filter --claim P31:Q5 --quiet
pigz -dc sample.json.gz | jq -c 'select(any(.claims.P31[]?; .mainsnak.datavalue.value.id == "Q5"))'
pigz -dc sample.json.gz | parallel --pipe --block 100M --line-buffer "wikibase-dump-filter --claim P31:Q5 --quiet"
```

The parallel row is the upstream
[wikibase-dump-filter "Parallelize" recipe](https://github.com/maxlath/wikibase-dump-filter/blob/main/docs/parallelize.md)
(`parallel --pipe --block 100M --line-buffer`). GNU `parallel` does not preserve
line order, so that row is a throughput measurement, not a byte-exact one. The
table is written to `results-benchmark.md` and printed to the terminal.

By default it goes straight to timing. Set `CHECK_DIFF=true` to first verify
every tool keeps the same matches (and that wdgrep and wikibase-dump-filter are
byte-identical); it is off by default because it runs each tool an extra full
pass.

You can also point it at a `.gz` dump you already have:

```sh
bash quick-bench.sh path/to/dump.json.gz
```

### Example result

300,000 entities (865 MB gzip / 5.9 GB JSON), 16-core machine, `pigz -dc` feeding
every tool:

| Command | Mean [s] | Relative |
| --- | ---: | ---: |
| `wdgrep --claim P31:Q5` | **8.5** | **1.0×** |
| `parallel … "wikibase-dump-filter --claim P31:Q5"` | 22.7 | 2.7× |
| `wikibase-dump-filter --claim P31:Q5` | 42.9 | 5.0× |
| `jq -c 'select(…)'` | 70.9 | 8.3× |

## Configuration

Override anything from the environment (see `config.sh` for the full list):

```sh
ENTITY_COUNT=200000 bash download-dataset.sh   # smaller sample
RUNS=5 bash quick-bench.sh                      # 5 timed runs
CHECK_DIFF=true bash quick-bench.sh             # also verify every tool agrees
FORCE=1 bash download-dataset.sh               # rebuild the dataset
```

| Variable | Default | Meaning |
| --- | --- | --- |
| `ENTITY_COUNT` | `300000` | entities sliced from the dump |
| `RUNS` | `3` | timed hyperfine runs per command |
| `WARMUP_COUNT` | `1` | warmup runs (page-cache warming) |
| `BLOCK` | `100M` | GNU parallel block size (parallel row) |
| `WDGREP` | `../target/release/wdgrep` | wdgrep binary |
| `WDF` | `wikibase-dump-filter` | the Node reference on PATH (`npm install -g`, no npx) |
| `CHECK_DIFF` | `false` | opt-in: print kept-line counts per tool before timing |

## Files

| File | Purpose |
| --- | --- |
| `config.sh` | shared, env-overridable configuration |
| `download-dataset.sh` | build the 300k-entity sample (raw `.json` + `.gz`) |
| `quick-bench.sh` | the benchmark: wdgrep vs wikibase-dump-filter vs jq vs wikibase-dump-filter (parallel) |
| `data/` | generated dataset (git-ignored) |

## Notes

- The sample is plain NDJSON (one entity per line, array wrapper stripped), so
  **jq reads it directly** — no `sed` preprocessing, unlike on the raw
  array-style dump.
- `NODE_OPTIONS=--max_old_space_size=4096` is set for the Node tool as the
  upstream guide suggests, but it is a no-op on machines with enough RAM (recent
  Node already auto-sizes the old-space limit to ~4 GB) — it just guards against
  OOM on memory-constrained boxes.
- This shell tooling assumes **bash** (the scripts rely on bash arrays and word
  behavior). Run them with `bash`, not zsh.
