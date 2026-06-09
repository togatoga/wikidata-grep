//! Graph-reachability filtering support.
//!
//! `graph.jsonl` (as produced by `build-graph`) is treated as a relationship
//! DB. We load it once into a reverse adjacency (`target -> [sources]`), then
//! BFS from the requested `--graph-include` / `--graph-exclude` targets to
//! materialise the set of entity ids that can *reach* each target by following
//! graph edges. Filtering an entity is then an O(1) node-set membership test (no
//! per-entity traversal), checked before the JSON parse so non-matching lines
//! are rejected cheaply.
//!
//! Edge semantics are intentionally not interpreted: whichever properties the
//! graph stores (P31, P279, …) are all followed the same way, so an instance
//! with only P31 reaches the class hierarchy through that P31 edge with no
//! special casing. `--graph-properties` can narrow which properties are
//! followed (so a graph built with `--all-properties` can be traversed along a
//! chosen subset); without it every property in the file is an edge.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::thread;

use ahash::{AHashMap, AHashSet};
use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use crate::parse::trim_ascii;

/// A graph node: an entity id parsed into a small, comparable form. `Q`/`P`/`L`
/// ids with a numeric body use the compact variants; every other id (other
/// entity kinds, lexeme forms/senses like `L1-F2`, or a number too big for
/// `u32`) is kept verbatim as `Other`. So no id is ever silently dropped or
/// conflated with another — `Q100`, `P100` and `M100` are all distinct nodes —
/// without baking in a fragile "ids are always Q/P/L numbers that fit 30 bits"
/// assumption.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum NodeId {
    Q(u32),
    P(u32),
    L(u32),
    Other(Box<str>),
}

impl NodeId {
    fn parse(s: &str) -> NodeId {
        let bytes = s.as_bytes();
        if bytes.len() >= 2 {
            let rest = &s[1..];
            if rest.bytes().all(|b| b.is_ascii_digit())
                && let Ok(n) = rest.parse::<u32>()
            {
                match bytes[0] {
                    b'Q' => return NodeId::Q(n),
                    b'P' => return NodeId::P(n),
                    b'L' => return NodeId::L(n),
                    _ => {}
                }
            }
        }
        NodeId::Other(s.into())
    }
}

/// Precomputed reachability sets used as an extra filter predicate.
#[derive(Debug)]
pub struct GraphReach {
    /// Nodes that can reach a `--graph-include` target. `None` when none was
    /// given (every id passes the include test).
    include: Option<AHashSet<NodeId>>,
    /// Nodes that can reach a `--graph-exclude` target (empty when none given).
    exclude: AHashSet<NodeId>,
    /// Total edges read from the graph file (for the load summary).
    edges: usize,
    /// The property subset edges were restricted to (from `--graph-properties`),
    /// for the load summary. `None` means every property in the file was used.
    edge_properties: Option<Vec<String>>,
}

impl GraphReach {
    /// Load `path` and compute the include/exclude reachable id-sets for the
    /// given target QID arguments. When `properties` is non-empty only those
    /// properties are followed as edges; otherwise every property in the file is.
    pub fn load(
        path: &str,
        include: &[String],
        exclude: &[String],
        properties: &[String],
        quiet: bool,
    ) -> Result<GraphReach> {
        let include_targets = parse_node_ids(include);
        let exclude_targets = parse_node_ids(exclude);
        let allowed = parse_property_set(properties)?;

        let (reverse, edges) = load_reverse_adjacency(path, allowed.as_ref(), quiet)?;
        let include = if include_targets.is_empty() {
            None
        } else {
            Some(reachable(&reverse, &include_targets))
        };
        let exclude = if exclude_targets.is_empty() {
            AHashSet::new()
        } else {
            reachable(&reverse, &exclude_targets)
        };
        Ok(GraphReach {
            include,
            exclude,
            edges,
            edge_properties: if properties.is_empty() {
                None
            } else {
                Some(properties.to_vec())
            },
        })
    }

