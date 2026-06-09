//! Byte-exact snapshot tests over a shared NDJSON fixture.
//!
//! Each case runs the `wdgrep` binary on `tests/fixtures/input.ndjson` and
//! compares stdout *byte-for-byte* against `tests/fixtures/expected/<case>.ndjson`.
//! This locks in the verified-correct output so refactors can't silently drift
//! (the project's defining constraint is byte-identical output).
//!
//! To (re)generate the expected files after an intentional behaviour change:
//!
//! ```sh
//! UPDATE_FIXTURES=1 cargo test --test fixtures
//! ```
//!
//! Always confirm regenerated fixtures still match the reference
//! (`npx wikibase-dump-filter`) before committing them — see CLAUDE.md.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// (case name, input fixture, CLI args). The name is the expected-output stem.
const CASES: &[(&str, &str, &[&str])] = &[
    ("default", "input.ndjson", &[]),
    ("type-item", "input.ndjson", &["--type", "item"]),
    ("type-property", "input.ndjson", &["--type", "property"]),
    ("claim-value", "input.ndjson", &["--claim", "P31:Q5"]),
    ("claim-presence", "input.ndjson", &["--claim", "P31"]),
    ("claim-negated", "input.ndjson", &["--claim", "~P31:Q5"]),
    ("sitelink", "input.ndjson", &["--sitelink", "frwiki"]),
    ("keep", "input.ndjson", &["--keep", "id,labels,claims"]),
    ("omit", "input.ndjson", &["--omit", "sitelinks,aliases"]),
    ("languages", "input.ndjson", &["--keep-languages", "en,ja"]),
    (
        "keep-claims",
        "input.ndjson",
        &["--keep-claims", "P31,P625"],
    ),
    // Claim notations `@P` (deep: mainsnak or any qualifier) and `P.P` (scoped
    // qualifier) have no reference equivalent, so these lock in wdgrep's own
    // behaviour.
    ("ext-deep", "input-ext.ndjson", &["--claim", "@P1814"]),
    (
        "ext-deep-negated",
        "input-ext.ndjson",
        &["--claim", "~@P1814"],
    ),
    ("ext-scoped", "input-ext.ndjson", &["--claim", "P31.P1814"]),
    (
        "ext-scoped-value",
        "input-ext.ndjson",
        &["--claim", "P31.P642:Q100"],
    ),
    (
        "ext-scoped-value-miss",
        "input-ext.ndjson",
        &["--claim", "P31.P642:Q999"],
    ),
    (
        "ext-scoped-miss",
        "input-ext.ndjson",
        &["--claim", "P31.P999"],
    ),
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Run the binary with `args --quiet`, feeding the named input on stdin.
fn run(input_file: &str, args: &[&str]) -> Vec<u8> {
    let input = fs::read(fixtures_dir().join(input_file)).expect("read input fixture");
    let bin = env!("CARGO_BIN_EXE_wdgrep");
    let mut child = Command::new(bin)
        .args(args)
        .arg("--quiet")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn wdgrep");
    child.stdin.take().unwrap().write_all(&input).unwrap();
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "wdgrep exited with failure");
    out.stdout
}

#[test]
fn fixtures_match() {
    let update = std::env::var_os("UPDATE_FIXTURES").is_some();
    let expected_dir = fixtures_dir().join("expected");
    if update {
        fs::create_dir_all(&expected_dir).expect("create expected dir");
    }

    let mut failures = Vec::new();
    for (name, input_file, args) in CASES {
        let got = run(input_file, args);
        let path = expected_dir.join(format!("{name}.ndjson"));

        if update {
            fs::write(&path, &got).expect("write expected fixture");
            continue;
        }

        match fs::read(&path) {
            Ok(want) if want == got => {}
            Ok(_) => failures.push(format!("{name}: output differs from {}", path.display())),
            Err(_) => failures.push(format!(
                "{name}: missing {} (run UPDATE_FIXTURES=1 cargo test --test fixtures)",
                path.display()
            )),
        }
    }

    assert!(
        failures.is_empty(),
        "fixture mismatches:\n{}",
        failures.join("\n")
    );
}
