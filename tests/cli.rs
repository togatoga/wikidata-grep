//! End-to-end tests that run the `wdgrep` binary on a small NDJSON fixture.

use std::io::Write;
use std::process::{Command, Stdio};

/// A compact dump: opening bracket, two items, one property, closing bracket.
/// Lines have the trailing comma that real dumps produce.
const DUMP: &str = r#"[
{"type":"item","id":"Q1","labels":{"en":{"language":"en","value":"one"},"fr":{"language":"fr","value":"un"}},"descriptions":{"en":{"language":"en","value":"the number one"}},"aliases":{},"claims":{"P31":[{"mainsnak":{"snaktype":"value","property":"P31","datavalue":{"value":{"entity-type":"item","numeric-id":5,"id":"Q5"},"type":"wikibase-entityid"},"datatype":"wikibase-item"},"type":"statement","rank":"normal"}]},"sitelinks":{"enwiki":{"site":"enwiki","title":"One"},"frwiki":{"site":"frwiki","title":"Un"}}},
{"type":"item","id":"Q2","labels":{"en":{"language":"en","value":"two"}},"claims":{"P31":[{"mainsnak":{"snaktype":"value","property":"P31","datavalue":{"value":{"entity-type":"item","numeric-id":1,"id":"Q1"},"type":"wikibase-entityid"},"datatype":"wikibase-item"},"type":"statement","rank":"normal"}]},"sitelinks":{"enwiki":{"site":"enwiki","title":"Two"}}},
{"type":"property","id":"P31","datatype":"wikibase-item","labels":{"en":{"language":"en","value":"instance of"}},"claims":{}},
]"#;

fn run(args: &[&str]) -> String {
    let bin = env!("CARGO_BIN_EXE_wdgrep");
    let mut child = Command::new(bin)
        .args(args)
        .arg("--quiet")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn wdgrep");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(DUMP.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "wdgrep exited with failure");
    String::from_utf8(out.stdout).expect("utf8 output")
}

fn ids(output: &str) -> Vec<String> {
    output
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["id"].as_str().unwrap().to_string()
        })
        .collect()
}

#[test]
fn default_keeps_all_types() {
    assert_eq!(ids(&run(&[])), vec!["Q1", "Q2", "P31"]);
}

#[test]
fn type_item() {
    assert_eq!(ids(&run(&["--type", "item"])), vec!["Q1", "Q2"]);
}

#[test]
fn type_property() {
    assert_eq!(ids(&run(&["--type", "property"])), vec!["P31"]);
}

#[test]
fn claim_value_filter() {
    assert_eq!(ids(&run(&["--claim", "P31:Q5"])), vec!["Q1"]);
}

#[test]
fn claim_presence_filter() {
    assert_eq!(ids(&run(&["--claim", "P31"])), vec!["Q1", "Q2"]);
}

#[test]
fn claim_or_values() {
    assert_eq!(ids(&run(&["--claim", "P31:Q5,Q1"])), vec!["Q1", "Q2"]);
}

#[test]
fn negated_claim() {
    // Q1 has P31:Q5; everything else (Q2 and the property P31) lacks it.
    assert_eq!(ids(&run(&["--claim", "~P31:Q5"])), vec!["Q2", "P31"]);
}

#[test]
fn parenthesised_claim() {
    // Q1 is P31:Q5, Q2 is P31:Q1. `(P31:Q5|P31:Q1)&P31` keeps both;
    // grouping with parens behaves like the flattened CNF form.
    assert_eq!(
        ids(&run(&["--claim", "(P31:Q5|P31:Q1)&P31"])),
        vec!["Q1", "Q2"]
    );
    // ~(P31:Q5) keeps everything without P31:Q5 (Q2 and the property P31).
    assert_eq!(ids(&run(&["--claim", "~(P31:Q5)"])), vec!["Q2", "P31"]);
    // De Morgan: ~(P31:Q5 & P31:Q1) keeps everything lacking both values.
    assert_eq!(
        ids(&run(&["--claim", "~(P31:Q5&P31:Q1)"])),
        vec!["Q1", "Q2", "P31"]
    );
    // Whitespace around operators/parens is accepted.
    assert_eq!(
        ids(&run(&["--claim", "( P31:Q5 | P31:Q1 ) & P31"])),
        vec!["Q1", "Q2"]
    );
}

#[test]
fn sitelink_filter() {
    assert_eq!(ids(&run(&["--sitelink", "frwiki"])), vec!["Q1"]);
}