    /// One-line summary of what was loaded, for stderr feedback at startup:
    /// edge count plus the sizes of the include/exclude reachable id-sets (so an
    /// empty include set — i.e. a target QID that matched nothing — is visible).
    pub fn summary(&self) -> String {
        let include = match &self.include {
            Some(set) => set.len().to_string(),
            None => "all".to_string(),
        };
        let edges = match &self.edge_properties {
            Some(props) => format!("{} edges ({})", self.edges, props.join(", ")),
            None => format!("{} edges", self.edges),
        };
        format!(
            "graph loaded: {edges}, include reachable: {include} ids, exclude reachable: {} ids",
            self.exclude.len(),
        )
    }

    /// Whether an entity with this id (the raw line's first `"id":"…"`) passes
    /// the graph predicate: in the include-reachable set (if any) and not in the
    /// exclude-reachable set. A line without an entity id fails an active include.
    pub fn allows(&self, id: Option<NodeId>) -> bool {
        if let Some(inc) = &self.include {
            match &id {
                Some(i) if inc.contains(i) => {}
                _ => return false,
            }
        }
        if let Some(i) = &id
            && self.exclude.contains(i)
        {
            return false;
        }
        true
    }
}

/// Parse `["Q5", "P17", …]`-style CLI target args into nodes. Any id kind is
/// accepted (the graph is followed as defined); a target that exists nowhere in
/// the graph simply reaches nothing, which the load summary makes visible.
fn parse_node_ids(raw: &[String]) -> Vec<NodeId> {
    raw.iter().map(|s| NodeId::parse(s)).collect()
}

/// Parse `--graph-properties` into an allow-set of property-id strings, matched
/// against the graph file's property keys verbatim. Empty input → `None` (follow
/// every property in the file); anything not a `P<digits>` id errors.
fn parse_property_set(raw: &[String]) -> Result<Option<HashSet<String>>> {
    if raw.is_empty() {
        return Ok(None);
    }
    for p in raw {
        if !is_property_id(p) {
            bail!("invalid property id: {p}");
        }
    }
    Ok(Some(raw.iter().cloned().collect()))
}

/// `P<digits>` — the shape of a property key in the graph file.
fn is_property_id(s: &str) -> bool {
    matches!(s.as_bytes().split_first(),
        Some((b'P', rest)) if !rest.is_empty() && rest.iter().all(u8::is_ascii_digit))
}

/// Target block size (whole lines) handed to each parse worker.
const LOAD_BLOCK: usize = 4 * 1024 * 1024;

