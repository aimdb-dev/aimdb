//! Dependency graph for AimDB record topology
//!
//! This module provides the `DependencyGraph` and related types that represent
//! the entire database topology — sources, links, transforms, taps, and their
//! relationships.
//!
//! The graph is constructed once during `build()` and is immutable thereafter.
//! It enables:
//! - Build-time validation (cycle detection, missing inputs; input types are
//!   checked by `AimDbBuilder::build()` against the record registry)
//! - Spawn ordering (topological sort)
//! - Runtime introspection (AimX protocol, MCP tools, CLI)

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

// ============================================================================
// Record Origin
// ============================================================================

/// How a record gets its values — part of the dependency graph.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum RecordOrigin {
    /// Autonomous producer via `.source()`
    Source,
    /// Inbound connector via `.link_from()`
    Link { protocol: String, address: String },
    /// Single-input reactive derivation via `.transform()`
    Transform { input: String },
    /// Multi-input reactive join via `.transform_join()`
    TransformJoin { inputs: Vec<String> },
    /// No registered producer (writable via `record.set` / `db.produce()`)
    Passive,
}

// ============================================================================
// Graph Node
// ============================================================================

/// Metadata for one node in the dependency graph.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct GraphNode {
    /// Record key (e.g. "temp.vienna")
    pub key: String,
    /// How this record gets its values
    pub origin: RecordOrigin,
    /// Buffer type ("spmc_ring", "single_latest", "mailbox", "none")
    pub buffer_type: String,
    /// Buffer capacity (None for unbounded or non-ring buffers)
    pub buffer_capacity: Option<usize>,
    /// Number of taps attached
    pub tap_count: usize,
    /// Whether an outbound link is configured
    pub has_outbound_link: bool,
}

// ============================================================================
// Graph Edge
// ============================================================================

/// One directed edge in the dependency graph.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct GraphEdge {
    /// Source record key (None for external origins like source/link)
    pub from: Option<String>,
    /// Target record key (None for side-effects like taps/link_out)
    pub to: Option<String>,
    /// Classification of this edge
    pub edge_type: EdgeType,
}

/// Classification of a dependency graph edge.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum EdgeType {
    Source,
    Link { protocol: String },
    Transform,
    TransformJoin,
    Tap { index: usize },
    LinkOut { protocol: String },
}

// ============================================================================
// Dependency Graph
// ============================================================================

/// The dependency graph, constructed once during `build()` and immutable thereafter.
#[derive(Clone, Debug)]
pub struct DependencyGraph {
    /// All nodes indexed by record key.
    pub nodes: Vec<GraphNode>,
    /// All edges (both internal and external).
    pub edges: Vec<GraphEdge>,
    /// Topological order of record keys (transforms come after their inputs).
    pub topo_order: Vec<String>,
}

/// SplitMix64: a few lines of seeded PRNG for
/// [`DependencyGraph::build_and_validate_seeded`]. Not for anything that needs
/// real randomness.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// An index in `0..len` (`len > 0`).
    fn below(&mut self, len: usize) -> usize {
        ((u128::from(self.next()) * len as u128) >> 64) as usize
    }
}

impl DependencyGraph {
    /// Construct and validate the dependency graph from registered records.
    ///
    /// This builds a complete `DependencyGraph` with nodes, edges, and topological order.
    /// The graph is immutable after construction and can be used for introspection.
    ///
    /// # Arguments
    /// * `nodes` - One [`GraphNode`] per registered record (origin, buffer, etc.)
    ///
    /// # Returns
    /// * `Ok(DependencyGraph)` - The complete, validated graph
    /// * `Err(DbError::TransformInputNotFound)` - If a transform references a missing record
    /// * `Err(DbError::CyclicDependency)` - If the transform edges form a cycle
    pub fn build_and_validate(nodes: Vec<GraphNode>) -> crate::DbResult<Self> {
        Self::build_and_validate_seeded(nodes, None)
    }

