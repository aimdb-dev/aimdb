use core::any::Any;
use core::fmt::Debug;
use core::marker::PhantomData;

use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use aimdb_executor::{ExecutorError, ExecutorResult};

use crate::transform::{CollectedTransform, TransformDescriptor};
use crate::typed_record::BoxFuture;

// ============================================================================
// Fan-in queue
// ============================================================================

/// Capacity of the bounded fan-in channel between input forwarders and the
/// trigger loop. Sized per target to match the per-adapter GAT queue family
/// this replaces: 64 on std and wasm32 (tokio/WASM used 64 — a blocked
/// forwarder stops draining its input buffer, so burst headroom matters
/// there; the WASM adapter builds core without `std`, hence the target cfg).
#[cfg(any(feature = "std", target_arch = "wasm32"))]
const JOIN_QUEUE_CAPACITY: usize = 64;
/// Embedded no_std: the Embassy queue this replaces used 8; 16 adds burst
/// headroom at a few words of RAM per slot.
#[cfg(not(any(feature = "std", target_arch = "wasm32")))]
const JOIN_QUEUE_CAPACITY: usize = 16;

/// One bounded queue for all runtimes — the same `async-channel` primitive the
/// session engine already uses on tokio, Embassy, and WASM.
fn join_channel() -> (
    async_channel::Sender<JoinTrigger>,
    async_channel::Receiver<JoinTrigger>,
) {
    async_channel::bounded(JOIN_QUEUE_CAPACITY)
}

// ============================================================================
// JoinTrigger
// ============================================================================

/// Identifies which input produced a value in a multi-input join transform.
///
/// Passed to the event loop inside the closure registered with [`JoinBuilder::on_triggers`].
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
// JoinEventRx — trigger receiver
// ============================================================================

/// Receiver for join trigger events.
///
/// Obtained as the first argument to the [`JoinBuilder::on_triggers`] closure.
/// Call `.recv().await` in a loop to consume trigger events from all input forwarders.
/// Returns `Err` when all input forwarders have exited and the channel is closed.
///
/// ```rust,ignore
/// .on_triggers(|mut rx, producer| async move {
///     let mut last_a: Option<f32> = None;
///     let mut last_b: Option<f32> = None;
///     while let Ok(trigger) = rx.recv().await {
///         match trigger.index() {
///             0 => last_a = trigger.as_input::<InputA>().copied(),
///             1 => last_b = trigger.as_input::<InputB>().copied(),
///             _ => {}
///         }
///         if let (Some(a), Some(b)) = (last_a, last_b) {
///             producer.produce(compute(a, b));
///         }
///     }
/// })
/// ```
pub struct JoinEventRx {
    inner: async_channel::Receiver<JoinTrigger>,
}

impl JoinEventRx {
    fn new(inner: async_channel::Receiver<JoinTrigger>) -> Self {
        Self { inner }
    }

    /// Receive the next trigger event.
    ///
    /// Returns `Ok(JoinTrigger)` when an input fires, or `Err` when all input
    /// forwarders have dropped their senders — on every runtime, ending any
    /// `while let Ok(_) = rx.recv().await` loop.
    pub async fn recv(&mut self) -> ExecutorResult<JoinTrigger> {
        self.inner
            .recv()
            .await
            .map_err(|_| ExecutorError::QueueClosed)
    }
}

// ============================================================================
// JoinBuilder → JoinPipeline
// ============================================================================

/// Type-erased factory for creating a forwarder task for one join input.
type JoinInputFactory = Box<
    dyn FnOnce(
            Arc<crate::AimDb>,
            usize,
            async_channel::Sender<JoinTrigger>,
        ) -> BoxFuture<'static, ()>
        + Send
        + Sync,
>;

/// Configures a multi-input join transform.
///
/// Available on every runtime. The fan-in queue (bounded channel between input
/// forwarders and the trigger loop) lives in core; its capacity is
/// [`JOIN_QUEUE_CAPACITY`].
///
/// Obtain via [`RecordRegistrar::transform_join`](crate::RecordRegistrar::transform_join).
pub struct JoinBuilder<O> {
    inputs: Vec<(String, JoinInputFactory)>,
    _phantom: PhantomData<O>,
}