/// Load the graph file into a reverse adjacency `target -> [sources]`.
///
/// Each flat line (`{"id":"Q31","P31":["Q…"],"P279":["Q…"]}`) is JSON-parsed by
/// `parse_graph_line` into its source id and edge targets (optionally filtered
/// to the `allowed` properties). For each edge `id --> target` we record
/// `reverse[target].push(id)`. The expensive parse is done by a pool of workers;
/// this thread folds their edge lists into the single map. Returns the adjacency
/// and the total edge count.
fn load_reverse_adjacency(
    path: &str,
    allowed: Option<&HashSet<String>>,
    quiet: bool,
) -> Result<(AHashMap<NodeId, Vec<NodeId>>, usize)> {
    let file = File::open(path).with_context(|| format!("cannot open graph {path}"))?;
    let total = file.metadata().map(|m| m.len()).unwrap_or(0);

    // Byte-progress bar so a multi-GB graph load isn't a silent wait. indicatif's
    // stderr target hides itself when stderr isn't a terminal.
    let progress = (!quiet && std::io::stderr().is_terminal() && total > 0).then(|| {
        let pb = ProgressBar::new(total);
        pb.set_draw_target(ProgressDrawTarget::stderr_with_hz(4));
        pb.set_style(
            ProgressStyle::with_template(
                "loading graph [{elapsed_precise}] {bar:30} {bytes}/{total_bytes} ({bytes_per_sec}, eta {eta})",
            )
            .unwrap(),
        );
        pb
    });

    let workers = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .max(1);

    // Pipeline: one reader slices the file into ~4 MB blocks of whole lines; N
    // workers JSON-parse blocks in parallel (the expensive part) into edge lists;
    // this thread (the builder) folds the edges into the single reverse map.
    // Keeping map construction single-threaded avoids duplicating popular keys
    // across per-worker maps, so peak memory stays at one map plus a few blocks.
    let (block_tx, block_rx) = sync_channel::<Vec<u8>>(workers * 2);
    let block_rx = Arc::new(Mutex::new(block_rx));
    let (edge_tx, edge_rx) = sync_channel::<Vec<(NodeId, NodeId)>>(workers * 2);
    let first_error: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let aborted = AtomicBool::new(false);

    let mut reverse: AHashMap<NodeId, Vec<NodeId>> = AHashMap::new();
    let mut edges = 0usize;

    thread::scope(|scope| {
        let err: &Mutex<Option<anyhow::Error>> = &first_error;
        let abort: &AtomicBool = &aborted;

        // Reader.
        let reader_pb = progress.clone();
        scope.spawn(move || {
            let mut reader = BufReader::with_capacity(1 << 20, file);
            let mut block = Vec::with_capacity(LOAD_BLOCK + 65536);
            loop {
                match reader.read_until(b'\n', &mut block) {
                    Ok(0) => {
                        if !block.is_empty() {
                            let _ = block_tx.send(std::mem::take(&mut block));
                        }
                        break;
                    }
                    Ok(n) => {
                        if let Some(pb) = &reader_pb {
                            pb.inc(n as u64);
                        }
                        if block.len() >= LOAD_BLOCK {
                            if abort.load(Ordering::Relaxed)
                                || block_tx.send(std::mem::take(&mut block)).is_err()
                            {
                                break;
                            }
                            block = Vec::with_capacity(LOAD_BLOCK + 65536);
                        }
                    }
                    Err(e) => {
                        *err.lock().unwrap() =
                            Some(anyhow::Error::new(e).context("graph read error"));
                        break;
                    }
                }
            }
            // `block_tx` is dropped here, signalling the workers that input ended.
        });

        // Parse workers: pull blocks and parse them in parallel. The first parse
        // error aborts the load (a malformed line means a corrupt graph file).
        for _ in 0..workers {
            let block_rx = Arc::clone(&block_rx);
            let edge_tx = edge_tx.clone();
            scope.spawn(move || {
                loop {
                    let block = {
                        let rx = block_rx.lock().unwrap();
                        rx.recv()
                    };
                    let Ok(block) = block else { break };
                    match parse_block(&block, allowed) {
                        Ok(local) => {
                            if edge_tx.send(local).is_err() || abort.load(Ordering::Relaxed) {
                                break;
                            }
                        }
                        Err(e) => {
                            let mut slot = err.lock().unwrap();
                            if slot.is_none() {
                                *slot = Some(e);
                            }
                            abort.store(true, Ordering::Relaxed);
                            break;
                        }
                    }
                }
            });
        }
        drop(edge_tx); // workers hold clones; lets `edge_rx` end once they finish

        // Builder (this thread): the only writer of the reverse map.
        for local in edge_rx {
            for (tgt, src) in local {
                reverse.entry(tgt).or_default().push(src);
                edges += 1;
            }
        }
    });

    if let Some(pb) = &progress {
        pb.finish_and_clear();
    }
    if let Some(e) = first_error.into_inner().unwrap() {
        return Err(e);
    }
    Ok((reverse, edges))
}

/// Parse one block of whole lines into `(target, source)` edges. A malformed
/// line is a hard error (the graph file is `build-graph`'s own output, so a parse
/// failure means corruption); the offending line is included in the message.
fn parse_block(block: &[u8], allowed: Option<&HashSet<String>>) -> Result<Vec<(NodeId, NodeId)>> {
    let mut edges = Vec::new();
    for line in block.split(|&b| b == b'\n') {
        let parsed = parse_graph_line(line, allowed).map_err(|e| {
            let snippet = String::from_utf8_lossy(&line[..line.len().min(80)]).into_owned();
            e.context(format!("malformed graph line: {snippet}"))
        })?;
        if let Some((src, targets)) = parsed {
            for tgt in targets {
                edges.push((tgt, src.clone()));
            }
        }
    }
    Ok(edges)
}

