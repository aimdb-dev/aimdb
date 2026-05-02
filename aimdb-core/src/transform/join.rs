use core::any::Any;
use core::fmt::Debug;
use core::marker::PhantomData;

extern crate alloc;
use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use aimdb_executor::{JoinFanInRuntime, JoinQueue, JoinReceiver as _, JoinSender};

use crate::transform::TransformDescriptor;
use crate::typed_record::BoxFuture;

// ============================================================================
// JoinTrigger
// ============================================================================

/// Identifies which input produced a value in a multi-input join transform.
///
/// Passed to the handler registered with [`JoinStateBuilder::on_trigger`].
/// Use [`JoinTrigger::index`] to branch on the source input and
/// [`JoinTrigger::as_input`] to downcast the value to the concrete type.
pub enum JoinTrigger {
    Input {
        index: usize,
        value: Box<dyn Any + Send>,
    },
}

impl JoinTrigger {
    pub fn as_input<T: 'static>(&self) -> Option<&T> {
        match self {
            JoinTrigger::Input { value, .. } => value.downcast_ref::<T>(),
        }
    }

    pub fn index(&self) -> usize {
        match self {
            JoinTrigger::Input { index, .. } => *index,
        }
    }
}

// ============================================================================
// JoinBuilder → JoinPipeline
// ============================================================================

/// Type-erased factory for creating a forwarder task for one join input.
///
/// The third argument is the concrete sender from the runtime's join queue.
#[cfg(feature = "alloc")]
type JoinInputFactory<R> = Box<
    dyn FnOnce(
            Arc<crate::AimDb<R>>,
            usize,
            <<R as JoinFanInRuntime>::JoinQueue<JoinTrigger> as JoinQueue<JoinTrigger>>::Sender,
        ) -> BoxFuture<'static, ()>
        + Send
        + Sync,
>;

/// Configures a multi-input join transform.
///
/// Available on any runtime that implements [`aimdb_executor::JoinFanInRuntime`].
/// The fan-in queue (bounded channel between input forwarders and the trigger
/// loop) is created by the runtime adapter at database startup — capacity is an
/// internal constant chosen per adapter (Tokio: 64, Embassy: 8, WASM: 64).
///
/// Obtain via [`RecordRegistrar::transform_join`].
#[cfg(feature = "alloc")]
pub struct JoinBuilder<O, R: JoinFanInRuntime + 'static> {
    inputs: Vec<(String, JoinInputFactory<R>)>,
    _phantom: PhantomData<(O, R)>,
}

