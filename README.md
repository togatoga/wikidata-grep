# wikidata-grep (`wdgrep`)

[![CI](https://github.com/togatoga/wikidata-grep/actions/workflows/ci.yml/badge.svg)](https://github.com/togatoga/wikidata-grep/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/wikidata-grep.svg)](https://crates.io/crates/wikidata-grep)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![MSRV](https://img.shields.io/badge/rustc-1.88+-blue.svg)](https://www.rust-lang.org/)

**A fast grep for Wikidata dumps: match entities by their claims, and by their place in the type graph.**

`wdgrep` is a fast grep for [Wikidata](https://www.wikidata.org/wiki/Wikidata:Main_Page)
JSON dumps, inspired by
[`wikibase-dump-filter`](https://codeberg.org/maxlath/wikibase-dump-filter). It
reads a dump on stdin and writes the matching entities as
[newline-delimited JSON (NDJSON)](https://en.wikipedia.org/wiki/JSON_streaming#NDJSON),
one object per line. Beyond matching flat claims, it also follows the **graph**
that properties like [P31](https://www.wikidata.org/wiki/Property:P31)
(*instance of*) and [P279](https://www.wikidata.org/wiki/Property:P279)
(*subclass of*) form, so you can match by **reachability**: for example, every
entity that is a kind of *[chain](https://www.wikidata.org/wiki/Q65553774)*
(Q65553774) at any depth, which a single `--claim` pass cannot express.

Two kinds of matching:

- **Per-entity filters** (`--claim`, `--sitelink`, `--type`): keep an entity by
  looking only at its own fields, fast.
- **Graph reachability** (`--graph`): `build-graph` distils the dump into a
  compact property-graph once, then `--graph-include`/`--graph-exclude` keep an
  entity by whether its id can *reach* a target QID through that graph. See
  [Graph grep](#graph-grep).

```sh
# keep only humans (entities with a P31:Q5 claim)
cat latest-all.json | wdgrep --claim P31:Q5 > humans.ndjson

# directly from a compressed dump
gzip -dc latest-all.json.gz | wdgrep --claim P31:Q5 > humans.ndjson

# every chain (Q65553774), including all subclasses and their instances
wdgrep build-graph --properties P31,P279 < latest-all.json > graph.ndjson
wdgrep --graph graph.ndjson --graph-include Q65553774 < latest-all.json > chains.ndjson
```

## Contents

- [Install](#install)
- [Get a Wikidata dump](#get-a-wikidata-dump)
- [Usage](#usage)
  - [Filters](#filters)
  - [Claim expressions](#claim-expressions)
    - [Qualifier matching](#qualifier-matching)
  - [Sitelink expressions](#sitelink-expressions)
  - [Graph grep](#graph-grep)
    - [Step 1: build the graph](#step-1-build-the-graph)
    - [Step 2: filter the full dump by graph reachability](#step-2-filter-the-full-dump-by-graph-reachability)
    - [`build-graph` options](#build-graph-options)
  - [Formatters](#formatters)
  - [Other options](#other-options)
- [Benchmark](#benchmark)
- [License](#license)

## Install

From [crates.io](https://crates.io/crates/wikidata-grep) (the crate is
`wikidata-grep`; the installed command is `wdgrep`):

```sh
cargo install wikidata-grep
```

Or build from source:

```sh
cargo install --path .   # installs the `wdgrep` binary
# or
cargo build --release    # binary at target/release/wdgrep
```

Needs a [Rust toolchain](https://rustup.rs).

## Get a Wikidata dump

wdgrep reads a Wikidata **JSON** dump on stdin. Wikidata publishes a full entity
dump as a single
[newline-delimited JSON](https://en.wikipedia.org/wiki/JSON_streaming#NDJSON)
file (with an array-style `[`…`]` wrapper and trailing commas, which wdgrep
handles transparently):

- **JSON dumps directory:** <https://dumps.wikimedia.org/wikidatawiki/entities/>
- **Latest full JSON dump:**
  - gzip: <https://dumps.wikimedia.org/wikidatawiki/entities/latest-all.json.gz> (~154 GB)
  - bzip2: <https://dumps.wikimedia.org/wikidatawiki/entities/latest-all.json.bz2> (~102 GB)

The dump is huge (sizes above are from 2026-06; it decompresses to roughly
1.5 TB), so download the whole file first. Use `wget --continue` so an interrupted transfer can resume
instead of starting over:

```sh
wget --continue https://dumps.wikimedia.org/wikidatawiki/entities/latest-all.json.gz
```

For a file this big, [`aria2`](https://aria2.github.io/) is the better choice:
it opens a few parallel connections and resumes an interrupted transfer just
like `wget --continue`. The [official server](https://dumps.wikimedia.org/) caps connections at 3 per IP, so
keep `-x` and `-s` at 3 or below:

```sh
aria2c -x3 -s3 --continue https://dumps.wikimedia.org/wikidatawiki/entities/latest-all.json.gz
```

### gz vs bz2: which to download?

Both compress the same JSON; pick by what you optimise for:

- **`.bz2` compresses better**: the file is noticeably smaller to download and
  store.
- **`.gz` decompresses *far* faster**: bzip2 decompression is several times
  slower, and since wdgrep itself is fast the decompressor is usually the
  bottleneck.

So if you only stream the dump through once, `.bz2` saves bandwidth. **But if
you'll process the dump repeatedly (different filters, building a graph, then
filtering again), prefer the `.gz` dump:** you pay the decompression cost on
every pass, and gz wins by a wide margin each time. Decompress with a parallel
decompressor (`pigz -dc`, or `lbzip2 -dc` for `.bz2`) for another speedup:

```sh
# one-off pass straight from the compressed file
gzip -dc latest-all.json.gz | wdgrep --claim P31:Q5 > humans.ndjson

# repeated processing: faster with pigz, and gz over bz2
pigz -dc latest-all.json.gz | wdgrep --claim P31:Q5 > humans.ndjson
```

## Usage

```
wdgrep [OPTIONS] < dump.json > subset.ndjson
```

### Filters

| Option | Description |
| --- | --- |
| `-t, --type <type>` | Restrict to `item` or `property`. Without `--type`, all entity types are kept. |
| `-c, --claim <claim>` | Keep entities matching a claim expression (see below). |
| `--claim-file <path>` | Read the claim expression from a file (for expressions too long for the shell). Mutually exclusive with `--claim`. |
| `-i, --sitelink <sitelink>` | Keep entities matching a sitelink expression. |
| `--has-sitelinks` | Keep only entities that have at least one sitelink. |
| `--graph <file>` | Property-graph file (from `build-graph`) for graph-reachability filtering. Required by the options below. See [Graph grep](#graph-grep). |
| `--graph-include <ids>` | Comma-separated target entity ids (`Q…`/`P…`/`L…`); keep an entity only if its id can reach **any** of them (OR). ANDs with the other filters. |
| `--graph-exclude <ids>` | Comma-separated target entity ids; drop an entity if its id can reach **any** of them (OR). Takes precedence over `--graph-include`. |
| `--graph-properties <props>` | Comma-separated property IDs to follow as edges when computing reachability (default: every property in the graph file). |

### Claim expressions

A [*claim*](https://www.wikidata.org/wiki/Wikidata:Glossary#Claims_and_statements)
is a property/value pair on an entity, e.g.
[P31](https://www.wikidata.org/wiki/Property:P31) (*instance of*) =
[Q5](https://www.wikidata.org/wiki/Q5) (*human*). `--claim` keeps only entities
that have a matching claim. The basic shape is a property id `Pxx` (which must
be present, with any value), optionally followed by `:Qyy` to require the value
be one of the listed item ids:

```sh
wdgrep --claim P18                    # has any P18 (image) claim
wdgrep --claim P31:Q5                 # P31 (instance of) is Q5 (human)
wdgrep --claim P31:Q5,Q6256           # P31 is Q5 OR Q6256 (humans and countries)
```

Combine claims with three operators: `&` (AND), `|` (OR) and `~` (NOT):

```sh
wdgrep --claim 'P31:Q571&P50'         # a book (Q571) that has an author (P50)   (& = and)
wdgrep --claim 'P31:Q146|P31:Q144'    # cats (Q146) OR dogs (Q144)               (| = or)
wdgrep --claim 'P31:Q571&~P50'        # books without an author                  (~ = not)
```

`|` binds tighter than `&`, so `P31:Q571&P50|P110` means
`P31:Q571 AND (P50 OR P110)`. A comma-separated value list is just shorthand for
an `|` of the same property: `P31:Q146,Q144` is identical to `P31:Q146|P31:Q144`.

**Parentheses** group sub-expressions for arbitrary nesting. Precedence is
`~` (not) > `|` (or) > `&` (and), and whitespace around the operators is
ignored, so nested boolean filters can be written directly:

```sh
# an anime film that is neither an adaptation nor part of a series (a standalone original)
wdgrep --claim 'P31:Q20650540 & ~(P144 | P179)'

# a Japanese anime film, or a manga series that is not an adaptation
wdgrep --claim '(P31:Q20650540 & P495:Q17) | (P31:Q21198342 & ~P144)'
```

Only the value of a claim's **mainsnak** is matched (not qualifiers or
references), and values must be item ids (`Qxxx`). To reach into qualifiers,
see the qualifier notations below.

If the expression is too long for your shell (`Argument list too long`), put it
in a file and pass it with `--claim-file` (its trimmed contents are used as the
expression):

```sh
echo 'P31:Q5,Q6256' > ./claim
wdgrep --claim-file ./claim < dump.json > subset.ndjson
```

#### Qualifier matching

By default `--claim Pxx` only looks at top-level claims (the mainsnak). Two
notations also reach into **qualifiers**:

```sh
wdgrep --claim @P1814         # P1814 appears as a mainsnak OR any qualifier
wdgrep --claim ~@P1814        # P1814 appears nowhere (mainsnak or qualifier)
wdgrep --claim P31.P580       # a P31 statement that carries a P580 qualifier
wdgrep --claim P31:Q5.P580    # a P31=Q5 statement carrying a P580 qualifier
wdgrep --claim P31.P642:Q100  # a P31 statement whose P642 qualifier is Q100
```

- `@P` (**deep**): match the property at *any* depth, as a mainsnak or as a
  qualifier of any statement. Combine with `~` as `~@P` (note the order). The
  parent statement is not constrained.
- `Pa.Pb` (**scoped**): match a statement of `Pa` (satisfying its own value
  constraint, if given) that carries a `Pb` qualifier on the **same** statement.

Both compose with `& | ~`. **References are not searched** by either notation. Qualifier value
constraints accept item ids (`:Qxxx`) only, like the mainsnak; string-valued
qualifiers (e.g. *name in kana*) are matched by presence (`P31.P1814`), not by
value.

### Sitelink expressions

```sh
wdgrep --sitelink enwiki              # has an English Wikipedia article
wdgrep --sitelink 'zhwiki&frwiki'     # Chinese AND French articles
wdgrep --sitelink 'ruwiki|ruwikiquote'
wdgrep --has-sitelinks                # has at least one sitelink (any project)
```

`A&B|C` is interpreted as `A AND (B OR C)` (`|` binds tighter than `&`, the
opposite of C). To keep entities that have *any* sitelink at all (rather than a
specific one), use `--has-sitelinks`.

### Graph grep

Imagine you want a list of **every chain store** in the dump: 7-Eleven,
McDonald's, Gold's Gym, Jiffy Lube, and thousands more. This is hard. There are
many kinds of chain (convenience stores, restaurants, gyms, car-repair shops,
self-storage, and so on), and Wikidata gives each kind its own specific type, not
one shared `chain` label. So a single `--claim` cannot match them all.

Wikidata stores its types as a **tree**. The general type `chain` sits at the top,
more specific types sit under it (each linked to its parent by
**[P279](https://www.wikidata.org/wiki/Property:P279)**, *subclass of*), and a
real entity links to a type near the bottom with
**[P31](https://www.wikidata.org/wiki/Property:P31)** (*instance of*):

- **[chain](https://www.wikidata.org/wiki/Q65553774)** (Q65553774): the target
  - **[retail chain](https://www.wikidata.org/wiki/Q507619)** (Q507619): subclass of chain
    - **[convenience store chain](https://www.wikidata.org/wiki/Q76213979)** (Q76213979): subclass of retail chain
      - **[7-Eleven](https://www.wikidata.org/wiki/Q259340)** (Q259340)
      - **[Lawson](https://www.wikidata.org/wiki/Q1557223)** (Q1557223)
      - **[FamilyMart](https://www.wikidata.org/wiki/Q11247682)** (Q11247682)
    - **[restaurant chain](https://www.wikidata.org/wiki/Q18534542)** (Q18534542): subclass of retail chain
      - **[McDonald's](https://www.wikidata.org/wiki/Q38076)** (Q38076)
      - **[Burger King](https://www.wikidata.org/wiki/Q177054)** (Q177054)
    - **[Costco](https://www.wikidata.org/wiki/Q715583)** (Q715583): instance of retail chain
  - **[fitness center chain](https://www.wikidata.org/wiki/Q76223357)** (Q76223357): subclass of chain
    - **[Gold's Gym](https://www.wikidata.org/wiki/Q1536234)** (Q1536234)
    - **[Anytime Fitness](https://www.wikidata.org/wiki/Q4778364)** (Q4778364)
  - **[automobile repair shop chain](https://www.wikidata.org/wiki/Q130639613)** (Q130639613): subclass of chain
    - **[Jiffy Lube](https://www.wikidata.org/wiki/Q6192247)** (Q6192247)
    - **[Pep Boys](https://www.wikidata.org/wiki/Q3375007)** (Q3375007)

7-Eleven does not point to `chain` directly; it is two P279 steps below it. So
"every chain" really means "every entity whose type is `chain`, or any subclass of
it, at any depth." To find them, you have to follow the P31 and P279 links step by
step.

`--claim` only checks the types an entity points to **directly** (one step). It
cannot follow P279 up from *convenience store chain* to *chain*. To match the same
entities with `--claim`, you would have to write out every type by hand:

```sh
# you must list every chain type by hand (these 7 are only a few of ~100)
wdgrep --claim 'P31:Q65553774,Q507619,Q76213979,Q18534542,Q76223357,Q130639613,Q132731329'
```

But you cannot even know that full list (about 100 types) without following the
P279 links first, and the list breaks as soon as a new subclass is added.

`wdgrep build-graph` follows those links for you. It reads the dump once and
writes a small graph file. The filter mode then uses that file to keep every
entity that can *reach* your target type. You build the graph once, then query it
as many times as you want.

#### Step 1: build the graph

For the type-hierarchy use case you only need **P31** (*instance of*) and
**P279** (*subclass of*):

```sh
wdgrep build-graph --properties P31,P279 < latest-all.json > graph.ndjson
```

Output is one line per entity that has at least one of the requested
properties:

```json
{"id":"Q5","P31":["Q215627"],"P279":["Q1884349"]}
```

Only entity-valued mainsnaks are collected. Properties without
entity-valued statements are omitted from the output line.

To capture **every** entity-valued property without naming them up front
(e.g. also pulling in P137 *operator*, P276 *location*, and so on), omit
`--properties`. This is slower and produces a larger graph file, but lets you
reuse one graph for many different queries:

```sh
wdgrep build-graph < latest-all.json > graph-full.ndjson
```

#### Step 2: filter the full dump by graph reachability

Pass the graph back into the filter mode with `--graph`. The graph file is
loaded into memory once at startup — in parallel across all CPU cores, so even a
multi-GB all-properties graph loads quickly — and then an entity is kept only if
its id can *reach* one of the target QIDs by following the graph edges, so a
class **and all of its subclasses and their instances** are matched in one pass:

```sh
# Everything that reaches "chain" (Q65553774) — uses the P31,P279 graph from Step 1
wdgrep --graph graph.ndjson --graph-include Q65553774 < latest-all.json > chains.ndjson
```

That single command keeps **7-Eleven**, **McDonald's**,
**[Gold's Gym](https://www.wikidata.org/wiki/Q1536234)**, and more, even though
none of them carry `P31:Q65553774` directly; they reach it through one or two
P279 steps. The `chain` class has ~100 subclasses in total, covering ~11k
entities, all matched in a single pass.

`--graph-include` / `--graph-exclude` take **comma-separated** target
entity ids. Multiple targets are combined with **OR** (reach *any* of them), and
the graph test is **AND**ed with your other filters (`--claim`, `--type`, …):

```sh
wdgrep --graph graph.ndjson --graph-include Q5,Q6256          # reaches Q5 OR Q6256
wdgrep --graph graph.ndjson --graph-include Q65553774 --graph-exclude Q18534542    # chains, but not restaurant chains
wdgrep --graph graph.ndjson --graph-include Q65553774 --claim P17:Q30              # AND a P17:Q30 (country: USA) claim
wdgrep --graph graph.ndjson --graph-include Q65553774 --graph-properties P31,P279  # follow only the type hierarchy
```

By default every property in the graph file is followed as an edge.
`--graph-properties` restricts traversal to the listed ones — useful when the
graph was built without `--properties` (all edges present) but you only want
to follow a subset:

```sh
# Full graph built once; restrict traversal to the type hierarchy at query time
wdgrep --graph graph-full.ndjson --graph-include Q65553774 --graph-properties P31,P279 < latest-all.json > chains.ndjson
```

The graph is followed **as defined**, so targets aren't limited to items —
properties (`P…`) and lexemes (`L…`) are nodes too. With a full graph you can
walk the *property* hierarchy the same way:

```sh
# every property that is transitively a subproperty of P17 (country)
wdgrep --graph graph-full.ndjson --graph-include P17 --graph-properties P1647 < latest-all.json
```

`--graph-exclude` takes precedence over `--graph-include` (a reachable
exclude target drops the entity even if it also reaches an include
target). Unless `--quiet`, a one-line summary is printed to stderr so you can
confirm the targets actually matched something:

```
graph loaded: 12345678 edges, include reachable: 11417 ids, exclude reachable: 0 ids
```

(`include reachable: 0 ids` means the include target matched nothing,
usually a wrong id.)

#### `build-graph` options

| Option | Description |
| --- | --- |
| `--properties <props>` | Comma-separated property IDs to extract (e.g. `P279,P31`). When omitted, every property with an entity-valued mainsnak is extracted. |
| `-j, --threads <n>` | Worker threads for parsing/extraction (default: number of CPUs; `-j1` is fully sequential). Output order is always preserved. |
| `--line-buffered` | Flush each matching line as it is written instead of block-buffering. Implies single-threaded processing; auto-enabled when stdout is a TTY. |
| `-q, --quiet` | Suppress the progress bar and informational stderr output. |

### Formatters

| Option | Description |
| --- | --- |
| `-o, --omit <attrs>` | Comma-separated attributes to drop (`labels,claims,...`). |
| `-k, --keep <attrs>` | The inverse of `--omit`; attributes to keep. |
| `--keep-languages <langs>` | Keep only these languages in labels/descriptions/aliases/sitelinks. |
| `--keep-claims <props>` | Keep only these claim properties within `claims` (comma-separated, kept in order). |

These options *select and project*: they pick which entities and which
attributes to emit, but never reshape claim values; the raw claim/snak structure
passes through unchanged. (wdgrep has no `--simplify`; pipe to `jq` or
`wikibase-dump-filter --simplify` when you need flattened values.)

```sh
# keep only id, labels and claims
wdgrep --claim P31:Q5 --keep id,labels,claims < dump.json

# drop sitelinks and aliases from the output
wdgrep --claim P31:Q5 --omit sitelinks,aliases < dump.json

# keep only English and Japanese labels/descriptions/aliases
wdgrep --claim P31:Q5 --keep-languages en,ja < dump.json
```

`--keep-claims` narrows `claims` to selected properties in one pass, handy for,
e.g., extracting just the P31/P279 (instance-of / subclass-of) taxonomy while
dropping every other claim (the statements are kept in full, not flattened):

```sh
pigz -dc latest-all.json.gz \
  | wdgrep --keep-claims P31,P279 --keep id,type,claims \
  > taxonomy.ndjson
```

### Other options

| Option | Description |
| --- | --- |
| `-j, --threads <n>` | Worker threads for parsing/filtering (default: number of CPUs; `-j1` is fully sequential). Output order is always preserved. |
| `--line-buffered` | Flush each matching line as it is written instead of block-buffering. Implies single-threaded; auto-enabled when stdout is a terminal. |
| `-q, --quiet` | Suppress the progress bar and informational stderr output. |
| `-V, --version` | Print the version. |
| `-h, --help` | Print help. |

## Benchmark

Extracting every human (`--claim P31:Q5`) from a 300,000-entity slice of the
Wikidata dump (865 MB gzip, 5.9 GB uncompressed) on a 16-core machine, timed
with [`hyperfine`](https://github.com/sharkdp/hyperfine). Every tool reads the
same `.gz` through `pigz -dc` and keeps the same set of entities.

| Tool | Command | Mean time | Relative |
| --- | --- | --- | --- |
| **wdgrep** | `pigz -dc dump.json.gz \| wdgrep --claim P31:Q5` | **8.5 s** | **1.0×** |
| wikibase-dump-filter (parallel) | `pigz -dc dump.json.gz \| parallel --pipe --block 100M --line-buffer "wikibase-dump-filter --claim P31:Q5"` | 22.7 s | 2.7× |
| wikibase-dump-filter | `pigz -dc dump.json.gz \| wikibase-dump-filter --claim P31:Q5` | 42.9 s | 5.0× |
| jq | `pigz -dc dump.json.gz \| jq -c 'select(any(.claims.P31[]?; .mainsnak.datavalue.value.id == "Q5"))'` | 70.9 s | 8.3× |

wdgrep is parallel out of the box, so it stays fastest without a `parallel`
wrapper. See [`benchmarks/`](benchmarks/) for the full setup and one-command reproduction.

## License

[MIT](LICENSE)