/// Parse one graph line `{"id":"Q…","P…":["Q…",…],…}` into its source node and
/// the edge target nodes, following only the `allowed` property keys when given.
/// The source and targets are parsed by `NodeId::parse`, so any entity kind is a
/// node and the graph is followed as defined. Returns `None` for a blank line or
/// one without an entity id, and an error on invalid JSON (the graph file is
/// `build-graph`'s own output, so a parse failure means a corrupt file, not
/// something to silently skip).
fn parse_graph_line(
    line: &[u8],
    allowed: Option<&HashSet<String>>,
) -> Result<Option<(NodeId, Vec<NodeId>)>> {
    let trimmed = trim_ascii(line);
    if trimmed.is_empty() {
        return Ok(None);
    }
    let node: sonic_rs::Value = sonic_rs::from_slice(trimmed).context("invalid JSON")?;
    let Some(src) = node["id"].as_str().map(NodeId::parse) else {
        return Ok(None);
    };
    let Some(obj) = node.as_object() else {
        return Ok(None);
    };
    let mut targets = Vec::new();
    for (key, val) in obj.iter() {
        if key == "id" {
            continue;
        }
        let follow = match allowed {
            None => true,
            Some(set) => set.contains(key),
        };
        if !follow {
            continue;
        }
        if let Some(arr) = val.as_array() {
            for t in arr.iter() {
                if let Some(s) = t.as_str() {
                    targets.push(NodeId::parse(s));
                }
            }
        }
    }
    Ok(Some((src, targets)))
}

/// BFS the reverse adjacency from every target, returning all ids that can
/// reach some target (the targets themselves included). Cycles are handled by
/// the `seen` set.
fn reachable(reverse: &AHashMap<NodeId, Vec<NodeId>>, targets: &[NodeId]) -> AHashSet<NodeId> {
    let mut seen: AHashSet<NodeId> = targets.iter().cloned().collect();
    let mut stack: Vec<NodeId> = targets.to_vec();
    while let Some(node) = stack.pop() {
        if let Some(preds) = reverse.get(&node) {
            for p in preds {
                if seen.insert(p.clone()) {
                    stack.push(p.clone());
                }
            }
        }
    }
    seen
}

