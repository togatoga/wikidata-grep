//! wdgrep — filter and format a newline-delimited JSON stream of Wikibase
//! entities. A Rust reimplementation of `wikibase-dump-filter`.

mod build_graph;
mod cli;
mod filter;
mod format;
mod graph;
mod parallel;
mod parse;
mod process;
mod progress;
mod runner;

use std::fs;
use std::io::Write;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Parser;

use cli::{Cli, Commands};
use filter::Filter;
use format::Formatter;
use process::process_line;

fn main() -> ExitCode {
    let args = Cli::parse();
    let result = match args.command {
        Some(Commands::BuildGraph(ref bg)) => build_graph::run(bg),
        None => run(args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Cli) -> Result<()> {
    let claim = resolve_claim(args.claim.as_deref(), args.claim_file.as_deref())?;

    let mut filter = Filter::build(
        args.r#type.as_deref(),
        claim.as_deref(),
        args.sitelink.as_deref(),
        args.has_sitelinks,
    )?;

    // Graph-reachability predicate: load the graph DB once and precompute the
    // include/exclude reachable id-sets. --graph-include/-exclude need a --graph.
    match args.graph.as_deref() {
        Some(path) => {
            let reach = graph::GraphReach::load(
                path,
                &args.graph_include,
                &args.graph_exclude,
                &args.graph_properties,
                args.common.quiet,
            )?;
            if !args.common.quiet {
                eprintln!("{}", reach.summary());
            }
            filter.graph = Some(reach);
        }
        None if !args.graph_include.is_empty()
            || !args.graph_exclude.is_empty()
            || !args.graph_properties.is_empty() =>
        {
            bail!("--graph-include/--graph-exclude/--graph-properties require --graph");
        }
        None => {}
    }

    let formatter = Formatter::build(
        args.keep.as_deref(),
        args.omit.as_deref(),
        args.keep_claims.as_deref(),
        args.keep_languages.as_deref(),
    )?;

    runner::run(args.input, &args.common, |want_id| {
        move |line: &[u8], out: &mut dyn Write| {
            process_line(line, &filter, &formatter, want_id, out)
        }
    })
}

/// Resolve the claim expression from the inline `--claim` or the `--claim-file`
/// path (read its trimmed contents). The two are mutually exclusive (enforced by
/// clap), so at most one is `Some`; a missing/unreadable file is a hard error.
fn resolve_claim(claim: Option<&str>, claim_file: Option<&str>) -> Result<Option<String>> {
    match (claim, claim_file) {
        (Some(expr), _) => Ok(Some(expr.to_string())),
        (None, Some(path)) => {
            let content = fs::read_to_string(path)
                .with_context(|| format!("cannot read claim file {path}"))?;
            Ok(Some(content.trim().to_string()))
        }
        (None, None) => Ok(None),
    }
}
