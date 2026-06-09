//! `wdgrep build-graph`: extract a compact property-graph from a Wikidata dump.
//!
//! For each entity that has at least one of the requested properties, emits:
//!   {"id":"Q5","P279":["Q215627"],"P31":["Q5"]}
//!
//! Uses sonic-rs for both JSON parsing and serialization.
//! A memchr prefilter skips the parse entirely for lines that lack all
//! requested properties.

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;

use anyhow::Result;
use memchr::memmem::Finder;
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Object, Value};

use crate::cli::BuildGraphArgs;
use crate::parse::{is_entity_line, trim_ascii};
use crate::process::LineOutcome;
use crate::progress::ProgressBar;
use crate::{parallel, runner};

/// Build a prefilter to skip the JSON parse for lines that cannot contribute a
/// node. In fixed-property mode that's one Finder per requested `"Pxxx"`. When
/// no properties are specified (all-properties mode) no property name is known
/// up front, so we fall back to the entity-value type marker `wikibase-entityid`
/// — a sound necessary condition: a line with no entity-valued snak yields no
/// graph node.
fn build_finders(properties: &[String]) -> Vec<Finder<'static>> {
    if properties.is_empty() {
        return vec![Finder::new(b"wikibase-entityid").into_owned()];
    }
    properties
        .iter()
        .map(|p| Finder::new(format!("\"{p}\"").as_bytes()).into_owned())
        .collect()
}

pub fn run(args: &BuildGraphArgs) -> Result<()> {
    let all_properties = args.properties.is_empty();
    let finders = build_finders(&args.properties);

    let stdout_tty = io::stdout().is_terminal();
    let show_progress = !args.quiet && io::stderr().is_terminal();
    let progress = if show_progress {
        Some(ProgressBar::new())
    } else {
        None
    };

    let (line_buffered, workers) = runner::dispatch(args.line_buffered, args.threads, stdout_tty);
    let want_id = progress.is_some();

    if workers == 1 {
        runner::run_sequential(None, progress, line_buffered, move |line, out| {
            process_graph_line(
                line,
                &finders,
                &args.properties,
                all_properties,
                want_id,
                out,
            )
        })
    } else {
        let finders = Arc::new(finders);
        let properties = Arc::new(args.properties.clone());
        parallel::run(workers, progress, None, move |line, out| {
            // Writing to a Vec is infallible, so the Err arm is unreachable.
            process_graph_line(line, &finders, &properties, all_properties, want_id, out)
                .unwrap_or_default()
        })
    }
}

/// Pre-filter, parse and extract one raw line into a graph node, appending the
/// serialized node (plus newline) to `out` on a match. Shared by the sequential
/// and parallel paths. Writing to a `Vec<u8>` never fails; writing to a real
/// stream may (e.g. broken pipe).
fn process_graph_line<W: Write + ?Sized>(
    raw: &[u8],
    finders: &[Finder<'static>],
    properties: &[String],
    all_properties: bool,
    want_id: bool,
    out: &mut W,
) -> io::Result<LineOutcome> {
    if !is_entity_line(raw) {
        return Ok(LineOutcome::default());
    }
    let entity = LineOutcome {
        is_entity: true,
        ..Default::default()
    };

    // Skip JSON parse entirely if no requested property appears in raw bytes.
    if !finders.iter().any(|f| f.find(raw).is_some()) {
        return Ok(entity);
    }

    let Some(node) = parse_and_extract(raw, properties, all_properties) else {
        return Ok(entity);
    };

    let id = if want_id {
        Some(
            node.get(&"id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        )
    } else {
        None
    };

    let mut buf = sonic_rs::to_vec(&node).map_err(io::Error::other)?;
    buf.push(b'\n');
    out.write_all(&buf)?;

    Ok(LineOutcome {
        is_entity: true,
        kept: true,
        last_id: id,
    })
}

/// Parse one raw line with sonic-rs and extract the graph node.
/// Returns `None` for non-entity lines or entities with no entity-valued claims.
///
/// In fixed-property mode only the requested `properties` are scanned, in the
/// given order. In `all_properties` mode every claim property is scanned (in the
/// dump's claim order) and any property carrying at least one entity-valued
/// mainsnak is emitted — so nothing entity-graphable is missed.
fn parse_and_extract(raw: &[u8], properties: &[String], all_properties: bool) -> Option<Object> {
    let trimmed = trim_ascii(raw);
    let cleaned = trimmed.strip_suffix(b",").unwrap_or(trimmed);
    if cleaned.first() != Some(&b'{') {
        return None;
    }

    let entity: Value = match sonic_rs::from_slice(cleaned) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("parsing error: {e}");
            return None;
        }
    };

    let id = entity["id"].as_str()?.to_string();

    let mut node = Object::new();
    node.insert("id", id.as_str());

    let mut found_any = false;
    let mut add_property = |node: &mut Object, prop: &str, statements: &Value| {
        let Some(statements) = statements.as_array() else {
            return;
        };
        let qids: Value = statements
            .iter()
            .filter_map(|stmt| snak_qid(&stmt["mainsnak"]))
            .map(|s| Value::from(&s))
            .collect();
        if !qids.as_array().map(|a| a.is_empty()).unwrap_or(true) {
            found_any = true;
            node.insert(prop, qids);
        }
    };

    if all_properties {
        if let Some(claims) = entity["claims"].as_object() {
            for (prop, statements) in claims.iter() {
                add_property(&mut node, prop, statements);
            }
        }
    } else {
        for prop in properties {
            add_property(&mut node, prop, &entity["claims"][prop.as_str()]);
        }
    }

    if found_any { Some(node) } else { None }
}

/// Extract the QID string from a mainsnak (sonic-rs Value).
/// Handles both the modern `{"id":"Q5"}` and legacy `{"entity-type":"item","numeric-id":5}` formats.
fn snak_qid(snak: &sonic_rs::Value) -> Option<String> {
    let value = &snak["datavalue"]["value"];
    // Modern format
    if let Some(id) = value["id"].as_str() {
        return Some(id.to_string());
    }
    // Legacy format
    let letter = match value["entity-type"].as_str()? {
        "item" => "Q",
        "property" => "P",
        "lexeme" => "L",
        _ => return None,
    };
    let num = value["numeric-id"].as_u64()?;
    Some(format!("{letter}{num}"))
}