/// The node of an input line: the value of the first top-level `"id":"…"`
/// (Wikibase lines start `{"type":…,"id":"Q…"`), parsed by `NodeId::parse` so it
/// matches the nodes in the reachability sets. `None` for lines without an
/// entity id (array wrappers, blanks).
pub fn entity_id(line: &[u8]) -> Option<NodeId> {
    let pos = memchr::memmem::find(line, b"\"id\":\"")?;
    let rest = &line[pos + b"\"id\":\"".len()..];
    let end = memchr::memchr(b'"', rest)?;
    let id = std::str::from_utf8(&rest[..end]).ok()?;
    Some(NodeId::parse(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_reverse(graph: &[u8]) -> AHashMap<NodeId, Vec<NodeId>> {
        let mut reverse: AHashMap<NodeId, Vec<NodeId>> = AHashMap::new();
        for line in graph.split(|&b| b == b'\n') {
            if let Some((src, targets)) = parse_graph_line(line, None).unwrap() {
                for t in targets {
                    reverse.entry(t).or_default().push(src.clone());
                }
            }
        }
        reverse
    }

    fn set(v: Vec<NodeId>) -> AHashSet<NodeId> {
        v.into_iter().collect()
    }

    fn props(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_source_and_targets() {
        let (src, targets) = parse_graph_line(
            br#"{"id":"Q31","P31":["Q3624078","Q6256"],"P279":["Q7275"]}"#,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(src, NodeId::Q(31));
        assert_eq!(
            set(targets),
            set(vec![NodeId::Q(3624078), NodeId::Q(6256), NodeId::Q(7275)])
        );
    }

    #[test]
    fn parse_keeps_only_allowed_properties() {
        let line = br#"{"id":"Q31","P31":["Q3624078","Q6256"],"P279":["Q7275"],"P137":["Q42"]}"#;
        // Only follow P279.
        let (src, targets) = parse_graph_line(line, Some(&props(&["P279"])))
            .unwrap()
            .unwrap();
        assert_eq!(src, NodeId::Q(31));
        assert_eq!(set(targets), set(vec![NodeId::Q(7275)]));

        // Follow P31 + P137, skipping P279.
        let (src, targets) = parse_graph_line(line, Some(&props(&["P31", "P137"])))
            .unwrap()
            .unwrap();
        assert_eq!(src, NodeId::Q(31));
        assert_eq!(
            set(targets),
            set(vec![NodeId::Q(3624078), NodeId::Q(6256), NodeId::Q(42)])
        );
    }

    #[test]
    fn parse_errors_on_invalid_json() {
        // A corrupt graph line is a hard error, not silently mis-scanned.
        assert!(parse_graph_line(br#"{"id":"Q1","P31":[broken}"#, None).is_err());
        // Blank lines are skipped, not errors.
        assert!(parse_graph_line(b"   \n", None).unwrap().is_none());
    }

    #[test]
    fn parse_follows_property_entities() {
        // A property entity ("P19") is a P-node, so its edges are followed like
        // any other — the graph is traversed as defined, not limited to items.
        // The P1647 edge points at another property (P21): a P->P edge.
        let (src, targets) =
            parse_graph_line(br#"{"id":"P19","P31":["Q18608756"],"P1647":["P21"]}"#, None)
                .unwrap()
                .unwrap();
        assert_eq!(src, NodeId::P(19));
        assert_eq!(set(targets), set(vec![NodeId::Q(18608756), NodeId::P(21)]));
    }

    #[test]
    fn distinct_kinds_never_collide() {
        // Q100, P100 and L100 share the number 100 but are distinct nodes.
        assert_ne!(NodeId::parse("Q100"), NodeId::parse("P100"));
        assert_ne!(NodeId::parse("Q100"), NodeId::parse("L100"));
        assert_eq!(NodeId::parse("Q100"), NodeId::Q(100));

        // Ids that aren't a Q/P/L number are kept verbatim, never dropped or
        // coerced: other entity kinds, lexeme forms, out-of-u32 numbers.
        assert_eq!(NodeId::parse("M100"), NodeId::Other("M100".into()));
        assert_eq!(NodeId::parse("L1-F2"), NodeId::Other("L1-F2".into()));
        assert_eq!(
            NodeId::parse("Q99999999999"),
            NodeId::Other("Q99999999999".into())
        );

        // The allow-set matches property keys, never targets, so the target Q100
        // (number 100) is followed only when its property P100 is allowed.
        let line = br#"{"id":"Q1","P100":["Q100"],"P31":["Q5"]}"#;
        let (src, targets) = parse_graph_line(line, Some(&props(&["P100"])))
            .unwrap()
            .unwrap();
        assert_eq!(src, NodeId::Q(1));
        assert_eq!(set(targets), set(vec![NodeId::Q(100)]));
        let (_, targets) = parse_graph_line(line, Some(&props(&["P31"])))
            .unwrap()
            .unwrap();
        assert_eq!(set(targets), set(vec![NodeId::Q(5)]));
    }

    #[test]
    fn entity_id_takes_first_id() {
        assert_eq!(
            entity_id(br#"{"type":"item","id":"Q1192377","claims":{}}"#),
            Some(NodeId::Q(1192377))
        );
        assert_eq!(entity_id(br#"["#), None);
    }

    #[test]
    fn reaches_through_edges() {
        // Q1192377 --(P31)--> Q842402 --(P279)--> Q27096235
        let reverse = build_reverse(
            b"{\"id\":\"Q1192377\",\"P31\":[\"Q842402\"]}\n\
              {\"id\":\"Q842402\",\"P279\":[\"Q27096235\"]}\n",
        );
        let inc = reachable(&reverse, &[NodeId::Q(27096235)]);
        assert!(
            inc.contains(&NodeId::Q(1192377)),
            "instance reaches the abstract class"
        );
        assert!(inc.contains(&NodeId::Q(842402)));
        assert!(
            !inc.contains(&NodeId::Q(999)),
            "unrelated id is not reachable"
        );
    }

    #[test]
    fn allows_applies_include_and_exclude() {
        let reverse = build_reverse(
            b"{\"id\":\"Q1\",\"P31\":[\"Q100\"]}\n\
              {\"id\":\"Q2\",\"P31\":[\"Q200\"]}\n",
        );
        let reach = GraphReach {
            include: Some(reachable(&reverse, &[NodeId::Q(100)])),
            exclude: reachable(&reverse, &[NodeId::Q(200)]),
            edges: 2,
            edge_properties: None,
        };
        assert!(
            reach.allows(Some(NodeId::Q(1))),
            "reaches include, not exclude"
        );
        assert!(
            !reach.allows(Some(NodeId::Q(2))),
            "reaches exclude (precedence)"
        );
        assert!(!reach.allows(Some(NodeId::Q(999))), "outside include");
        assert!(!reach.allows(None), "no id fails active include");
    }
}
