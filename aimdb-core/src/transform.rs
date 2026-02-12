//! Reactive transform primitives for derived records
//!
//! This module provides the `.transform()` and `.transform_join()` API for declaring
//! reactive derivations from one or more input records to an output record.
//!
//! # Transform Archetypes
//!
//! - **Map** (1:1, stateless): Transform each input value to zero-or-one output value
//! - **Accumulate** (N:1, stateful): Aggregate a stream of values with persistent state
//! - **Join** (M×N:1, stateful, multi-input): Combine values from multiple input records
//!
//! All three are handled by a unified API surface:
//! - Single-input: `.transform()` with `TransformBuilder`
//! - Multi-input: `.transform_join()` with `JoinBuilder`
//!
//! # Design Principles
//!
//! - Transforms are **owned by AimDB** — visible in the dependency graph
//! - Transforms are **mutually exclusive** with `.source()` on the same record
//! - Multiple `.tap()` observers can still be attached to a transform's output
//! - Input subscriptions use existing `Consumer<T, R>` / `BufferReader<T>` API
//! - Build-time validation catches missing input keys and cyclic dependencies

use core::any::Any;
use core::fmt::Debug;
use core::marker::PhantomData;

extern crate alloc;
use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};

#[cfg(feature = "std")]
use alloc::sync::Arc;

#[cfg(not(feature = "std"))]
use alloc::sync::Arc;

use crate::typed_record::BoxFuture;

// ============================================================================
// TransformDescriptor — stored per output record in TypedRecord
// ============================================================================

/// Transform descriptor stored in `TypedRecord`.
///
/// Contains the input record keys and a type-erased spawn function that captures
/// all type information (input types, state type) in its closure. At spawn time
/// it receives a `Producer<T, R>` and the `AimDb<R>` handle.
///
/// This follows the same pattern as `ProducerServiceFn<T, R>`.
pub(crate) struct TransformDescriptor<T, R: aimdb_executor::Spawn + 'static>
where
    T: Send + 'static + Debug + Clone,
{
    /// Record keys this transform subscribes to (for build-time validation).
    pub input_keys: Vec<String>,

    /// Spawn function: takes (Producer<T, R>, Arc<AimDb<R>>, Arc<dyn Any + Send + Sync>) → Future.
    ///
    /// The closure captures input types, state, and user logic. At spawn time it
    /// receives:
    /// - `Producer<T, R>` bound to the output record
    /// - `Arc<AimDb<R>>` for subscribing to input records
    /// - `Arc<dyn Any + Send + Sync>` runtime context (same as source/tap)
    #[allow(clippy::type_complexity)]
    pub spawn_fn: Box<
        dyn FnOnce(
                crate::Producer<T, R>,
                Arc<crate::AimDb<R>>,
                Arc<dyn Any + Send + Sync>,
            ) -> BoxFuture<'static, ()>
            + Send
            + Sync,
    >,
}

// ============================================================================
// Single-Input Transform: TransformBuilder → TransformPipeline
// ============================================================================

/// Configures a single-input transform pipeline.
///
/// Created by `RecordRegistrar::transform_raw()`. Use `.map()` for stateless
/// transforms or `.with_state()` for stateful transforms.
pub struct TransformBuilder<I, O, R: aimdb_executor::Spawn + 'static> {
    input_key: String,
    _phantom: PhantomData<(I, O, R)>,
}

impl<I, O, R> TransformBuilder<I, O, R>
where
    I: Send + Sync + Clone + Debug + 'static,
    O: Send + Sync + Clone + Debug + 'static,
    R: aimdb_executor::Spawn + 'static,
{
    pub(crate) fn new(input_key: String) -> Self {
        Self {
            input_key,
            _phantom: PhantomData,
        }
    }

    /// Stateless 1:1 map. Returning `None` skips output for this input value.
    pub fn map<F>(self, f: F) -> TransformPipeline<I, O, R>
    where
        F: Fn(&I) -> Option<O> + Send + Sync + 'static,
    {
        // A stateless map is a stateful transform with () state
        TransformPipeline {
            input_key: self.input_key,
            spawn_factory: Box::new(move |input_key| {
                let transform_fn = move |val: &I, _state: &mut ()| f(val);
                create_single_transform_descriptor::<I, O, (), R>(input_key, (), transform_fn)
            }),
            _phantom_i: PhantomData,
        }
    }

    /// Begin configuring a stateful transform. `S` is the user-defined state type.
    pub fn with_state<S: Send + Sync + 'static>(
        self,
        initial: S,
    ) -> StatefulTransformBuilder<I, O, S, R> {
        StatefulTransformBuilder {
            input_key: self.input_key,
            initial_state: initial,
            _phantom: PhantomData,
        }
    }
}

