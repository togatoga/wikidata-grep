//! Entity filtering: by type, claims and sitelinks.
//! Port of `filter_entity.js` and its helpers.

use std::collections::HashSet;

use anyhow::{Result, anyhow, bail};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

mod claim;
use claim::build_claim_filter;

#[derive(Debug)]
pub enum TypeFilter {
    Any,
    Exact(String),
}

impl TypeFilter {
    pub fn matches(&self, type_: Option<&str>) -> bool {
        match self {
            TypeFilter::Any => true,
            TypeFilter::Exact(t) => type_ == Some(t.as_str()),
        }
    }
}

/// A property and optional value constraint, e.g. `P31` or `P31:Q5,Q6`.
#[derive(Debug, Clone)]
struct PropValue {
    property: String,
    /// Present when a value constraint (`:Qxxx`) was given.
    values: Option<HashSet<String>>,
}

/// One disjunctive term of a claim filter, e.g. `~P31:Q5,Q6`.
///
/// Two extra notations widen what "having a claim" means:
/// - `deep` (`@P31`): the property may appear as a mainsnak **or** as a
///   qualifier of any statement (references are not searched).
/// - `qualifier` (`P31.P580`): a statement of `property` (matching its value
///   constraint, if any) must carry the named qualifier (matching its value
///   constraint, if any) on the *same* statement.
#[derive(Debug, Clone)]
struct ClaimTerm {
    property: String,
    negated: bool,
    /// Present when a value constraint (`:Qxxx`) was given.
    values: Option<HashSet<String>>,
    /// `P31.P580` — require a qualifier on a matching statement (wdgrep ext).
    qualifier: Option<PropValue>,
    /// `@P31` — match the property at mainsnak or qualifier depth (wdgrep ext).
    deep: bool,
}

/// A full claim filter: a conjunction (AND, `&`) of disjunctions (OR, `|`).
#[derive(Debug)]
pub struct ClaimFilter {
    groups: Vec<Vec<ClaimTerm>>,
}

/// A sitelink filter: a conjunction of disjunction groups.
#[derive(Debug)]
pub struct SitelinkFilter {
    required_groups: Vec<HashSet<String>>,
}

/// A cheap, sound pre-filter on raw line bytes: an AND of OR-groups of
/// substrings. A line passes only if, for every clause, at least one of its
/// substrings is present. It is a *necessary* condition for the real filter to
/// match (never a false negative), so lines that fail it can skip JSON parsing.
pub struct Prefilter {
    clauses: Vec<Vec<memchr::memmem::Finder<'static>>>,
}

impl std::fmt::Debug for Prefilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Prefilter({} clauses)", self.clauses.len())
    }
}

impl Prefilter {
    #[inline]
    pub fn matches(&self, raw: &[u8]) -> bool {
        self.clauses
            .iter()
            .all(|alts| alts.iter().any(|f| f.find(raw).is_some()))
    }
}

#[derive(Debug)]
pub struct Filter {
    pub(crate) type_: TypeFilter,
    pub(crate) claim: Option<ClaimFilter>,
    pub(crate) sitelink: Option<SitelinkFilter>,
    require_sitelinks: bool,
    /// Sound raw-bytes pre-filter derived from the claim/sitelink filters.
    pub(crate) prefilter: Option<Prefilter>,
    /// Graph-reachability predicate (`--graph` + `--graph-include`/`-exclude`).
    /// Checked on the raw id before parsing, so it gates the JSON parse.
    pub(crate) graph: Option<crate::graph::GraphReach>,
}

const TYPES: &[&str] = &["item", "property"];