#[cfg(feature = "alloc")]
impl<O, R> JoinBuilder<O, R>
where
    O: Send + Sync + Clone + Debug + 'static,
    R: JoinFanInRuntime + 'static,
{
    pub(crate) fn new() -> Self {
        Self {
            inputs: Vec::new(),
            _phantom: PhantomData,
        }
    }

    /// Add a typed input to the join.
    pub fn input<I>(mut self, key: impl crate::RecordKey) -> Self
    where
        I: Send + Sync + Clone + Debug + 'static,
    {
        let key_str = key.as_str().to_string();
        let key_for_factory = key_str.clone();

        type Tx<R> =
            <<R as JoinFanInRuntime>::JoinQueue<JoinTrigger> as JoinQueue<JoinTrigger>>::Sender;

        let factory: JoinInputFactory<R> = Box::new(
            move |db: Arc<crate::AimDb<R>>, index: usize, tx: Tx<R>| {
                Box::pin(async move {
                    let consumer =
                        crate::typed_api::Consumer::<I, R>::new(db, key_for_factory.clone());
                    let mut reader = match consumer.subscribe() {
                        Ok(r) => r,
                        Err(_e) => {
                            #[cfg(feature = "tracing")]
                            tracing::error!(
                                "🔄 Join input '{}' (index {}) subscription failed: {:?}",
                                key_for_factory,
                                index,
                                _e
                            );
                            #[cfg(all(feature = "std", not(feature = "tracing")))]
                            eprintln!(
                                "AIMDB TRANSFORM ERROR: Join input '{}' (index {}) subscription failed: {:?}",
                                key_for_factory, index, _e
                            );
                            return;
                        }
                    };

                    while let Ok(value) = reader.recv().await {
                        let trigger = JoinTrigger::Input {
                            index,
                            value: Box::new(value),
                        };
                        if tx.send(trigger).await.is_err() {
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

/// Intermediate builder that holds join inputs and initial state.
///
/// Created by [`JoinBuilder::with_state`]. Call [`JoinStateBuilder::on_trigger`]
/// to complete the pipeline.
#[cfg(feature = "alloc")]
pub struct JoinStateBuilder<O, S, R: JoinFanInRuntime + 'static> {
    inputs: Vec<(String, JoinInputFactory<R>)>,
    initial_state: S,
    _phantom: PhantomData<(O, R)>,
}

#[cfg(feature = "alloc")]
impl<O, S, R> JoinStateBuilder<O, S, R>
where
    O: Send + Sync + Clone + Debug + 'static,
    S: Send + Sync + 'static,
    R: JoinFanInRuntime + 'static,
{
    /// Register the handler called whenever any input produces a value.
    ///
    /// The handler receives a [`JoinTrigger`] (which input fired), a mutable
    /// reference to the shared state `S`, and a [`crate::Producer`] to emit
    /// output values.
    ///
    /// Because the returned future must be `'static`, the handler must not
    /// capture the `state` or `producer` references directly in the `async`
    /// block. The idiomatic pattern is to update state synchronously, then
    /// clone/copy any values needed into an owned `async move` block:
    ///
    /// ```rust,ignore
    /// .on_trigger(|trigger, state, producer| {
    ///     state.value = trigger.as_input::<Input>().copied();
    ///     let p = producer.clone();
    ///     let v = state.value;
    ///     Box::pin(async move { if let Some(v) = v { let _ = p.produce(v).await; } })
    /// })
    /// ```
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

/// Completed multi-input join pipeline, ready to be registered on a record.
///
/// Produced by [`JoinStateBuilder::on_trigger`] and consumed by
/// [`RecordRegistrar::transform_join`]. Not normally constructed directly.
#[cfg(feature = "alloc")]
pub struct JoinPipeline<O: Send + Sync + Clone + Debug + 'static, R: JoinFanInRuntime + 'static> {
    pub(crate) _input_keys: Vec<String>,
    pub(crate) spawn_factory: Box<dyn FnOnce(()) -> TransformDescriptor<O, R> + Send + Sync>,
}

#[cfg(feature = "alloc")]
impl<O, R> JoinPipeline<O, R>
where
    O: Send + Sync + Clone + Debug + 'static,
    R: JoinFanInRuntime + 'static,
{
    pub(crate) fn into_descriptor(self) -> TransformDescriptor<O, R> {
        (self.spawn_factory)(())
    }
}

// ============================================================================
// Join Transform Task Runner
// ============================================================================

#[cfg(feature = "alloc")]
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
    R: JoinFanInRuntime + 'static,
    F: Fn(JoinTrigger, &mut S, &crate::Producer<O, R>) -> Fut + Send + Sync + 'static,
    Fut: core::future::Future<Output = ()> + Send + 'static,
{
    let output_key = producer.key().to_string();
    let input_keys: Vec<String> = inputs.iter().map(|(k, _)| k.clone()).collect();

    #[cfg(feature = "tracing")]
    tracing::info!(
        "🔄 Join transform started: {:?} → '{}'",
        input_keys,
        output_key
    );

    let runtime: &R = runtime_ctx
        .downcast_ref::<Arc<R>>()
        .map(|arc| arc.as_ref())
        .or_else(|| runtime_ctx.downcast_ref::<R>())
        .expect("Failed to extract runtime from context for join transform");

    // Create the shared trigger queue via runtime trait
    let queue = match runtime.create_join_queue::<JoinTrigger>() {
        Ok(q) => q,
        Err(_e) => {
            #[cfg(feature = "tracing")]
            tracing::error!(
                "🔄 Join transform '{}' FATAL: failed to create join queue",
                output_key
            );
            return;
        }
    };
    let (tx, mut rx) = queue.split();

    // Spawn per-input forwarder tasks
    for (index, (_key, factory)) in inputs.into_iter().enumerate() {
        let sender = tx.clone();
        let db = db.clone();

        let forwarder_future = factory(db, index, sender);
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

    // Drop our sender copy — when all forwarders exit the channel closes
    drop(tx);

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "✅ Join transform '{}' all forwarders spawned, entering event loop",
        output_key
    );

    while let Ok(trigger) = rx.recv().await {
        handler(trigger, &mut state, &producer).await;
    }

    #[cfg(feature = "tracing")]
    tracing::warn!(
        "🔄 Join transform '{}' all inputs closed, task exiting",
        output_key
    );
}