/// Intermediate builder for stateful single-input transforms.
pub struct StatefulTransformBuilder<I, O, S, R: aimdb_executor::Spawn + 'static> {
    input_key: String,
    initial_state: S,
    _phantom: PhantomData<(I, O, R)>,
}

impl<I, O, S, R> StatefulTransformBuilder<I, O, S, R>
where
    I: Send + Sync + Clone + Debug + 'static,
    O: Send + Sync + Clone + Debug + 'static,
    S: Send + Sync + 'static,
    R: aimdb_executor::Spawn + 'static,
{
    /// Called for each input value. Receives mutable state, returns optional output.
    pub fn on_value<F>(self, f: F) -> TransformPipeline<I, O, R>
    where
        F: Fn(&I, &mut S) -> Option<O> + Send + Sync + 'static,
    {
        let initial = self.initial_state;
        TransformPipeline {
            input_key: self.input_key,
            spawn_factory: Box::new(move |input_key| {
                create_single_transform_descriptor::<I, O, S, R>(input_key, initial, f)
            }),
            _phantom_i: PhantomData,
        }
    }
}

/// Completed single-input transform pipeline, ready to be stored in `TypedRecord`.
pub struct TransformPipeline<
    I,
    O: Send + Sync + Clone + Debug + 'static,
    R: aimdb_executor::Spawn + 'static,
> {
    pub(crate) input_key: String,
    /// Factory that produces a TransformDescriptor given the input key.
    /// This indirection lets the pipeline be constructed before we have the runtime.
    pub(crate) spawn_factory: Box<dyn FnOnce(String) -> TransformDescriptor<O, R> + Send + Sync>,
    _phantom_i: PhantomData<I>,
}

impl<I, O, R> TransformPipeline<I, O, R>
where
    I: Send + Sync + Clone + Debug + 'static,
    O: Send + Sync + Clone + Debug + 'static,
    R: aimdb_executor::Spawn + 'static,
{
    /// Consume this pipeline and produce the `TransformDescriptor` for storage.
    pub(crate) fn into_descriptor(self) -> TransformDescriptor<O, R> {
        (self.spawn_factory)(self.input_key)
    }
}

/// Helper: create a single-input TransformDescriptor from types and closure.
fn create_single_transform_descriptor<I, O, S, R>(
    input_key: String,
    initial_state: S,
    transform_fn: impl Fn(&I, &mut S) -> Option<O> + Send + Sync + 'static,
) -> TransformDescriptor<O, R>
where
    I: Send + Sync + Clone + Debug + 'static,
    O: Send + Sync + Clone + Debug + 'static,
    S: Send + Sync + 'static,
    R: aimdb_executor::Spawn + 'static,
{
    let input_key_clone = input_key.clone();
    let input_keys = alloc::vec![input_key];

    TransformDescriptor {
        input_keys,
        spawn_fn: Box::new(move |producer, db, _ctx| {
            Box::pin(run_single_transform::<I, O, S, R>(
                db,
                input_key_clone,
                producer,
                initial_state,
                transform_fn,
            ))
        }),
    }
}

// ============================================================================
// Multi-Input Join: JoinBuilder → JoinPipeline
// ============================================================================

/// Tells the join handler which input produced a value.
///
/// Users match on the index (corresponding to `.input()` call order)
/// and downcast to recover the typed value.
pub enum JoinTrigger {
    /// An input at the given index fired with a type-erased value.
    Input {
        index: usize,
        value: Box<dyn Any + Send>,
    },
}

impl JoinTrigger {
    /// Convenience: try to downcast the trigger value as the expected input type.
    pub fn as_input<T: 'static>(&self) -> Option<&T> {
        match self {
            JoinTrigger::Input { value, .. } => value.downcast_ref::<T>(),
        }
    }

    /// Returns the input index that triggered this event.
    pub fn index(&self) -> usize {
        match self {
            JoinTrigger::Input { index, .. } => *index,
        }
    }
}