#[test]
fn keep_attribute_order_is_preserved() {
    let out = run(&["--claim", "P31:Q5", "--keep", "labels,id"]);
    let line = out.lines().next().unwrap();
    // keep order should drive output key order.
    assert!(line.starts_with(r#"{"labels":"#), "got: {line}");
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(
        v.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["labels", "id"]
    );
}

#[test]
fn languages_filter() {
    let out = run(&["--claim", "P31:Q5", "--keep-languages", "en"]);
    let v: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    let langs: Vec<&String> = v["labels"].as_object().unwrap().keys().collect();
    assert_eq!(langs, vec!["en"]);
}

#[test]
fn has_sitelinks_filter() {
    // Q1 and Q2 have sitelinks; the property P31 has none.
    assert_eq!(ids(&run(&["--has-sitelinks"])), vec!["Q1", "Q2"]);
}

#[test]
fn keep_claims_selects_properties() {
    // Q1 has only P31; restricting to P31,P279 keeps P31 and drops the rest.
    let out = run(&["--claim", "P31", "--keep-claims", "P31,P279"]);
    let v: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    let props: Vec<&String> = v["claims"].as_object().unwrap().keys().collect();
    // Only requested properties that exist are present (here: P31).
    assert_eq!(props, vec!["P31"]);
    // Claims keep their raw statement shape (no simplification).
    assert_eq!(
        v["claims"]["P31"][0]["mainsnak"]["datavalue"]["value"]["id"],
        serde_json::json!("Q5")
    );
}

#[test]
fn line_buffered_output_matches() {
    // Buffering must not change the bytes, only when they are flushed.
    for args in [&[][..], &["--claim", "P31"][..]] {
        let normal = run(args);
        let buffered = run(&[args, &["--line-buffered"]].concat());
        assert_eq!(normal, buffered, "byte mismatch for {args:?}");
    }
}

#[test]
fn line_buffered_streams_before_eof() {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::time::Duration;

    let bin = env!("CARGO_BIN_EXE_wdgrep");
    let mut child = Command::new(bin)
        .args(["--line-buffered", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn wdgrep");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    // Read the first output line on a helper thread so we can time out.
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        if BufReader::new(stdout).read_line(&mut line).is_ok() {
            let _ = tx.send(line);
        }
    });

    // Write one entity but keep stdin OPEN: only line buffering can surface it.
    writeln!(stdin, r#"{{"type":"item","id":"Q1","claims":{{}}}}"#).unwrap();
    stdin.flush().unwrap();

    let got = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("no incremental output before EOF");
    let v: serde_json::Value = serde_json::from_str(got.trim()).unwrap();
    assert_eq!(v["id"], "Q1");

    drop(stdin); // EOF -> child finishes
    let _ = reader.join();
    let _ = child.wait();
}

#[test]
fn parallel_matches_sequential() {
    // Output must be identical regardless of thread count.
    for args in [
        &[][..],
        &["--claim", "P31"][..],
        &["--keep", "id,claims"][..],
    ] {
        let seq = run(&[args, &["-j1"]].concat());
        let par = run(&[args, &["-j4"]].concat());
        assert_eq!(seq, par, "mismatch for {args:?}");
    }
}

#[test]
fn build_graph_extracts_entity_claims() {
    let out = run(&["build-graph", "--properties", "P31"]);
    assert_eq!(
        out,
        "{\"id\":\"Q1\",\"P31\":[\"Q5\"]}\n{\"id\":\"Q2\",\"P31\":[\"Q1\"]}\n"
    );
}

#[test]
fn build_graph_all_properties_extracts_every_entity_claim() {
    // Without --properties, every entity-valued property is extracted.
    // The fixture only has P31 (all entity-valued).
    let out = run(&["build-graph"]);
    assert_eq!(
        out,
        "{\"id\":\"Q1\",\"P31\":[\"Q5\"]}\n{\"id\":\"Q2\",\"P31\":[\"Q1\"]}\n"
    );
}

#[test]
fn build_graph_all_properties_parallel_matches_sequential() {
    let base = &["build-graph"][..];
    let seq = run(&[base, &["-j1"]].concat());
    let par = run(&[base, &["-j4"]].concat());
    let lb = run(&[base, &["--line-buffered"]].concat());
    assert_eq!(seq, par, "parallel output differs from sequential");
    assert_eq!(seq, lb, "line-buffered output differs from sequential");
}

#[test]
fn build_graph_parallel_matches_sequential() {
    // build-graph output must be byte-identical across thread counts and the
    // line-buffered (sequential) path.
    let base = &["build-graph", "--properties", "P31"][..];
    let seq = run(&[base, &["-j1"]].concat());
    let par = run(&[base, &["-j4"]].concat());
    let lb = run(&[base, &["--line-buffered"]].concat());
    assert_eq!(seq, par, "parallel output differs from sequential");
    assert_eq!(seq, lb, "line-buffered output differs from sequential");
}