impl<O> JoinBuilder<O>
where
    O: Send + Sync + Clone + Debug + 'static,
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

        let factory: JoinInputFactory = Box::new(
            move |db: Arc<crate::AimDb>, index: usize, tx: async_channel::Sender<JoinTrigger>| {
                Box::pin(async move {
                    let consumer = match db.consumer::<I>(&key_for_factory) {
                        Ok(c) => c,
                        Err(_e) => {
                            log_error!(
                                "🔄 Join input '{}' (index {}) consumer resolution failed: {:?}",
                                key_for_factory,
                                index,
                                _e
                            );
                            return;
                        }
                    };
                    let mut reader = consumer.subscribe();

                    loop {
                        let value = match reader.recv().await {
                            Ok(v) => v,
                            // SPMC-ring overflow: the reader recovers (cursor
                            // resets to the oldest live value), so a transient
                            // lag must not permanently silence this input —
                            // same policy as every other recv loop in core.
                            Err(crate::DbError::BufferLagged { .. }) => {
                                log_warn!(
                                    "🔄 Join input '{}' (index {}) lagged behind, some values skipped",
                                    key_for_factory,
                                    index
                                );
                                continue;
                            }
                            // Buffer closed / fatal — the input record is gone.
                            Err(_) => break,
                        };
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

    /// Complete the pipeline by providing an async task that owns the event loop and state.
    ///
    /// The closure receives a [`JoinEventRx`] to read trigger events and a `Producer<O>`
    /// to emit output values. Both are owned — moved into the `async move` block — so the
    /// closure can freely hold borrows across `.await` points and maintain any state it needs.
    ///
    /// The task runs until all input forwarders close (i.e., all upstream records stop producing).
    ///
    /// ```rust,ignore
    /// .on_triggers(|mut rx, producer| async move {
    ///     let mut last_a: Option<f32> = None;
    ///     let mut last_b: Option<f32> = None;
    ///     while let Ok(trigger) = rx.recv().await {
    ///         match trigger.index() {
    ///             0 => last_a = trigger.as_input::<InputA>().copied(),
    ///             1 => last_b = trigger.as_input::<InputB>().copied(),
    ///             _ => {}
    ///         }
    ///         if let (Some(a), Some(b)) = (last_a, last_b) {
    ///             producer.produce(compute(a, b));
    ///         }
    ///     }
    /// })
    /// ```
    pub fn on_triggers<F, Fut>(self, handler: F) -> JoinPipeline<O>
    where
        F: FnOnce(JoinEventRx, crate::Producer<O>) -> Fut + Send + 'static,
        Fut: core::future::Future<Output = ()> + Send + 'static,
    {
        let inputs = self.inputs;
        let input_keys_for_descriptor: Vec<String> =
            inputs.iter().map(|(k, _)| k.clone()).collect();

        JoinPipeline {
            spawn_factory: Box::new(move |_| TransformDescriptor {
                input_keys: input_keys_for_descriptor,
                build_fn: Box::new(move |producer, db, output_key| {
                    build_join_collected(db, inputs, producer, handler, output_key)
                }),
            }),
        }
    }
}

/// Completed multi-input join pipeline, ready to be registered on a record.
///
/// Produced by [`JoinBuilder::on_triggers`] and consumed by
/// [`RecordRegistrar::transform_join`](crate::RecordRegistrar::transform_join). Not normally constructed directly.
pub struct JoinPipeline<O: Send + Sync + Clone + Debug + 'static> {
    pub(crate) spawn_factory: Box<dyn FnOnce(()) -> TransformDescriptor<O> + Send>,
}

impl<O> JoinPipeline<O>
where
    O: Send + Sync + Clone + Debug + 'static,
{
    pub(crate) fn into_descriptor(self) -> TransformDescriptor<O> {
        (self.spawn_factory)(())
    }
}

// ============================================================================
// Join Transform Build (forwarders + handler future, both collected at build time)
// ============================================================================

fn build_join_collected<O, F, Fut>(
    db: Arc<crate::AimDb>,
    inputs: Vec<(String, JoinInputFactory)>,
    producer: crate::Producer<O>,
    handler: F,
    output_key: &str,
) -> CollectedTransform
where
    O: Send + Sync + Clone + Debug + 'static,
    F: FnOnce(JoinEventRx, crate::Producer<O>) -> Fut + Send + 'static,
    Fut: core::future::Future<Output = ()> + Send + 'static,
{
    // Output key is threaded in from the descriptor so diagnostics stay
    // unambiguous when multiple records share output type `O` (design 029).
    // Owned copies are build-time, one-shot allocations.
    let output_key = output_key.to_string();
    let input_keys: Vec<String> = inputs.iter().map(|(k, _)| k.clone()).collect();
    log_info!(
        "🔄 Join transform building: {:?} → '{}'",
        input_keys,
        output_key
    );

    let (tx, rx) = join_channel();

    // Build all forwarder futures eagerly; they will be driven by AimDbRunner.
    let fanin_futures: Vec<BoxFuture<'static, ()>> = inputs
        .into_iter()
        .enumerate()
        .map(|(index, (_key, factory))| {
            let sender = tx.clone();
            factory(db.clone(), index, sender)
        })
        .collect();

    // Drop our local sender so the receiver closes once all forwarders exit.
    drop(tx);

    let task_future: BoxFuture<'static, ()> = Box::pin(async move {
        log_debug!(
            "✅ Join transform '{}' handing receiver to user task",
            output_key
        );

        handler(JoinEventRx::new(rx), producer).await;

        log_warn!("🔄 Join transform '{}' user task exited", output_key);
    });

    CollectedTransform {
        task_future,
        fanin_futures,
    }
}
