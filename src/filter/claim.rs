//! Claim-expression grammar: tokenizer, recursive-descent parser and CNF
//! normaliser.
//!
//! The grammar is a full boolean expression — `&` (AND), `|` (OR), `~` (NOT)
//! and parentheses for grouping, plus the per-term notations `@P` (deep) and
//! `Pa.Pb` (scoped qualifier). The frontend turns the raw string into a `ClaimAst`, then
//! `to_cnf` normalises it to the conjunctive-normal-form `groups` that the
//! evaluator and the byte-level `Prefilter` (both in the parent module) consume
//! unchanged.

use super::{ClaimFilter, ClaimTerm, PropValue, is_plain_property_id};

fn is_item_id(s: &str) -> bool {
    // /^Q[1-9][0-9]*$/
    match s.strip_prefix('Q') {
        Some(rest) => {
            let mut bytes = rest.bytes();
            matches!(bytes.next(), Some(b'1'..=b'9')) && bytes.all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// Validate a bare `Pxxx[:Qyyy,...]` (no `~`/`@`/`.` prefixes).
fn validate_prop_value(s: &str) -> Result<(), String> {
    let mut parts = s.splitn(2, ':');
    let p = parts.next().unwrap_or("");
    if !is_plain_property_id(p) {
        return Err(format!("invalid claim property: {p}"));
    }
    if let Some(q) = parts.next() {
        for v in q.split(',') {
            if !is_item_id(v) {
                return Err(format!("invalid claim value: {v}"));
            }
        }
    }
    Ok(())
}

/// Build the claim filter from the expression.
///
/// The grammar is a full boolean expression — `&` (AND), `|` (OR), `~` (NOT)
/// and **parentheses** for grouping. Operator precedence is `~` > `|` > `&`, so
/// `A&B|C` (without parens) means `A AND (B OR C)`, exactly as before. Whitespace around
/// operators/parens is ignored.
///
/// The parsed expression is normalised to conjunctive normal form — an AND of
/// OR-groups — which is the internal `ClaimFilter` representation that both the
/// evaluator (`valid_claims`) and the byte-level `Prefilter` already consume
/// unchanged. So for any paren-free input the resulting groups are identical to
/// the previous "split on `&` then `|`" parser.
pub(super) fn build_claim_filter(spec: &str) -> Result<ClaimFilter, String> {
    let tokens = tokenize(spec)?;
    let mut parser = ClaimParser {
        tokens: &tokens,
        pos: 0,
    };
    let ast = parser.parse()?;
    let groups = to_cnf(&ast, false)?;
    Ok(ClaimFilter { groups })
}

fn parse_prop_value(s: &str) -> PropValue {
    let mut parts = s.splitn(2, ':');
    let property = parts.next().unwrap_or("").to_string();
    let values = parts
        .next()
        .map(|q| q.split(',').map(|s| s.to_string()).collect());
    PropValue { property, values }
}

/// A single claim term (`[@]Pxx[:Qyy,...][.Pzz[:Qww,...]]`), with `~`/`&`/`|`/
/// `(`/`)` excluded — those are handled as operators by the parser. The term is
/// validated and returned with `negated = false`; negation is applied during
/// CNF normalisation.
fn parse_term_token(s: &str) -> Result<ClaimTerm, String> {
    // `@P31` (deep) marks "match at mainsnak or qualifier depth"; `@` disables
    // the scoped-qualifier `.` split, mirroring the previous parser.
    let (deep, rest) = match s.strip_prefix('@') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (head, qualifier) = match (deep, rest.split_once('.')) {
        (false, Some((parent, q))) => {
            validate_prop_value(parent)?;
            validate_prop_value(q)?;
            (parent, Some(parse_prop_value(q)))
        }
        _ => {
            validate_prop_value(rest)?;
            (rest, None)
        }
    };
    let PropValue { property, values } = parse_prop_value(head);
    Ok(ClaimTerm {
        property,
        negated: false,
        values,
        qualifier,
        deep,
    })
}

/// A claim-expression token.
#[derive(Debug)]
enum Token {
    LParen,
    RParen,
    And,
    Or,
    Not,
    Term(String),
}

/// Split the raw expression into tokens. `&|~()` are single-character operators;
/// any other run of non-whitespace characters is a `Term` (which keeps `:,.@`).
/// Whitespace separates tokens and is otherwise ignored.
fn tokenize(spec: &str) -> Result<Vec<Token>, String> {
    fn is_boundary(b: u8) -> bool {
        matches!(
            b,
            b' ' | b'\t' | b'\n' | b'\r' | b'(' | b')' | b'&' | b'|' | b'~'
        )
    }
    let bytes = spec.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            b')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            b'&' => {
                tokens.push(Token::And);
                i += 1;
            }
            b'|' => {
                tokens.push(Token::Or);
                i += 1;
            }
            b'~' => {
                tokens.push(Token::Not);
                i += 1;
            }
            _ => {
                // A term runs up to the next boundary byte. Boundary bytes are
                // all ASCII, and term bytes start/end on UTF-8 boundaries, so
                // the slice is always valid UTF-8.
                let start = i;
                while i < bytes.len() && !is_boundary(bytes[i]) {
                    i += 1;
                }
                tokens.push(Token::Term(spec[start..i].to_string()));
            }
        }
    }
    Ok(tokens)
}

