//! Dependency graph for AimDB record topology
//!
//! This module provides the `DependencyGraph` and related types that represent
//! the entire database topology — sources, links, transforms, taps, and their
//! relationships.
//!
//! The graph is constructed once during `build()` and is immutable thereafter.
//! It enables:
//! - Build-time validation (cycle detection, missing inputs)
//! - Spawn ordering (topological sort)
//! - Runtime introspection (AimX protocol, MCP tools, CLI)

extern crate alloc;
use alloc::{
    string::{String, ToString},
    vec::Vec,
};

// ============================================================================
// Record Origin
// ============================================================================

/// How a record gets its values — part of the dependency graph.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "snake_case"))]
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
#[cfg_attr(feature = "std", derive(serde::Serialize))]
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
#[cfg_attr(feature = "std", derive(serde::Serialize))]
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
#[cfg_attr(feature = "std", derive(serde::Serialize))]
#[cfg_attr(feature = "std", serde(rename_all = "snake_case"))]
pub enum EdgeType {
    Source,
    Link { protocol: String },
    Transform,
    TransformJoin,
    Tap { index: usize },
    LinkOut { protocol: String },
}

// ============================================================================
// Record Graph Info (builder input)
// ============================================================================

/// Information needed to build a GraphNode for one record.
///
/// Collected from each record during graph construction.
#[derive(Clone, Debug)]
pub struct RecordGraphInfo {
    /// Record key
    pub key: String,
    /// How this record gets its values
    pub origin: RecordOrigin,
    /// Buffer type name
    pub buffer_type: String,
    /// Buffer capacity (if applicable)
    pub buffer_capacity: Option<usize>,
    /// Number of taps attached
    pub tap_count: usize,
    /// Whether an outbound link is configured
    pub has_outbound_link: bool,
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

impl DependencyGraph {
    /// Construct and validate the dependency graph from registered records.
    ///
    /// This builds a complete `DependencyGraph` with nodes, edges, and topological order.
    /// The graph is immutable after construction and can be used for introspection.
    ///
    /// # Arguments
    /// * `record_infos` - Information about each registered record (origin, buffer, etc.)
    ///
    /// # Returns
    /// * `Ok(DependencyGraph)` - The complete, validated graph
    /// * `Err(DbError::TransformInputNotFound)` - If a transform references a missing record
    /// * `Err(DbError::CyclicDependency)` - If the transform edges form a cycle
    pub fn build_and_validate(record_infos: &[RecordGraphInfo]) -> crate::DbResult<Self> {
        use alloc::collections::VecDeque;

        // Build set of all keys for validation
        let all_keys: hashbrown::HashSet<&str> =
            record_infos.iter().map(|info| info.key.as_str()).collect();

        // Extract transform inputs for validation and edge construction
        let mut transform_inputs: Vec<(&str, Vec<&str>)> = Vec::new();
        for info in record_infos {
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
        #[allow(unused_variables)] // output_key, input_key used only in std error messages
        for (output_key, input_keys) in &transform_inputs {
            for input_key in input_keys {
                if !all_keys.contains(input_key) {
                    #[cfg(feature = "std")]
                    return Err(crate::DbError::TransformInputNotFound {
                        output_key: output_key.to_string(),
                        input_key: input_key.to_string(),
                    });
                    #[cfg(not(feature = "std"))]
                    return Err(crate::DbError::TransformInputNotFound {
                        _output_key: (),
                        _input_key: (),
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

        // Kahn's algorithm — topological sort
        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&node, _)| node)
            .collect();
        let mut topo_order = Vec::new();

        while let Some(node) = queue.pop_front() {
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
            #[cfg(feature = "std")]
            {
                // Find the cycle participants for a helpful error message
                let cycle_records: Vec<String> = in_degree
                    .iter()
                    .filter(|(_, &deg)| deg > 0)
                    .map(|(&k, _)| k.to_string())
                    .collect();

                return Err(crate::DbError::CyclicDependency {
                    records: cycle_records,
                });
            }
            #[cfg(not(feature = "std"))]
            return Err(crate::DbError::CyclicDependency { _records: () });
        }

        // Build nodes from record infos
        let nodes: Vec<GraphNode> = record_infos
            .iter()
            .map(|info| GraphNode {
                key: info.key.clone(),
                origin: info.origin.clone(),
                buffer_type: info.buffer_type.clone(),
                buffer_capacity: info.buffer_capacity,
                tap_count: info.tap_count,
                has_outbound_link: info.has_outbound_link,
            })
            .collect();

        // Build edges from origins
        let mut edges: Vec<GraphEdge> = Vec::new();
        for info in record_infos {
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
