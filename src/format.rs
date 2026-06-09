//! Entity formatting: attribute selection (keep/omit) and language filtering.
//! Port of `format_entity.js` and `get_format_data.js`.

use anyhow::{Result, bail};
use serde_json::{Map, Value};

/// An order-preserving JSON object (serde_json's `Map` is backed by `IndexMap`
/// when the `preserve_order` feature is on).
type Object = Map<String, Value>;

/// Canonical entity attribute order, used for `--omit`.
const ATTRIBUTES: &[&str] = &[
    "id",
    "type",
    "labels",
    "descriptions",
    "aliases",
    "claims",
    "sitelinks",
];

const ATTRIBUTES_WITH_LANGUAGES: &[&str] = &["labels", "descriptions", "aliases"];

const LANGUAGE_PROJECTS: &[&str] = &[
    "wiki",
    "wikiquote",
    "wikivoyage",
    "wikiversity",
    "wikinews",
    "wikibooks",
];

pub struct Formatter {
    /// Attributes to keep, in output order. `None` means "keep everything".
    keep: Option<Vec<String>>,
    /// Claim properties to keep within `claims`, in output order.
    /// `None` means "keep all claim properties".
    keep_claims: Option<Vec<String>>,
    keep_languages: Option<Vec<String>>,
}

impl Formatter {
    pub fn build(
        keep: Option<&[String]>,
        omit: Option<&[String]>,
        keep_claims: Option<&[String]>,
        keep_languages: Option<&[String]>,
    ) -> Result<Formatter> {
        if omit.is_some() && keep.is_some() {
            bail!("use either omit or keep");
        }
        if let Some(o) = omit {
            validate_attributes("omit", o)?;
        }
        if let Some(k) = keep {
            validate_attributes("keep", k)?;
        }
        if let Some(props) = keep_claims {
            validate_properties(props)?;
        }
        // No validation of keep_languages: Wikidata language codes have no formal
        // regex spec (the source of truth is a curated list, see
        // <https://www.wikidata.org/wiki/Wikidata:Lists/languages>), so any regex
        // would reject real codes (e.g. `es-419`, `simple`, `nan-latn-tailo`).
        // An unknown code simply matches no labels/sitelinks — grep semantics.

        // Resolve keep from omit (difference over the canonical attribute order).
        let keep = match (keep, omit) {
            (Some(k), _) => Some(k.to_vec()),
            (None, Some(o)) => Some(
                ATTRIBUTES
                    .iter()
                    .filter(|a| !o.iter().any(|x| x == *a))
                    .map(|s| s.to_string())
                    .collect(),
            ),
            (None, None) => None,
        };

        Ok(Formatter {
            keep,
            keep_claims: keep_claims.map(|p| p.to_vec()),
            keep_languages: keep_languages.map(|l| l.to_vec()),
        })
    }

    /// Returns true when no formatting options are set and the entity can be
    /// passed through as raw bytes without re-serialization.
    pub fn is_noop(&self) -> bool {
        self.keep.is_none() && self.keep_claims.is_none() && self.keep_languages.is_none()
    }

    pub fn format(&self, entity: Value) -> Value {
        let mut obj = match entity {
            Value::Object(m) => m,
            _ => Object::new(),
        };

        // Keep only the desired attributes, in the requested order.
        if let Some(keep) = &self.keep {
            obj = pick_in_order(&obj, keep);
        }

        // Narrow `claims` to the requested properties, in the requested order.
        if let Some(props) = &self.keep_claims
            && let Some(claims) = obj.get("claims").and_then(|v| v.as_object())
        {
            let narrowed = pick_in_order(claims, props);
            obj.insert("claims".to_string(), Value::Object(narrowed));
        }

        // Filter languages on labels/descriptions/aliases (and sitelinks).
        if let Some(langs) = &self.keep_languages {
            for attr in ATTRIBUTES_WITH_LANGUAGES {
                if let Some(attr_obj) = obj.get(*attr).and_then(|v| v.as_object()) {
                    let picked = pick_in_order(attr_obj, langs);
                    obj.insert((*attr).to_string(), Value::Object(picked));
                }
            }
            let keep_sitelinks = self
                .keep
                .as_ref()
                .map(|k| k.iter().any(|a| a == "sitelinks"))
                .unwrap_or(true);
            if keep_sitelinks && let Some(sl) = obj.get("sitelinks").and_then(|v| v.as_object()) {
                let kept = keep_matching_sitelinks(sl, langs);
                obj.insert("sitelinks".to_string(), Value::Object(kept));
            }
        }

        Value::Object(obj)
    }
}

/// Build a new object containing the given keys, in the given order, copying the
/// values that are present in `src` (lodash `pick` over an ordered key list).
fn pick_in_order(src: &Object, keys: &[String]) -> Object {
    let mut out = Object::new();
    for k in keys {
        if let Some(v) = src.get(k) {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// Keep sitelinks belonging to one of the requested languages' projects.
/// Port of `keep_matching_sitelinks.js`.
fn keep_matching_sitelinks(sitelinks: &Object, langs: &[String]) -> Object {
    let mut out = Object::new();
    for (name, v) in sitelinks {
        'lang: for lang in langs {
            // Split the sitelink name on the language code; the suffix must be a
            // known project name (e.g. "frwiki" -> lang "fr", project "wiki").
            let parts: Vec<&str> = name.split(lang.as_str()).collect();
            if parts.len() > 1 && LANGUAGE_PROJECTS.contains(&parts[1]) {
                out.insert(name.clone(), v.clone());
                break 'lang;
            }
        }
    }
    out
}

fn validate_attributes(label: &str, values: &[String]) -> Result<()> {
    for v in values {
        if !ATTRIBUTES.contains(&v.as_str()) {
            bail!(
                "invalid value for {label}: {v}\nPossible values: {}",
                ATTRIBUTES.join(",")
            );
        }
    }
    Ok(())
}

fn validate_properties(props: &[String]) -> Result<()> {
    for p in props {
        if !crate::filter::is_plain_property_id(p) {
            bail!("invalid claim property for keep-claims: {p}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_sitelinks_by_language_project() {
        let sitelinks: Value = serde_json::from_str(
            r#"{"frwiki":{"title":"Chat"},"enwiki":{"title":"Cat"},"frwikiquote":{"title":"Chat"}}"#,
        )
        .unwrap();
        let kept = keep_matching_sitelinks(sitelinks.as_object().unwrap(), &["fr".to_string()]);
        // Only the French projects survive, original insertion order preserved.
        let keys: Vec<&str> = kept.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec!["frwiki", "frwikiquote"]);
    }
}