    /// [`build_and_validate`](Self::build_and_validate), with a seed that
    /// permutes the start order.
    ///
    /// Without a seed, records ready at once are taken in registration order.
    /// With one, the next record is drawn from all ready records by a seeded
    /// PRNG, so different seeds explore different valid orders and the same
    /// seed replays one. Either way every record comes after its inputs.
    pub fn build_and_validate_seeded(
        nodes: Vec<GraphNode>,
        seed: Option<u64>,
    ) -> crate::DbResult<Self> {
        use alloc::collections::VecDeque;

        // Everything in this block borrows `nodes` (&str keys); scoping it
        // lets `nodes` move into the returned graph afterwards.
        let (topo_order, edges) = {
            // Build set of all keys for validation
            let all_keys: hashbrown::HashSet<&str> =
                nodes.iter().map(|info| info.key.as_str()).collect();

            // Extract transform inputs for validation and edge construction
            let mut transform_inputs: Vec<(&str, Vec<&str>)> = Vec::new();
            for info in &nodes {
                match &info.origin {
                    RecordOrigin::Transform { input } => {
                        transform_inputs.push((info.key.as_str(), alloc::vec![input.as_str()]));
                    }
                    RecordOrigin::TransformJoin { inputs } => {
                        let input_refs: Vec<&str> = inputs.iter().map(|s| s.as_str()).collect();
                        transform_inputs.push((info.key.as_str(), input_refs));
                    }
                    _ => {}
                }
            }

            // Validate: check all transform input keys exist
            for (output_key, input_keys) in &transform_inputs {
                for input_key in input_keys {
                    if !all_keys.contains(input_key) {
                        return Err(crate::DbError::TransformInputNotFound {
                            output_key: output_key.to_string(),
                            input_key: input_key.to_string(),
                        });
                    }
                }
            }

            // Build adjacency list and in-degree map for Kahn's algorithm
            let mut in_degree: hashbrown::HashMap<&str, usize> = hashbrown::HashMap::new();
            let mut adjacency: hashbrown::HashMap<&str, Vec<&str>> = hashbrown::HashMap::new();

            // Initialize all keys with in-degree 0
            for key in &all_keys {
                in_degree.entry(*key).or_insert(0);
                adjacency.entry(*key).or_default();
            }

            // Add transform edges: input → output
            for (output_key, input_keys) in &transform_inputs {
                for input_key in input_keys {
                    adjacency.entry(*input_key).or_default().push(*output_key);
                    *in_degree.entry(*output_key).or_insert(0) += 1;
                }
            }

            // Kahn's algorithm — topological sort. The queue is seeded in
            // registration order (`nodes`), not map order: the map's hasher is
            // randomly seeded, and this order is the record task start order,
            // so it must not change from build to build (#324). Neighbours
            // below follow `transform_inputs`, which is registration order too.
            let mut queue: VecDeque<&str> = nodes
                .iter()
                .map(|info| info.key.as_str())
                .filter(|key| in_degree.get(key) == Some(&0))
                .collect();
            let mut topo_order = Vec::new();
            let mut rng = seed.map(SplitMix64);

            loop {
                let next = match rng.as_mut() {
                    Some(rng) if !queue.is_empty() => {
                        queue.swap_remove_back(rng.below(queue.len()))
                    }
                    _ => queue.pop_front(),
                };
                let Some(node) = next else { break };
                topo_order.push(node.to_string());
                if let Some(neighbors) = adjacency.get(node) {
                    for &neighbor in neighbors {
                        if let Some(deg) = in_degree.get_mut(neighbor) {
                            *deg -= 1;
                            if *deg == 0 {
                                queue.push_back(neighbor);
                            }
                        }
                    }
                }
            }

            if topo_order.len() != all_keys.len() {
                // Find the cycle participants for a helpful error message
                let cycle_records: Vec<String> = nodes
                    .iter()
                    .map(|info| info.key.as_str())
                    .filter(|key| in_degree.get(key).is_some_and(|&deg| deg > 0))
                    .map(|k| k.to_string())
                    .collect();

                return Err(crate::DbError::CyclicDependency {
                    records: cycle_records,
                });
            }

            // Build edges from origins
            let mut edges: Vec<GraphEdge> = Vec::new();
            for info in &nodes {
                match &info.origin {
                    RecordOrigin::Source => {
                        edges.push(GraphEdge {
                            from: None,
                            to: Some(info.key.clone()),
                            edge_type: EdgeType::Source,
                        });
                    }
                    RecordOrigin::Link { protocol, .. } => {
                        edges.push(GraphEdge {
                            from: None,
                            to: Some(info.key.clone()),
                            edge_type: EdgeType::Link {
                                protocol: protocol.clone(),
                            },
                        });
                    }
                    RecordOrigin::Transform { input } => {
                        edges.push(GraphEdge {
                            from: Some(input.clone()),
                            to: Some(info.key.clone()),
                            edge_type: EdgeType::Transform,
                        });
                    }
                    RecordOrigin::TransformJoin { inputs } => {
                        for input in inputs {
                            edges.push(GraphEdge {
                                from: Some(input.clone()),
                                to: Some(info.key.clone()),
                                edge_type: EdgeType::TransformJoin,
                            });
                        }
                    }
                    RecordOrigin::Passive => {
                        // No inbound edge for passive records
                    }
                }

                // Add outbound link edges if present
                if info.has_outbound_link {
                    edges.push(GraphEdge {
                        from: Some(info.key.clone()),
                        to: None,
                        edge_type: EdgeType::LinkOut {
                            protocol: "unknown".to_string(), // Protocol info not available here
                        },
                    });
                }

                // Add tap edges
                for tap_idx in 0..info.tap_count {
                    edges.push(GraphEdge {
                        from: Some(info.key.clone()),
                        to: None,
                        edge_type: EdgeType::Tap { index: tap_idx },
                    });
                }
            }

            (topo_order, edges)
        };

        Ok(DependencyGraph {
            nodes,
            edges,
            topo_order,
        })
    }

