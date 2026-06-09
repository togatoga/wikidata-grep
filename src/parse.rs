//! Line parsing. Port of `parse_line.js`.

use sonic_rs::{JsonValueTrait, Value};

/// Trim and strip a trailing comma from a raw dump line, returning the cleaned
/// slice if it looks like a JSON object, or `None` otherwise.
pub fn clean_line(raw: &[u8]) -> Option<&[u8]> {
    let trimmed = trim_ascii(raw);
    let cleaned = trimmed.strip_suffix(b",").unwrap_or(trimmed);
    if cleaned.first() == Some(&b'{') {
        Some(cleaned)
    } else {
        None
    }
}

/// Parse one raw dump line into an entity object.
///
/// Returns `None` for lines that are not entity objects (e.g. the opening `[`
/// and closing `]` of a JSON array dump, or blank lines). A trailing comma is
/// stripped, matching dumps produced by `dumpJson.php`.
pub fn parse_line(raw: &[u8]) -> Option<Value> {
    let cleaned = clean_line(raw)?;
    match sonic_rs::Deserializer::from_slice(cleaned)
        .use_rawnumber()
        .deserialize::<Value>()
    {
        Ok(v) if v.is_object() => Some(v),
        Ok(_) => None,
        Err(err) => {
            eprintln!("parsing error: {err}");
            None
        }
    }
}

/// Cheaply decide whether a raw line is an entity line (its first
/// non-whitespace byte is `{`), without parsing it. Used to count the total
/// number of entities processed even for lines the pre-filter skips.
pub fn is_entity_line(raw: &[u8]) -> bool {
    raw.iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|&b| b == b'{')
}

/// Trim leading/trailing ASCII whitespace from a byte slice, mirroring
/// JavaScript's `String.prototype.trim` for the whitespace seen in dumps
/// (space, tab, CR, LF, form feed, vertical tab).
pub(crate) fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = bytes {
        if first.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = bytes {
        if last.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
}
