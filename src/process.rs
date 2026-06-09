//! Shared per-line processing used by both the sequential and parallel paths.

use std::io::{self, Write};

use sonic_rs::JsonValueTrait;

use crate::filter::Filter;
use crate::format::Formatter;
use crate::parse;

/// What happened to one input line.
#[derive(Default)]
pub struct LineOutcome {
    /// The line is an entity line (counts toward the total processed), whether
    /// or not it was actually parsed — pre-filtered-out lines still count.
    pub is_entity: bool,
    /// The entity passed the filter and was written to `out`.
    pub kept: bool,
    /// The kept entity's id (only when `want_id` was requested, for progress).
    pub last_id: Option<String>,
}

/// Pre-filter, parse, filter, format and serialize a single raw line.
///
/// On a match, the serialized entity (plus a trailing newline) is appended to
/// `out`. Writing to a `Vec<u8>` never fails; writing to a real stream may.
pub fn process_line<W: Write + ?Sized>(
    raw: &[u8],
    filter: &Filter,
    formatter: &Formatter,
    want_id: bool,
    out: &mut W,
) -> io::Result<LineOutcome> {
    // Graph-reachability gate on the raw id: reject before parsing entities
    // whose id cannot reach a --graph-include target (or reaches an exclude one).
    if let Some(g) = &filter.graph
        && !g.allows(crate::graph::entity_id(raw))
    {
        return Ok(LineOutcome {
            is_entity: parse::is_entity_line(raw),
            ..Default::default()
        });
    }

    // Cheap SIMD pre-filter on the raw bytes: skip the expensive JSON parse for
    // lines that cannot possibly match the claim/sitelink filter. Skipped lines
    // still count toward the total if they are entity lines.
    if let Some(pf) = &filter.prefilter
        && !pf.matches(raw)
    {
        return Ok(LineOutcome {
            is_entity: parse::is_entity_line(raw),
            ..Default::default()
        });
    }

    let entity = match parse::parse_line(raw) {
        Some(e) => e,
        None => return Ok(LineOutcome::default()),
    };

    let mut outcome = LineOutcome {
        is_entity: true,
        ..Default::default()
    };

    if filter.passes(&entity) {
        let id = if want_id {
            Some(
                entity
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        } else {
            None
        };
        if formatter.is_noop() {
            // No formatting needed: write the cleaned raw bytes directly,
            // skipping the parse→Value→serialize round-trip.
            out.write_all(parse::clean_line(raw).unwrap_or(raw))?;
        } else {
            // Re-parse with serde_json (order-preserving) for the formatting
            // path: sonic-rs 0.4+ scrambles built-object key order, so we use an
            // IndexMap-backed Value to keep output keys in input/requested order.
            // Only kept+formatted lines pay this extra parse.
            let cleaned = parse::clean_line(raw).unwrap_or(raw);
            let value: serde_json::Value =
                serde_json::from_slice(cleaned).map_err(io::Error::other)?;
            let formatted = formatter.format(value);
            let buf = serde_json::to_vec(&formatted).map_err(io::Error::other)?;
            out.write_all(&buf)?;
        }
        out.write_all(b"\n")?;
        outcome.kept = true;
        outcome.last_id = id;
    }

    Ok(outcome)
}