    /// Get a node by key
    pub fn node(&self, key: &str) -> Option<&GraphNode> {
        self.nodes.iter().find(|n| n.key == key)
    }

    /// Get all nodes
    pub fn nodes(&self) -> &[GraphNode] {
        &self.nodes
    }

    /// Get all edges
    pub fn edges(&self) -> &[GraphEdge] {
        &self.edges
    }

    /// Get the topological order
    pub fn topo_order(&self) -> &[String] {
        &self.topo_order
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn node(key: &str, origin: RecordOrigin) -> GraphNode {
        GraphNode {
            key: key.to_string(),
            origin,
            buffer_type: "single_latest".to_string(),
            buffer_capacity: None,
            tap_count: 0,
            has_outbound_link: false,
        }
    }

    /// Six sources and a passive record, one join, and three transforms, two
    /// of which share an input.
    fn sample_nodes() -> Vec<GraphNode> {
        vec![
            node(
                "sum",
                RecordOrigin::TransformJoin {
                    inputs: vec!["zeta".to_string(), "alpha".to_string()],
                },
            ),
            node("zeta", RecordOrigin::Source),
            node("mid", RecordOrigin::Passive),
            node("alpha", RecordOrigin::Source),
            node(
                "double",
                RecordOrigin::Transform {
                    input: "zeta".to_string(),
                },
            ),
            node("omega", RecordOrigin::Source),
            node("beta", RecordOrigin::Source),
            node("kappa", RecordOrigin::Source),
            // Two transforms on one input, released together when `beta`
            // is taken: they follow registration order, not key order.
            node(
                "y_half",
                RecordOrigin::Transform {
                    input: "beta".to_string(),
                },
            ),
            node(
                "b_triple",
                RecordOrigin::Transform {
                    input: "beta".to_string(),
                },
            ),
        ]
    }

    /// #324: records ready at once start in registration order, and records
    /// that depend on others still come after their inputs, every build.
    #[test]
    fn topo_order_is_registration_order_among_ready_records() {
        // `double` is ready once `zeta` is; `sum` waits for `alpha` as well.
        let expected = [
            "zeta", "mid", "alpha", "omega", "beta", "kappa", "double", "sum", "y_half", "b_triple",
        ];

        // Each build seeds a fresh map hasher; ask enough times that a
        // map-ordered queue would show.
        for _ in 0..20 {
            let graph = DependencyGraph::build_and_validate(sample_nodes()).unwrap();
            assert_eq!(graph.topo_order(), expected);
        }
    }

    fn seeded(seed: u64) -> Vec<String> {
        DependencyGraph::build_and_validate_seeded(sample_nodes(), Some(seed))
            .unwrap()
            .topo_order
    }

    /// A seed replays its order, which still has every record after its
    /// inputs and every record exactly once.
    #[test]
    fn a_seed_replays_a_valid_order() {
        let pos = |order: &[String], key: &str| order.iter().position(|k| k == key).unwrap();
        for seed in 0..64 {
            let order = seeded(seed);
            assert_eq!(order, seeded(seed), "seed {seed} must replay");
            assert_eq!(order.len(), sample_nodes().len());
            for (record, inputs) in [
                ("sum", &["zeta", "alpha"][..]),
                ("double", &["zeta"]),
                ("y_half", &["beta"]),
                ("b_triple", &["beta"]),
            ] {
                for input in inputs {
                    assert!(
                        pos(&order, input) < pos(&order, record),
                        "seed {seed}: {record} before its input {input}: {order:?}"
                    );
                }
            }
        }
    }

    /// Different seeds explore different orders, including among records
    /// released together (`y_half`, `b_triple`), not just the initial queue.
    #[test]
    fn seeds_explore_different_orders() {
        let unseeded = DependencyGraph::build_and_validate(sample_nodes())
            .unwrap()
            .topo_order;
        let orders: Vec<Vec<String>> = (0..64).map(seeded).collect();
        let distinct: hashbrown::HashSet<&Vec<String>> = orders.iter().collect();
        assert!(
            distinct.len() > 32,
            "only {} distinct orders",
            distinct.len()
        );
        assert!(orders.iter().any(|o| *o != unseeded));
        let first = |o: &Vec<String>| {
            o.iter()
                .find(|k| *k == "y_half" || *k == "b_triple")
                .cloned()
        };
        let firsts: hashbrown::HashSet<_> = orders.iter().map(first).collect();
        assert_eq!(
            firsts.len(),
            2,
            "both transforms of `beta` must lead sometimes"
        );
    }
}