/// The boolean abstract syntax tree of a claim expression.
#[derive(Debug)]
enum ClaimAst {
    Term(ClaimTerm),
    Not(Box<ClaimAst>),
    And(Vec<ClaimAst>),
    Or(Vec<ClaimAst>),
}

/// Recursive-descent parser. Precedence (loosest first): `&`, then `|`, then the
/// unary `~`, then a primary (a term or a parenthesised expression).
struct ClaimParser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> ClaimParser<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    fn parse(&mut self) -> Result<ClaimAst, String> {
        let ast = self.parse_and()?;
        if self.pos != self.tokens.len() {
            return Err("invalid claim expression: unexpected ')' or trailing input".to_string());
        }
        Ok(ast)
    }

    fn parse_and(&mut self) -> Result<ClaimAst, String> {
        let mut parts = vec![self.parse_or()?];
        while matches!(self.peek(), Some(Token::And)) {
            self.pos += 1;
            parts.push(self.parse_or()?);
        }
        Ok(if parts.len() == 1 {
            parts.pop().unwrap()
        } else {
            ClaimAst::And(parts)
        })
    }

    fn parse_or(&mut self) -> Result<ClaimAst, String> {
        let mut parts = vec![self.parse_unary()?];
        while matches!(self.peek(), Some(Token::Or)) {
            self.pos += 1;
            parts.push(self.parse_unary()?);
        }
        Ok(if parts.len() == 1 {
            parts.pop().unwrap()
        } else {
            ClaimAst::Or(parts)
        })
    }

    fn parse_unary(&mut self) -> Result<ClaimAst, String> {
        if matches!(self.peek(), Some(Token::Not)) {
            self.pos += 1;
            Ok(ClaimAst::Not(Box::new(self.parse_unary()?)))
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Result<ClaimAst, String> {
        match self.peek() {
            Some(Token::LParen) => {
                self.pos += 1;
                let inner = self.parse_and()?;
                match self.peek() {
                    Some(Token::RParen) => {
                        self.pos += 1;
                        Ok(inner)
                    }
                    _ => Err("invalid claim expression: unbalanced parentheses".to_string()),
                }
            }
            Some(Token::Term(s)) => {
                let term = parse_term_token(s)?;
                self.pos += 1;
                Ok(ClaimAst::Term(term))
            }
            Some(Token::And) | Some(Token::Or) => {
                Err("invalid claim expression: missing operand before '&'/'|'".to_string())
            }
            Some(Token::Not) => unreachable!("handled by parse_unary"),
            Some(Token::RParen) | None => {
                Err("invalid claim expression: expected a claim term".to_string())
            }
        }
    }
}

/// Cap on the number of OR-groups a single expression may expand to. CNF
/// conversion of an OR of ANDs is a cartesian product, so a deeply nested
/// expression can blow up; this keeps a pathological filter from exhausting
/// memory (real-world claim filters are tiny).
const MAX_CNF_GROUPS: usize = 4096;

/// Normalise the AST to conjunctive normal form: a `Vec` of OR-groups whose
/// conjunction is the whole filter. `neg` carries a pending negation down the
/// tree (De Morgan), so `~(A&B)` becomes the single group `[~A, ~B]` and
/// `~(A|B)` becomes the two groups `[~A]`, `[~B]`.
fn to_cnf(ast: &ClaimAst, neg: bool) -> Result<Vec<Vec<ClaimTerm>>, String> {
    match ast {
        ClaimAst::Term(t) => {
            let mut t = t.clone();
            if neg {
                t.negated = !t.negated;
            }
            Ok(vec![vec![t]])
        }
        ClaimAst::Not(inner) => to_cnf(inner, !neg),
        // AND under no negation, or OR under negation (De Morgan), is a
        // conjunction: concatenate the children's clauses.
        ClaimAst::And(xs) if !neg => cnf_conjunction(xs, neg),
        ClaimAst::Or(xs) if neg => cnf_conjunction(xs, neg),
        // OR under no negation, or AND under negation (De Morgan), is a
        // disjunction: distribute (cartesian product of clauses).
        ClaimAst::Or(xs) => cnf_disjunction(xs, neg),
        ClaimAst::And(xs) => cnf_disjunction(xs, neg),
    }
}

fn cnf_conjunction(xs: &[ClaimAst], neg: bool) -> Result<Vec<Vec<ClaimTerm>>, String> {
    let mut groups = Vec::new();
    for x in xs {
        groups.extend(to_cnf(x, neg)?);
        if groups.len() > MAX_CNF_GROUPS {
            return Err(too_complex());
        }
    }
    Ok(groups)
}

fn cnf_disjunction(xs: &[ClaimAst], neg: bool) -> Result<Vec<Vec<ClaimTerm>>, String> {
    // Cross-product identity: a single empty group. Folding in each child's CNF
    // unions one of its clauses into every accumulated group.
    let mut acc: Vec<Vec<ClaimTerm>> = vec![vec![]];
    for x in xs {
        let child = to_cnf(x, neg)?;
        let mut next = Vec::with_capacity(acc.len() * child.len());
        for partial in &acc {
            for clause in &child {
                let mut merged = partial.clone();
                merged.extend(clause.iter().cloned());
                next.push(merged);
                if next.len() > MAX_CNF_GROUPS {
                    return Err(too_complex());
                }
            }
        }
        acc = next;
    }
    Ok(acc)
}

fn too_complex() -> String {
    format!("claim expression is too complex (expands to more than {MAX_CNF_GROUPS} OR-groups)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals(t: &ClaimTerm) -> Option<Vec<&str>> {
        t.values.as_ref().map(|s| {
            let mut v: Vec<&str> = s.iter().map(String::as_str).collect();
            v.sort_unstable();
            v
        })
    }

    #[test]
    fn item_id_boundaries() {
        // /^Q[1-9][0-9]*$/
        assert!(is_item_id("Q5"));
        assert!(is_item_id("Q42"));
        assert!(is_item_id("Q1000000"));
        assert!(!is_item_id("Q0"), "leading zero is rejected");
        assert!(!is_item_id("Q05"), "leading zero is rejected");
        assert!(!is_item_id("Q"), "no digits");
        assert!(!is_item_id("P31"), "wrong prefix");
        assert!(!is_item_id("Q5a"), "trailing non-digit");
        assert!(!is_item_id(""));
    }

    /// Canonical string for one term: `[~][@]prop[:v1,v2][.qual[:v1,v2]]`,
    /// values sorted so the (set-backed) order is deterministic.
    fn term_repr(t: &ClaimTerm) -> String {
        let mut s = String::new();
        if t.negated {
            s.push('~');
        }
        if t.deep {
            s.push('@');
        }
        s.push_str(&t.property);
        if let Some(v) = vals(t) {
            s.push(':');
            s.push_str(&v.join(","));
        }
        if let Some(q) = &t.qualifier {
            s.push('.');
            s.push_str(&q.property);
            if let Some(qv) = &q.values {
                let mut v: Vec<&str> = qv.iter().map(String::as_str).collect();
                v.sort_unstable();
                s.push(':');
                s.push_str(&v.join(","));
            }
        }
        s
    }

    /// Build the claim filter and render its CNF groups for comparison.
    fn cnf(spec: &str) -> Vec<Vec<String>> {
        build_claim_filter(spec)
            .unwrap()
            .groups
            .iter()
            .map(|g| g.iter().map(term_repr).collect())
            .collect()
    }

    #[test]
    fn parses_plain_term() {
        let t = parse_term_token("P31:Q5,Q6").unwrap();
        assert_eq!(t.property, "P31");
        assert!(!t.negated && !t.deep && t.qualifier.is_none());
        assert_eq!(vals(&t), Some(vec!["Q5", "Q6"]));
    }

    #[test]
    fn parses_deep_prefix() {
        let t = parse_term_token("@P1814").unwrap();
        assert_eq!(t.property, "P1814");
        assert!(t.deep && !t.negated && t.qualifier.is_none());
        // `@` disables scoped-qualifier parsing: `@P31.P580` is one invalid id.
        assert!(parse_term_token("@P31.P580").is_err());
    }

    #[test]
    fn parses_scoped_qualifier_with_values() {
        let t = parse_term_token("P31:Q5.P642:Q100").unwrap();
        assert_eq!(t.property, "P31");
        assert_eq!(vals(&t), Some(vec!["Q5"]));
        let q = t.qualifier.expect("qualifier present");
        assert_eq!(q.property, "P642");
        assert_eq!(
            q.values.map(|s| s.into_iter().collect::<Vec<_>>()),
            Some(vec!["Q100".to_string()])
        );
    }

    #[test]
    fn validates_extension_grammar() {
        assert!(build_claim_filter("@P1814").is_ok());
        assert!(build_claim_filter("~@P1814").is_ok());
        assert!(build_claim_filter("P31.P580").is_ok());
        assert!(build_claim_filter("P31:Q5.P642:Q100").is_ok());
        // Bad qualifier property / value.
        assert!(build_claim_filter("P31.X9").is_err());
        assert!(build_claim_filter("P31.P642:notanid").is_err());
    }

    #[test]
    fn cnf_matches_legacy_split_for_paren_free_input() {
        // The previous parser split on `&` into groups, then each on `|` into
        // terms. The CNF normaliser must reproduce that exactly when there are
        // no parentheses.
        assert_eq!(cnf("P31:Q5"), vec![vec!["P31:Q5"]]);
        assert_eq!(cnf("P31:Q5,Q6256"), vec![vec!["P31:Q5,Q6256"]]);
        assert_eq!(cnf("P31:Q146|P31:Q144"), vec![vec!["P31:Q146", "P31:Q144"]]);
        // `&` is looser than `|`: `A&B|C` == `A AND (B OR C)`.
        assert_eq!(
            cnf("P31:Q571&P50|P110"),
            vec![vec!["P31:Q571"], vec!["P50", "P110"]]
        );
        assert_eq!(cnf("P31:Q571&~P50"), vec![vec!["P31:Q571"], vec!["~P50"]]);
        assert_eq!(cnf("@P1814"), vec![vec!["@P1814"]]);
        assert_eq!(cnf("~@P1814"), vec![vec!["~@P1814"]]);
    }

    #[test]
    fn parentheses_group_or_inside_and() {
        // (A & B) | C  ==  (A|C) & (B|C)
        assert_eq!(
            cnf("(P31:Q5&P21:Q6)|P31:Q43229"),
            vec![vec!["P31:Q5", "P31:Q43229"], vec!["P21:Q6", "P31:Q43229"],]
        );
        // Without parens, `|` already binds tighter, so this is different:
        assert_eq!(
            cnf("P31:Q5&P21:Q6|P31:Q43229"),
            vec![vec!["P31:Q5"], vec!["P21:Q6", "P31:Q43229"]]
        );
        // Whitespace around operators/parens is ignored.
        assert_eq!(
            cnf("( P31:Q5 & P21:Q6 ) | P31:Q43229"),
            cnf("(P31:Q5&P21:Q6)|P31:Q43229")
        );
    }

    #[test]
    fn de_morgan_on_negated_groups() {
        // ~(A & B) == ~A | ~B  -> one OR-group with two negated terms
        assert_eq!(cnf("~(P31:Q5&P50)"), vec![vec!["~P31:Q5", "~P50"]]);
        // ~(A | B) == ~A & ~B  -> two single-term groups
        assert_eq!(cnf("~(P31:Q5|P50)"), vec![vec!["~P31:Q5"], vec!["~P50"]]);
        // Double negation cancels.
        assert_eq!(cnf("~~P31:Q5"), vec![vec!["P31:Q5"]]);
        // ~((A & B) | C) == (~A | ~B) & ~C
        assert_eq!(
            cnf("~((P31:Q5&P50)|P110)"),
            vec![vec!["~P31:Q5", "~P50"], vec!["~P110"]]
        );
    }

    #[test]
    fn dnf_with_negated_term() {
        // (A & B) | (C & ~D) -> 4 OR-groups
        assert_eq!(
            cnf("(P31:Q5&P21:Q6)|(P31:Q43229&~P576)"),
            vec![
                vec!["P31:Q5", "P31:Q43229"],
                vec!["P31:Q5", "~P576"],
                vec!["P21:Q6", "P31:Q43229"],
                vec!["P21:Q6", "~P576"],
            ]
        );
    }

    #[test]
    fn rejects_malformed_expressions() {
        assert!(build_claim_filter("(P31:Q5").is_err(), "unbalanced (");
        assert!(build_claim_filter("P31:Q5)").is_err(), "unbalanced )");
        assert!(build_claim_filter("()").is_err(), "empty group");
        assert!(build_claim_filter("P31:Q5&").is_err(), "trailing &");
        assert!(build_claim_filter("&P31:Q5").is_err(), "leading &");
        assert!(build_claim_filter("P31:Q5|").is_err(), "trailing |");
        assert!(build_claim_filter("~").is_err(), "~ with no operand");
        assert!(build_claim_filter("").is_err(), "empty expression");
        // Term-level validation still applies inside parens.
        assert!(build_claim_filter("(P31:notanid)").is_err());
    }

    #[test]
    fn complex_expression_is_capped() {
        // A wide OR-of-ANDs distributes to a cartesian product of groups; an
        // absurd one must be rejected rather than blow up memory.
        let big = std::iter::repeat_n("(P1:Q1&P2:Q2&P3:Q3&P4:Q4)", 8)
            .collect::<Vec<_>>()
            .join("|");
        assert!(build_claim_filter(&big).is_err());
    }
}