/// Type-erased input descriptor for joins.
///
/// Each input captures a subscribe-and-forward function that, given the AimDb handle,
/// subscribes to the input buffer and forwards typed values as `JoinTrigger` into a
/// shared `mpsc::UnboundedSender`.
///
/// Configures a multi-input join transform.
///
/// Created by `RecordRegistrar::transform_join_raw()`. Add inputs with `.input()`,
/// then set state and handler with `.with_state().on_trigger()`.
#[cfg(feature = "std")]
pub struct JoinBuilder<O, R: aimdb_executor::Spawn + 'static> {
    inputs: Vec<(String, JoinInputFactory<R>)>,
    _phantom: PhantomData<(O, R)>,
}

/// Type-erased factory for creating a forwarder task for one join input.
#[cfg(feature = "std")]
type JoinInputFactory<R> = Box<
    dyn FnOnce(
            Arc<crate::AimDb<R>>,
            usize,
            tokio::sync::mpsc::UnboundedSender<JoinTrigger>,
        ) -> BoxFuture<'static, ()>
        + Send
        + Sync,
>;

#[cfg(feature = "std")]
impl<O, R> JoinBuilder<O, R>
where
    O: Send + Sync + Clone + Debug + 'static,
    R: aimdb_executor::Spawn + 'static,
{
    pub(crate) fn new() -> Self {
        Self {
            inputs: Vec::new(),
            _phantom: PhantomData,
        }
    }

    /// Add a typed input to the join.
    ///
    /// The input index corresponds to the order of `.input()` calls,
    /// starting from 0.
    pub fn input<I>(mut self, key: impl crate::RecordKey) -> Self
    where
        I: Send + Sync + Clone + Debug + 'static,
    {
        let key_str = key.as_str().to_string();
        let key_for_factory = key_str.clone();

        let factory: JoinInputFactory<R> = Box::new(
            move |db: Arc<crate::AimDb<R>>,
                  index: usize,
                  tx: tokio::sync::mpsc::UnboundedSender<JoinTrigger>| {
                Box::pin(async move {
                    // Create consumer and subscribe to the input buffer
                    let consumer =
                        crate::typed_api::Consumer::<I, R>::new(db, key_for_factory.clone());
                    let mut reader = match consumer.subscribe() {
                        Ok(r) => r,
                        Err(e) => {
                            #[cfg(feature = "tracing")]
                            tracing::error!(
                                "🔄 Join input '{}' (index {}) subscription failed: {:?}",
                                key_for_factory,
                                index,
                                e
                            );
                            // Defense-in-depth: always emit something on subscription failure
                            #[cfg(all(feature = "std", not(feature = "tracing")))]
                            eprintln!(
                                "AIMDB TRANSFORM ERROR: Join input '{}' (index {}) subscription failed: {:?}",
                                key_for_factory, index, e
                            );
                            return;
                        }
                    };

                    // Forward loop: recv from buffer, send as JoinTrigger
                    while let Ok(value) = reader.recv().await {
                        let trigger = JoinTrigger::Input {
                            index,
                            value: Box::new(value),
                        };
                        if tx.send(trigger).is_err() {
                            // Main join task dropped — exit
                            break;
                        }
                    }
                }) as BoxFuture<'static, ()>
            },
        );

        self.inputs.push((key_str, factory));
        self
    }

    /// Set the join state and begin configuring the trigger handler.
    pub fn with_state<S: Send + Sync + 'static>(self, initial: S) -> JoinStateBuilder<O, S, R> {
        JoinStateBuilder {
            inputs: self.inputs,
            initial_state: initial,
            _phantom: PhantomData,
        }
    }
}

/// Intermediate builder for setting the join trigger handler.
#[cfg(feature = "std")]
pub struct JoinStateBuilder<O, S, R: aimdb_executor::Spawn + 'static> {
    inputs: Vec<(String, JoinInputFactory<R>)>,
    initial_state: S,
    _phantom: PhantomData<(O, R)>,
}

#[cfg(feature = "std")]
impl<O, S, R> JoinStateBuilder<O, S, R>
where
    O: Send + Sync + Clone + Debug + 'static,
    S: Send + Sync + 'static,
    R: aimdb_executor::Spawn + 'static,
{
    /// Async handler called whenever any input produces a value.
    ///
    /// Receives a `JoinTrigger` (with index + typed value), mutable state,
    /// and a `Producer<O, R>` for emitting output values.
    pub fn on_trigger<F, Fut>(self, handler: F) -> JoinPipeline<O, R>
    where
        F: Fn(JoinTrigger, &mut S, &crate::Producer<O, R>) -> Fut + Send + Sync + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        let inputs = self.inputs;
        let initial = self.initial_state;

        let input_keys_for_descriptor: Vec<String> =
            inputs.iter().map(|(k, _)| k.clone()).collect();

        JoinPipeline {
            _input_keys: input_keys_for_descriptor.clone(),
            spawn_factory: Box::new(move |_| TransformDescriptor {
                input_keys: input_keys_for_descriptor,
                spawn_fn: Box::new(move |producer, db, ctx| {
                    Box::pin(run_join_transform(
                        db, inputs, producer, initial, handler, ctx,
                    ))
                }),
            }),
        }
    }
}