/// Match a bare property id: `P` followed by one or more digits.
pub fn is_plain_property_id(s: &str) -> bool {
    match s.strip_prefix('P') {
        Some(rest) => !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

impl Filter {
    pub fn build(
        type_opt: Option<&str>,
        claim_opt: Option<&str>,
        sitelink_opt: Option<&str>,
        require_sitelinks: bool,
    ) -> Result<Filter> {
        let type_ = build_type_filter(type_opt)?;
        let claim = match claim_opt {
            Some(c) => Some(build_claim_filter(c).map_err(anyhow::Error::msg)?),
            None => None,
        };
        let sitelink = match sitelink_opt {
            Some(s) => Some(build_sitelink_filter(s)?),
            None => None,
        };
        let prefilter = build_prefilter(&type_, claim.as_ref(), sitelink.as_ref());
        Ok(Filter {
            type_,
            claim,
            sitelink,
            require_sitelinks,
            prefilter,
            graph: None,
        })
    }

    pub fn passes(&self, entity: &Value) -> bool {
        let type_ = entity.get("type").and_then(|v| v.as_str());
        if !self.type_.matches(type_) {
            return false;
        }
        if let Some(cf) = &self.claim
            && !self.valid_claims(entity, cf)
        {
            return false;
        }
        if let Some(sf) = &self.sitelink
            && !valid_sitelinks(entity, sf)
        {
            return false;
        }
        if self.require_sitelinks && !has_any_sitelink(entity) {
            return false;
        }
        true
    }

    fn valid_claims(&self, entity: &Value, filter: &ClaimFilter) -> bool {
        let empty = Value::new_object();
        let claims = entity.get("claims").unwrap_or(&empty);
        // every conjunctive group must have at least one matching disjunctive term
        filter
            .groups
            .iter()
            .all(|group| group.iter().any(|term| self.valid_claim(claims, term)))
    }

    fn valid_claim(&self, claims: &Value, term: &ClaimTerm) -> bool {
        // `@P31` (deep) and `P31.P580` (qualifier) use plain positive/negate
        // semantics.
        if term.deep {
            return self.matches_deep(claims, term) != term.negated;
        }
        if let Some(q) = &term.qualifier {
            return self.matches_qualifier(claims, term, q) != term.negated;
        }

        let prop_claims = claims.get(term.property.as_str());
        let has_claims = prop_claims.is_some_and(nonempty_array);

        if has_claims {
            if term.negated && term.values.is_none() {
                return false;
            }
        } else if !term.negated {
            return false;
        }

        if let Some(qhash) = &term.values {
            let matched = prop_claims.is_some_and(|pc: &Value| mainsnak_id_in_set(pc, qhash));
            if matched {
                if term.negated {
                    return false;
                }
            } else if !term.negated {
                return false;
            }
        }
        true
    }

    /// `@P31`: the property appears as a mainsnak (top-level claim) or as a
    /// qualifier of any statement. References are intentionally not searched.
    /// Returns the *positive* condition (negation is applied by the caller).
    fn matches_deep(&self, claims: &Value, term: &ClaimTerm) -> bool {
        let want = term.values.as_ref();
        let mut present = false;
        let mut matched = false;

        // Top-level mainsnak values.
        if let Some(pc) = claims.get(term.property.as_str())
            && nonempty_array(pc)
        {
            present = true;
            if let Some(set) = want {
                matched |= mainsnak_id_in_set(pc, set);
            }
        }

        // Qualifier values across every statement of every property.
        if let Some(obj) = claims.as_object() {
            for (_, statements) in obj.iter() {
                for statement in statements.as_array().into_iter().flatten() {
                    if let Some(qsnaks) = statement
                        .get("qualifiers")
                        .and_then(|q| q.get(term.property.as_str()))
                        && nonempty_array(qsnaks)
                    {
                        present = true;
                        if let Some(set) = want {
                            matched |= snak_id_in_set(qsnaks, set);
                        }
                    }
                }
            }
        }

        match want {
            None => present,
            Some(_) => matched,
        }
    }

    /// `P31.P580`: at least one statement of `term.property` must satisfy the
    /// parent value constraint (on its mainsnak) *and* carry the qualifier
    /// `q.property` satisfying its value constraint, both on the same statement.
    /// Returns the *positive* condition (negation is applied by the caller).
    fn matches_qualifier(&self, claims: &Value, term: &ClaimTerm, q: &PropValue) -> bool {
        let statements = match claims
            .get(term.property.as_str())
            .and_then(|v| v.as_array())
        {
            Some(s) => s,
            None => return false,
        };
        statements
            .iter()
            .any(|statement| self.statement_matches(statement, term, q))
    }

    fn statement_matches(&self, statement: &Value, term: &ClaimTerm, q: &PropValue) -> bool {
        // Parent mainsnak value constraint, scoped to this one statement.
        if let Some(qhash) = &term.values {
            let id_ok = statement
                .get("mainsnak")
                .and_then(snak_entity_id)
                .is_some_and(|id| qhash.contains(&id));
            if !id_ok {
                return false;
            }
        }
        // Qualifier presence (and value) on the same statement.
        let qsnaks = match statement
            .get("qualifiers")
            .and_then(|quals| quals.get(q.property.as_str()))
        {
            Some(s) if nonempty_array(s) => s,
            _ => return false,
        };
        match &q.values {
            None => true,
            Some(qhash) => snak_id_in_set(qsnaks, qhash),
        }
    }
}

/// Letter prefix used to build an entity id from `entity-type` + `numeric-id`.
fn entity_letter(entity_type: &str) -> Option<&'static str> {
    Some(match entity_type {
        "item" => "Q",
        "property" => "P",
        "lexeme" => "L",
        "form" => "F",
        "sense" => "S",
        "entity-schema" => "E",
        _ => return None,
    })
}

/// The entity id a single snak points to (`"Q5"`, `"P31"`, …), if it is an
/// entity-valued snak. Claim value constraints (`:Qxxx`) are always item ids, so
/// only entity-valued snaks can ever match; other datatypes are ignored. (This
/// is the small slice of `simplifyEntity` that claim matching needs now that the
/// full `--simplify` machinery is gone.)
pub(crate) fn snak_entity_id(snak: &Value) -> Option<String> {
    let value = snak.get("datavalue")?.get("value")?;
    if let Some(id) = value.get("id").and_then(|v| v.as_str()) {
        return Some(id.to_string());
    }
    let letter = entity_letter(value.get("entity-type").and_then(|v| v.as_str())?)?;
    let numeric = value.get("numeric-id")?;
    numeric
        .as_u64()
        .map(|n| format!("{letter}{n}"))
        .or_else(|| numeric.as_str().map(|s| format!("{letter}{s}")))
}

/// True if `v` is a JSON array with at least one element.
fn nonempty_array(v: &Value) -> bool {
    v.as_array().is_some_and(|a| !a.is_empty())
}

/// True if any statement's mainsnak resolves to an entity id in `set`.
fn mainsnak_id_in_set(statements: &Value, set: &HashSet<String>) -> bool {
    statements.as_array().into_iter().flatten().any(|st| {
        st.get("mainsnak")
            .and_then(snak_entity_id)
            .is_some_and(|id| set.contains(&id))
    })
}

/// True if any snak in the array resolves to an entity id in `set`.
fn snak_id_in_set(snaks: &Value, set: &HashSet<String>) -> bool {
    snaks
        .as_array()
        .into_iter()
        .flatten()
        .any(|snak| snak_entity_id(snak).is_some_and(|id| set.contains(&id)))
}

/// `--type item` / `--type property` restrict to a single entity type. Without
/// `--type`, every entity type is kept.
fn build_type_filter(type_opt: Option<&str>) -> Result<TypeFilter> {
    match type_opt {
        None => Ok(TypeFilter::Any),
        Some(t) if TYPES.contains(&t) => Ok(TypeFilter::Exact(t.to_string())),
        Some(t) => Err(anyhow!(
            "invalid value for type: {t}\nPossible values: {}",
            TYPES.join(", ")
        )),
    }
}

fn build_sitelink_filter(spec: &str) -> Result<SitelinkFilter> {
    let required_groups: Vec<HashSet<String>> = spec
        .split('&')
        .map(|group| group.split('|').map(|s| s.to_string()).collect())
        .collect();

    for group in &required_groups {
        for name in group {
            if !is_valid_sitelink(name) {
                bail!("invalid sitelink: {name}");
            }
        }
    }
    Ok(SitelinkFilter { required_groups })
}

fn is_valid_sitelink(s: &str) -> bool {
    // /^[a-z_]{2,20}$/
    let len = s.chars().count();
    (2..=20).contains(&len) && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// A property/value/sitelink id appears in the raw JSON wrapped in quotes
/// (`"P31":`, `"id":"Q5"`, `"enwiki":`), so the quoted token is both a sound
/// necessary substring and precise enough to avoid `"Q5"` matching `"Q50"`.
fn quoted(s: &str) -> memchr::memmem::Finder<'static> {
    memchr::memmem::Finder::new(format!("\"{s}\"").as_bytes()).into_owned()
}

/// Derive a sound raw-bytes pre-filter from the type, claim and sitelink filters.
///
/// Each clause is a necessary condition (an OR of substrings, at least one must
/// be present). Groups that cannot be soundly reduced to substring presence
/// (those containing a negated term) contribute no clause, which only makes the
/// pre-filter less selective — never unsound.
fn build_prefilter(
    type_: &TypeFilter,
    claim: Option<&ClaimFilter>,
    sitelink: Option<&SitelinkFilter>,
) -> Option<Prefilter> {
    let mut clauses: Vec<Vec<memchr::memmem::Finder<'static>>> = Vec::new();

    // An exact entity type appears verbatim near the start of every matching
    // line as `"type":"item"` / `"type":"property"`, so it is a sound necessary
    // substring that lets the other type's lines skip parsing entirely. Pushed
    // first because it is usually very selective (e.g. `--type property` rejects
    // every item line), so the AND short-circuits before scanning for the rest.
    if let TypeFilter::Exact(t) = type_ {
        clauses.push(vec![
            memchr::memmem::Finder::new(format!("\"type\":\"{t}\"").as_bytes()).into_owned(),
        ]);
    }

    if let Some(cf) = claim {
        for group in &cf.groups {
            // A negated term can match by *absence*, so the group might pass
            // without any positive substring: skip the whole group.
            if group.iter().any(|t| t.negated) {
                continue;
            }
            if group.len() == 1 {
                // A single term contributes all of its necessary substrings.
                // `@P31` (deep) and `P31` share the property clause; `P31.P580`
                // additionally requires the qualifier property (and values).
                let t = &group[0];
                clauses.push(vec![quoted(&t.property)]);
                if let Some(values) = &t.values {
                    clauses.push(values.iter().map(|v| quoted(v)).collect());
                }
                if let Some(q) = &t.qualifier {
                    clauses.push(vec![quoted(&q.property)]);
                    if let Some(values) = &q.values {
                        clauses.push(values.iter().map(|v| quoted(v)).collect());
                    }
                }
            } else {
                // OR of positive terms: at least one term's (parent) property
                // must be present. For a scoped term the parent property is the
                // necessary substring; values/qualifiers are dropped to keep the
                // single OR clause sound and simple.
                clauses.push(group.iter().map(|t| quoted(&t.property)).collect());
            }
        }
    }

    if let Some(sf) = sitelink {
        for group in &sf.required_groups {
            clauses.push(group.iter().map(|n| quoted(n)).collect());
        }
    }

    if clauses.is_empty() {
        None
    } else {
        Some(Prefilter { clauses })
    }
}

/// True if the entity has at least one sitelink (non-empty `sitelinks` object).
fn has_any_sitelink(entity: &Value) -> bool {
    entity
        .get("sitelinks")
        .and_then(|v| v.as_object())
        .map(|o| !o.is_empty())
        .unwrap_or(false)
}

fn valid_sitelinks(entity: &Value, filter: &SitelinkFilter) -> bool {
    let sitelinks = entity.get("sitelinks").and_then(|v| v.as_object());
    // The filter holds only a handful of names, so probe the (potentially large)
    // sitelinks object per name instead of materialising a set of all its keys.
    filter.required_groups.iter().all(|group| {
        group
            .iter()
            .any(|name| sitelinks.is_some_and(|o| o.contains_key(name)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sitelink_name_boundaries() {
        // /^[a-z_]{2,20}$/
        assert!(is_valid_sitelink("enwiki"));
        assert!(is_valid_sitelink("zh_classical"));
        assert!(!is_valid_sitelink("e"), "too short");
        assert!(!is_valid_sitelink("a".repeat(21).as_str()), "too long");
        assert!(!is_valid_sitelink("Enwiki"), "uppercase rejected");
        assert!(!is_valid_sitelink("en-wiki"), "hyphen rejected");
    }

    #[test]
    fn type_property_prefilter_skips_items() {
        let f = Filter::build(Some("property"), None, None, false).unwrap();
        let pf = f
            .prefilter
            .expect("--type property should build a prefilter");
        assert!(pf.matches(br#"{"type":"property","id":"P31","datatype":"wikibase-item"}"#));
        // Item lines lack `"type":"property"`, so they skip parsing entirely.
        assert!(!pf.matches(br#"{"type":"item","id":"Q5","claims":{}}"#));
    }

    #[test]
    fn type_item_prefilter_skips_properties_and_nested_entity_type() {
        let f = Filter::build(Some("item"), None, None, false).unwrap();
        let pf = f.prefilter.expect("--type item should build a prefilter");
        assert!(pf.matches(br#"{"type":"item","id":"Q5","claims":{}}"#));
        assert!(!pf.matches(br#"{"type":"property","id":"P31"}"#));
        // A nested `"entity-type":"item"` must NOT falsely match `"type":"item"`
        // (the leading quote guards against it), so this property line is skipped.
        assert!(!pf.matches(
            br#"{"type":"property","id":"P31","claims":{"P1":[{"mainsnak":{"datavalue":{"value":{"entity-type":"item","id":"Q5"}}}}]}}"#
        ));
    }

    #[test]
    fn no_type_builds_no_prefilter_alone() {
        // No `--type` matches any type, so it adds no clause; without other
        // filters there is nothing to pre-filter on.
        let f = Filter::build(None, None, None, false).unwrap();
        assert!(f.prefilter.is_none());
    }

    #[test]
    fn type_prefilter_combines_with_claim() {
        // `--type property --claim P1647` needs both substrings present.
        let f = Filter::build(Some("property"), Some("P1647"), None, false).unwrap();
        let pf = f.prefilter.expect("prefilter expected");
        assert!(pf.matches(br#"{"type":"property","id":"P31","claims":{"P1647":[]}}"#));
        assert!(!pf.matches(br#"{"type":"property","id":"P31","claims":{}}"#)); // no P1647
        assert!(!pf.matches(br#"{"type":"item","id":"Q5","claims":{"P1647":[]}}"#)); // wrong type
    }
}
