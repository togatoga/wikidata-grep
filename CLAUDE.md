# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`wikidata-grep` (binary: `wdgrep`) is a fast **grep for Wikibase/Wikidata JSON
dumps**, inspired by
[`wikibase-dump-filter`](https://codeberg.org/maxlath/wikibase-dump-filter). It
reads a dump (NDJSON, optionally with the array-style `[`…`]` wrapper and
trailing commas) on stdin and writes matching entities to stdout, one JSON
object per line.

**The concept: grep that understands the graph.** Two complementary matching
modes:

- **Claim grep** — per-entity filtering/attribute-selection (`--type`,
  `--claim`, `--sitelink`, `--keep`/`--omit`, `--keep-languages`, `--keep-claims`).
- **Graph grep** — match by *reachability* through the property graph the data
  forms (P31/P279/…), which a single `--claim` pass cannot express (e.g. "every
  entity that is transitively a kind of X"). See the `build-graph` +
  `--graph`/`--graph-include`/`--graph-exclude` flow below.

**Scope: grep fast, don't reshape.** The heavier `simplify` transform is
intentionally **not** implemented — users pipe to `wikibase-dump-filter
--simplify` / `jq` for that. (It was removed; see git history if you need the
old `simplifyEntity` port.)

In addition to the filter mode, wdgrep provides a **`build-graph`
subcommand** that preprocesses a dump into a compact property-graph file
(`{"id":"Q5","P31":["Q215627"],"P279":["Q1884349"]}`). The filter mode then
consumes that file via **`--graph` + `--graph-include`/`--graph-exclude`**:
the graph is loaded once into a reverse adjacency, BFS materialises the id-set
that can *reach* each target QID, and filtering an entity is an O(1) id-set
membership test (done before the JSON parse). This replaces the originally
planned standalone `resolve-closure` step — reachability filtering is built
directly into the filter path.

## Commands

```sh
cargo build --release          # binary at target/release/wdgrep
cargo test                     # debug tests (catches integer-overflow panics that release wrapping hides)
cargo test --release           # release tests
cargo test --test cli          # only the end-to-end binary tests (tests/cli.rs)
cargo test matches_javascript  # run a single test by name substring
cargo fmt --all -- --check     # CI formatting gate
cargo clippy --all-targets -- -D warnings   # CI lint gate (must be clean)
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy `-D warnings`, build, and test.
Always run `cargo test` (debug) before committing — release builds set
`panic = "abort"` and wrap integer overflow, so debug catches arithmetic bugs
(e.g. epoch math) that release silently hides.

The `benchmarks/` setup compares wdgrep vs wikibase-dump-filter (single and
GNU-parallel) vs jq on a gzip sample, all decompressed by `pigz -dc` (needs
`hyperfine` + `pigz`, and `npm install` in `benchmarks/` for a local
wikibase-dump-filter — no npx). `download-dataset.sh` builds a sample under
`benchmarks/data/`, then `bash benchmarks/quick-bench.sh` runs the comparison and
reproduces the README table (fixed `--claim P31:Q5`).

## Architecture

Pipeline per line: **pre-filter → parse → filter → format → serialize**. The
porting unit is `wikibase-dump-filter`'s `lib/*.js` and the subset of
`wikibase-sdk` it uses; module names mirror those responsibilities.

- `main.rs` — CLI wiring; dispatches to the `build-graph` subcommand or the filter path. The buffering/worker-count decision and the sequential loop live in `runner` (shared with `build-graph`).
- `cli.rs` — clap options. `Cli` is the top-level struct with an optional `Commands` subcommand enum. `BuildGraphArgs` holds `build-graph` options; the remaining fields are the filter-mode options mirroring the reference flags. Both share the `-j/--threads` and `--line-buffered` flags.
- `build_graph.rs` — `wdgrep build-graph` implementation. Reads a dump on stdin, applies a `memchr` pre-filter (skip parse if no requested property string is present), then uses **sonic-rs** (SIMD JSON parser) to extract entity id and entity-valued mainsnaks for each requested property. Outputs `{"id":"Qxxx","P279":[...],"P31":[...]}` NDJSON. When `--properties` is omitted every property carrying an entity-valued mainsnak is emitted — the pre-filter falls back to the `wikibase-entityid` type marker (a sound necessary condition) and `parse_and_extract` iterates all `claims` keys in dump order. Both parsing and serialisation use sonic-rs. The per-line `process_graph_line` runs on both the sequential and parallel paths (same dispatch/buffering policy as the filter path: `-j/--threads`, `--line-buffered`); it feeds `parallel::run` via a closure.
- `process.rs` — `process_line` (filter path) and the `LineOutcome` type returned
  by every per-line routine (filter and `build-graph`). **Put per-line logic in a
  shared routine, not duplicated across the sequential/parallel paths.**
- `runner.rs` — shared read→process→write driver for the sequential path, used by
  both the filter path and `build-graph`, so the two can't drift. Owns the
  buffering/worker-count policy (`dispatch`), the input opener (`open_input`, also
  reused by `parallel::reader_loop`), and the sequential loop (read → per-line
  closure → progress accounting → line-buffered flush).
- `parallel.rs` — order-preserving thread pool (default path), generic over a
  caller-supplied per-line closure `(line, &mut out) -> LineOutcome`, reused by
  both the filter path and `build-graph`. One reader thread slices stdin into
  ~4 MB blocks of whole lines (tagged with a sequence number), N workers run the
  closure per block independently, one writer thread reorders finished blocks by
  sequence before writing. Workers only write to their own block buffer, so the
  single writer is the only thing touching stdout — output order (hence bytes) is
  identical to sequential regardless of thread count.
- `filter/mod.rs` — type/claim/sitelink filters (port of `filter_entity.js`)
  plus the `--has-sitelinks` filter (require ≥1 sitelink); the claim
  *evaluator* (`valid_claim`/`matches_deep`/`matches_qualifier`); a minimal
  entity-id extractor (`snak_entity_id`) — the small slice of `simplifyEntity`
  claim value matching still needs, since `:Qxxx` constraints are always item
  ids; and the **`Prefilter`**: a sound, SIMD (`memchr::memmem`)
  necessary-condition check on raw bytes (e.g. requires `"P31"` and `"Q5"`
  substrings) that lets non-matching lines skip the expensive JSON parse.
  Prefilter clauses are only emitted where provably sound (groups with a negated
  term contribute nothing).
- `graph.rs` — graph-reachability filter (`--graph` + `--graph-include`/
  `--graph-exclude`). Loads the `build-graph` file into a reverse adjacency
  `target -> [sources]` via a parallel pipeline (`load_reverse_adjacency`): one
  reader slices the file into ~4 MB blocks of whole lines, a pool of workers
  JSON-parse blocks with sonic-rs into `(target, source)` edge lists in parallel
  (`parse_block` → `parse_graph_line`), and the main thread folds those into the
  single map — keeping map construction single-threaded avoids duplicating popular
  keys across per-worker maps. In a parsed line the `"id"` is the source and each
  property's array values are edge targets. Ids are parsed into a `NodeId` enum — `Q(u32)`/`P(u32)`/
  `L(u32)` for the numeric kinds, `Other(Box<str>)` for anything else (other
  entity kinds, lexeme forms like `L1-F2`, or a number too big for `u32`) — so
  ids are never silently dropped or conflated (`Q100` ≠ `P100` ≠ `M100`) and
  there's no fragile "always Q/P/L numbers that fit 30 bits" assumption. The
  graph is followed **as defined**: edges are traversed whatever the endpoint
  kinds, so a property graph (`P… --P1647--> P…`) is reachable just like the item
  hierarchy, and the result is scoped by the target you ask for (`--graph-include
  Q5` only reaches Q nodes; `--graph-include P17` walks the subproperty tree).
  Invalid JSON is a hard error (the file is `build-graph`'s own output, so a parse
  failure means corruption). It then BFSes from the targets to materialise the
  reachable node-sets, and `GraphReach::allows(id)` is an O(1) membership test
  (the reverse map and the reachable sets are `ahash`-backed for speed).
  `--graph-include`/`-exclude` targets are OR'd (reach any one); exclude takes
  precedence over include. `--graph-properties` restricts which properties are
  followed as edges (`parse_graph_line`'s `allowed` set of property-key strings,
  matched verbatim against the file's keys). Held on `Filter::graph` and checked
  on the raw line's id (via `entity_id`, which finds the `"id":"` key and parses
  it) before the JSON parse.
  `main.rs` errors if
  `--graph-include`/`-exclude`/`--graph-properties` are given without `--graph`,
  and (unless `--quiet`) prints `GraphReach::summary()` to stderr at startup.
  During the load `load_reverse_adjacency` shows an `indicatif` byte-progress bar
  (bytes read / file size, speed, ETA) so a multi-GB graph isn't a silent wait;
  it is suppressed by `--quiet` and auto-hidden when stderr isn't a terminal.
- `filter/claim.rs` — the claim-expression grammar: `&` AND, `|` OR, `~` NOT,
  `,` value-OR, plus three extra notations —
  `@P` (**deep**: property as a mainsnak *or* a qualifier at any depth; combine
  with `~` as `~@P`), `Pa.Pb` (**scoped**: a statement of `Pa` carrying a `Pb`
  qualifier on the *same* statement), and **parentheses** for arbitrary boolean
  nesting (precedence `~` > `|` > `&`; whitespace ignored). It is a tokenizer +
  recursive-descent parser → `ClaimAst` → `to_cnf`, which normalises any
  expression (incl. parens/De Morgan) to the conjunctive normal form
  (`groups: Vec<Vec<ClaimTerm>>`) the evaluator and `Prefilter` consume — so a
  paren-free expression yields the same groups as the old split-based parser.
  CNF distribution is capped (`MAX_CNF_GROUPS`) to bound pathological blowup.
- `format.rs` — keep/omit (attribute selection, output order driven by `--keep`
  order or canonical order for `--omit`), language filtering, and the
  `--keep-claims` (narrow `claims` to selected properties). These only
  *select and project*; claim/snak structures pass through verbatim (no value
  reshaping). `Formatter::is_noop()` returns true when no formatting options are
  set; `process.rs` uses this to skip re-serialisation entirely and write the
  cleaned raw input bytes directly.
- `parse.rs` — `clean_line` (trim + strip trailing comma, return `&[u8]` if it
  looks like a JSON object) and `parse_line` (calls `clean_line` then
  `sonic_rs::Deserializer::from_slice(...).use_rawnumber()` so numbers are stored
  as raw strings and round-trip without reformatting).
- `progress.rs` — `indicatif`-based stderr progress bar (cosmetic; draws only
  when stderr is a terminal, so it survives tmux/resize). `parsed` counts all
  entities processed (including pre-filtered-out lines, via `is_entity_line`),
  `kept` counts those that passed the filter.

### JSON parsing and serialisation

All *parsing* uses **sonic-rs** (filter path, build-graph, graph load) — it is
the SIMD-fast, read-only hot path, where object key order doesn't matter.
`parse_line` calls `Deserializer::use_rawnumber()` so numeric values are stored
as raw byte strings and serialised back verbatim — no `f64` conversion, no
reformatting.

*Output serialisation* uses **serde_json** with the `preserve_order` and
`arbitrary_precision` features. sonic-rs ≥ 0.4.0 hash-orders
programmatically-built objects (insertion order is **not** preserved, and the
order is randomised per process run), which would scramble output key order and
make the bytes nondeterministic. serde_json's `Map` is `IndexMap`-backed, so
`format.rs` (`--keep`/`--omit`/`--keep-languages`/`--keep-claims`) and
`build_graph.rs` build and emit objects with keys in input / requested order;
`arbitrary_precision` round-trips numbers verbatim, matching sonic's
`use_rawnumber`. This re-parse-with-serde_json step is slower than sonic, but
runs only on kept + formatted lines (the filter hot path stays on sonic-rs).

When no formatting options are given (`--keep`/`--omit`/`--keep-languages`/
`--keep-claims` all absent), `process.rs` writes the cleaned raw input bytes
directly, skipping parse→Value→serialise entirely — so order is trivially
preserved and serde_json isn't involved at all.

### Differences from wikibase-dump-filter

- **`--type` keeps all entity types by default.** The reference defaults to
  `item` (silently dropping properties); wdgrep keeps every type unless
  `--type item` / `--type property` is given.
- **No `--simplify` / no claim-value reshaping.** Pipe to `jq` or
  `wikibase-dump-filter --simplify` when you need flattened values.
  `--claim P:Qxxx` compares only entity-valued claims, so a non-entity claim
  whose string value happens to be `"Qxxx"` won't match.
- **`--keep-languages` on entities without `sitelinks`** — the reference throws;
  wdgrep skips sitelink language filtering for those entities instead of crashing.
- **No language-code validation on `--keep-languages`.** The reference validates
  each code against a regex (`/^[a-z]{2,3}(-[a-z]{2,6})?$/`); wdgrep accepts any
  string. Wikidata language codes have no formal regex spec — the source of truth
  is a curated list (<https://www.wikidata.org/wiki/Wikidata:Lists/languages>),
  and that regex rejects real codes like `es-419`, `simple`, and
  `nan-latn-tailo`. An unknown code simply matches no labels/sitelinks (grep
  semantics) rather than erroring.
- **Number formatting** — wdgrep passes numbers through as raw bytes from the
  input; it does not reformat floats to match JavaScript's `Number.prototype.toString`.