/// Completed multi-input join pipeline, ready to be stored in `TypedRecord`.
#[cfg(feature = "std")]
pub struct JoinPipeline<
    O: Send + Sync + Clone + Debug + 'static,
    R: aimdb_executor::Spawn + 'static,
> {
    pub(crate) _input_keys: Vec<String>,
    pub(crate) spawn_factory: Box<dyn FnOnce(()) -> TransformDescriptor<O, R> + Send + Sync>,
}

#[cfg(feature = "std")]
impl<O, R> JoinPipeline<O, R>
where
    O: Send + Sync + Clone + Debug + 'static,
    R: aimdb_executor::Spawn + 'static,
{
    /// Consume this pipeline and produce the `TransformDescriptor` for storage.
    pub(crate) fn into_descriptor(self) -> TransformDescriptor<O, R> {
        (self.spawn_factory)(())
    }
}

// ============================================================================
// Transform Task Runners
// ============================================================================

/// Spawned task for a single-input stateful transform.
///
/// Subscribes to the input record's buffer, calls the user closure per value,
/// and produces output values to the output record's buffer.
#[allow(unused_variables)]
async fn run_single_transform<I, O, S, R>(
    db: Arc<crate::AimDb<R>>,
    input_key: String,
    producer: crate::Producer<O, R>,
    mut state: S,
    transform_fn: impl Fn(&I, &mut S) -> Option<O> + Send + Sync + 'static,
) where
    I: Send + Sync + Clone + Debug + 'static,
    O: Send + Sync + Clone + Debug + 'static,
    S: Send + 'static,
    R: aimdb_executor::Spawn + 'static,
{
    let output_key = producer.key().to_string();

    // OBSERVABILITY (Incident Lesson #1): Always confirm startup
    #[cfg(feature = "tracing")]
    tracing::info!("🔄 Transform started: '{}' → '{}'", input_key, output_key);

    // Subscribe to the input record's buffer.
    // Incident Lesson #3: subscription failure is FATAL for transforms.
    let consumer = crate::typed_api::Consumer::<I, R>::new(db, input_key.clone());
    let mut reader = match consumer.subscribe() {
        Ok(r) => r,
        Err(_e) => {
            #[cfg(feature = "tracing")]
            tracing::error!(
                "🔄 Transform '{}' → '{}' FATAL: failed to subscribe to input: {:?}",
                input_key,
                output_key,
                _e
            );
            // Defense-in-depth: always emit on subscription failure
            #[cfg(all(feature = "std", not(feature = "tracing")))]
            eprintln!(
                "AIMDB TRANSFORM ERROR: '{}' → '{}' failed to subscribe to input: {:?}",
                input_key, output_key, _e
            );
            return;
        }
    };

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "✅ Transform '{}' → '{}' subscribed, entering event loop",
        input_key,
        output_key
    );

    // React to each input value
    loop {
        match reader.recv().await {
            Ok(input_value) => {
                if let Some(output_value) = transform_fn(&input_value, &mut state) {
                    let _ = producer.produce(output_value).await;
                }
            }
            Err(crate::DbError::BufferLagged { .. }) => {
                #[cfg(feature = "tracing")]
                tracing::warn!(
                    "🔄 Transform '{}' → '{}' lagged behind, some values skipped",
                    input_key,
                    output_key
                );
                // Continue processing — lag is not fatal
                continue;
            }
            Err(_) => {
                // Buffer closed or other error — exit
                #[cfg(feature = "tracing")]
                tracing::warn!(
                    "🔄 Transform '{}' → '{}' input closed, task exiting",
                    input_key,
                    output_key
                );
                break;
            }
        }
    }
}

