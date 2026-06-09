use clap::{Args, Parser, Subcommand};

/// Filter and format a newline-delimited JSON stream of Wikibase entities.
///
/// Reads a Wikibase JSON dump (NDJSON) on stdin and writes the filtered and
/// formatted entities to stdout, one JSON object per line.
#[derive(Parser, Debug)]
#[command(
    name = "wdgrep",
    version,
    about = "Filter and format a newline-delimited JSON stream of Wikibase entities",
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Restrict to one entity type: item or property. Without --type, all types are kept.
    #[arg(short = 't', long = "type")]
    pub r#type: Option<String>,

    /// Specify the claim the entity should have to pass the filter.
    /// Example: to keep only humans: wdgrep -c P31:Q5
    #[arg(short = 'c', long = "claim")]
    pub claim: Option<String>,

    /// Read the claim expression from a file (its trimmed contents are used as
    /// the expression). Use this when the expression is too long for the shell
    /// (`Argument list too long`). Mutually exclusive with --claim.
    #[arg(long = "claim-file", conflicts_with = "claim")]
    pub claim_file: Option<String>,

    /// Specify the sitelink the entity should have to pass the filter.
    /// Example: entities with a Spanish Wikipedia article: wdgrep --sitelink eswiki
    #[arg(short = 'i', long = "sitelink")]
    pub sitelink: Option<String>,

    /// Keep only entities that have at least one sitelink.
    #[arg(long = "has-sitelinks")]
    pub has_sitelinks: bool,

    /// Comma-separated entity attributes to omit
    /// (type, labels, descriptions, aliases, claims, sitelinks).
    #[arg(short = 'o', long = "omit", value_delimiter = ',')]
    pub omit: Option<Vec<String>>,

    /// The inverse of omit: comma-separated attributes to keep.
    #[arg(short = 'k', long = "keep", value_delimiter = ',')]
    pub keep: Option<Vec<String>>,

    /// Comma-separated languages for which labels, descriptions and aliases should be kept.
    #[arg(long = "keep-languages", value_delimiter = ',')]
    pub keep_languages: Option<Vec<String>>,

    /// Keep only these claim properties within `claims`.
    /// Comma-separated property ids, kept in the given order.
    /// Example: build the P31/P279 taxonomy: wdgrep --keep-claims P31,P279
    #[arg(long = "keep-claims", value_delimiter = ',')]
    pub keep_claims: Option<Vec<String>>,

    /// Suppress the progress bar and informational stderr output.
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// Flush each matching line as it is written, instead of buffering. Makes
    /// output appear incrementally (useful for `| tee`, `| head` or debugging).
    /// Implies single-threaded processing. Auto-enabled when stdout is a
    /// terminal; pass --threads to force block-buffered parallel output.
    #[arg(long = "line-buffered")]
    pub line_buffered: bool,

    /// Number of worker threads for parsing/filtering. Defaults to the number
    /// of available CPUs. Use 1 for fully sequential processing. Output order
    /// is always preserved regardless of this value.
    #[arg(short = 'j', long = "threads")]
    pub threads: Option<usize>,

    /// Path to a property-graph file (as produced by `build-graph`, e.g. with
    /// P31,P279) used for graph-reachability filtering. Loaded into memory once
    /// at startup. Required when --graph-include / --graph-exclude are given.
    #[arg(long = "graph")]
    pub graph: Option<String>,

    /// Comma-separated target entity ids (Q…, P…, or L…): keep an entity only if
    /// its id can reach ANY of these (OR) by following graph edges. Requires
    /// --graph. ANDs with the other (non-graph) filters.
    #[arg(long = "graph-include", value_delimiter = ',')]
    pub graph_include: Vec<String>,

    /// Comma-separated target entity ids (Q…, P…, or L…): drop an entity if its id
    /// can reach ANY of these (OR) by following graph edges. Requires --graph.
    /// Takes precedence over --graph-include.
    #[arg(long = "graph-exclude", value_delimiter = ',')]
    pub graph_exclude: Vec<String>,

    /// Comma-separated property IDs to follow as edges when computing graph
    /// reachability (e.g. P279,P31). Defaults to every property present in the
    /// graph file. Lets you build the graph once with all properties (e.g.
    /// `build-graph --all-properties`) but traverse only a chosen subset.
    /// Requires --graph.
    #[arg(long = "graph-properties", value_delimiter = ',')]
    pub graph_properties: Vec<String>,

    /// Input NDJSON file to filter. Reads stdin when omitted.
    pub input: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Build a compact property-graph file from a Wikidata dump.
    ///
    /// Reads a Wikidata NDJSON dump on stdin and writes one line per entity
    /// that has at least one of the requested properties:
    ///   {"id":"Q5","P279":["Q215627"],"P31":["Q5"]}
    ///
    /// Use the output as the --graph for reachability filtering, e.g.
    /// `wdgrep --graph graph.ndjson --graph-include Q11696`.
    BuildGraph(BuildGraphArgs),
}

#[derive(Args, Debug)]
pub struct BuildGraphArgs {
    /// Comma-separated property IDs to extract (e.g. P279,P31).
    /// When omitted, every property with an entity-valued mainsnak is extracted.
    #[arg(long = "properties", value_delimiter = ',')]
    pub properties: Vec<String>,

    /// Suppress the progress bar and informational stderr output.
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// Flush each matching line as it is written, instead of buffering. Makes
    /// output appear incrementally. Implies single-threaded processing.
    /// Auto-enabled when stdout is a terminal; pass --threads to force
    /// block-buffered parallel output.
    #[arg(long = "line-buffered")]
    pub line_buffered: bool,

    /// Number of worker threads for parsing/extraction. Defaults to the number
    /// of available CPUs. Use 1 for fully sequential processing. Output order
    /// is always preserved regardless of this value.
    #[arg(short = 'j', long = "threads")]
    pub threads: Option<usize>,
}