/// Spawned task for a multi-input join transform.
///
/// Spawns N lightweight forwarder tasks (one per input), each subscribing to
/// its input buffer and forwarding type-erased `JoinTrigger` values to a shared
/// `mpsc::UnboundedChannel`. The main task reads from this channel and calls
/// the user handler.
#[cfg(feature = "std")]
#[allow(unused_variables)]
async fn run_join_transform<O, S, R, F, Fut>(
    db: Arc<crate::AimDb<R>>,
    inputs: Vec<(String, JoinInputFactory<R>)>,
    producer: crate::Producer<O, R>,
    mut state: S,
    handler: F,
    runtime_ctx: Arc<dyn Any + Send + Sync>,
) where
    O: Send + Sync + Clone + Debug + 'static,
    S: Send + 'static,
    R: aimdb_executor::Spawn + 'static,
    F: Fn(JoinTrigger, &mut S, &crate::Producer<O, R>) -> Fut + Send + Sync + 'static,
    Fut: core::future::Future<Output = ()> + Send + 'static,
{
    let output_key = producer.key().to_string();
    let input_keys: Vec<String> = inputs.iter().map(|(k, _)| k.clone()).collect();

    // OBSERVABILITY: Always confirm startup
    #[cfg(feature = "tracing")]
    tracing::info!(
        "🔄 Join transform started: {:?} → '{}'",
        input_keys,
        output_key
    );

    // Extract runtime for spawning forwarder tasks
    let runtime: &R = runtime_ctx
        .downcast_ref::<Arc<R>>()
        .map(|arc| arc.as_ref())
        .or_else(|| runtime_ctx.downcast_ref::<R>())
        .expect("Failed to extract runtime from context for join transform");

    // Create the shared trigger channel
    let (trigger_tx, mut trigger_rx) = tokio::sync::mpsc::unbounded_channel();

    // Spawn per-input forwarder tasks
    for (index, (_key, factory)) in inputs.into_iter().enumerate() {
        let tx = trigger_tx.clone();
        let db = db.clone();

        // Each forwarder subscribes to one input and sends JoinTrigger values
        let forwarder_future = factory(db, index, tx);
        if let Err(_e) = runtime.spawn(forwarder_future) {
            #[cfg(feature = "tracing")]
            tracing::error!(
                "🔄 Join transform '{}' FATAL: failed to spawn forwarder for input index {}",
                output_key,
                index
            );
            return;
        }
    }

    // Drop our copy of the sender — when all forwarders exit, the channel closes
    drop(trigger_tx);

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "✅ Join transform '{}' all forwarders spawned, entering event loop",
        output_key
    );

    // Event loop: dispatch typed triggers to the user handler
    while let Some(trigger) = trigger_rx.recv().await {
        handler(trigger, &mut state, &producer).await;
    }

    #[cfg(feature = "tracing")]
    tracing::warn!(
        "🔄 Join transform '{}' all inputs closed, task exiting",
        output_key
    );
}

// ============================================================================
// Dependency Graph
// ============================================================================

/// How a record gets its values — part of the dependency graph.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "std", derive(serde::Serialize))]
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
    /// `transform_inputs` maps output_key → list of input_keys for all transforms.
    /// `all_keys` is the set of all registered record keys.
    ///
    /// Returns `Err(DbError::CyclicDependency)` if the transform edges form a cycle.
    #[cfg(feature = "std")]
    pub fn validate_dag(
        transform_inputs: &[(String, Vec<String>)],
        all_keys: &std::collections::HashSet<String>,
    ) -> crate::DbResult<Vec<String>> {
        use alloc::collections::VecDeque;

        // First: check all input keys exist
        for (output_key, input_keys) in transform_inputs {
            for input_key in input_keys {
                if !all_keys.contains(input_key) {
                    return Err(crate::DbError::TransformInputNotFound {
                        output_key: output_key.clone(),
                        input_key: input_key.clone(),
                    });
                }
            }
        }

        // Build adjacency list and in-degree map for Kahn's algorithm
        let mut in_degree: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        let mut adjacency: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();

        // Initialize all keys with in-degree 0
        for key in all_keys {
            in_degree.entry(key.as_str()).or_insert(0);
            adjacency.entry(key.as_str()).or_default();
        }

        // Add transform edges: input → output
        for (output_key, input_keys) in transform_inputs {
            for input_key in input_keys {
                adjacency
                    .entry(input_key.as_str())
                    .or_default()
                    .push(output_key.as_str());
                *in_degree.entry(output_key.as_str()).or_insert(0) += 1;
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

        Ok(topo_order)
    }
}
