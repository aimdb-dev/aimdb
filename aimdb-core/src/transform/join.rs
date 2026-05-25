use core::any::Any;
use core::fmt::Debug;
use core::marker::PhantomData;

use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use aimdb_executor::{ExecutorResult, JoinFanInRuntime, JoinQueue, JoinReceiver, JoinSender};

use crate::transform::{CollectedTransform, TransformDescriptor};
use crate::typed_record::BoxFuture;

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
// JoinEventRx — type-erased trigger receiver
// ============================================================================

/// Type-erased receiver for join trigger events.
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
///             producer.produce(compute(a, b)).await.ok();
///         }
///     }
/// })
/// ```
pub struct JoinEventRx {
    inner: Box<dyn DynJoinRx + Send>,
}

impl JoinEventRx {
    fn new<R: JoinReceiver<JoinTrigger> + Send + 'static>(inner: R) -> Self {
        Self {
            inner: Box::new(inner),
        }
    }

    /// Receive the next trigger event.
    ///
    /// Returns `Ok(JoinTrigger)` when an input fires, or `Err` when all inputs are closed.
    ///
    /// # Runtime portability
    ///
    /// On Tokio and WASM, the channel closes once every input forwarder has
    /// dropped its sender, and `recv` returns `Err`, ending any
    /// `while let Ok(_) = rx.recv().await` loop.
    ///
    /// On Embassy the channel **never** closes — this branch is unreachable
    /// and the loop runs for the device lifetime. Portable handlers should
    /// not rely on the loop exiting to release resources.
    pub async fn recv(&mut self) -> ExecutorResult<JoinTrigger> {
        self.inner.recv_boxed().await
    }
}

trait DynJoinRx: Send {
    fn recv_boxed<'a>(&'a mut self) -> BoxFuture<'a, ExecutorResult<JoinTrigger>>;
}

impl<R: JoinReceiver<JoinTrigger> + Send> DynJoinRx for R {
    fn recv_boxed<'a>(&'a mut self) -> BoxFuture<'a, ExecutorResult<JoinTrigger>> {
        Box::pin(self.recv())
    }
}

// ============================================================================
// JoinBuilder → JoinPipeline
// ============================================================================

/// Type-erased factory for creating a forwarder task for one join input.
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
                    let consumer = match db.consumer::<I>(&key_for_factory) {
                        Ok(c) => c,
                        Err(_e) => {
                            #[cfg(feature = "tracing")]
                            tracing::error!(
                                "🔄 Join input '{}' (index {}) consumer resolution failed: {:?}",
                                key_for_factory,
                                index,
                                _e
                            );
                            #[cfg(all(feature = "std", not(feature = "tracing")))]
                            eprintln!(
                                "AIMDB TRANSFORM ERROR: Join input '{}' (index {}) consumer resolution failed: {:?}",
                                key_for_factory, index, _e
                            );
                            return;
                        }
                    };
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
    ///             producer.produce(compute(a, b)).await.ok();
    ///         }
    ///     }
    /// })
    /// ```
    pub fn on_triggers<F, Fut>(self, handler: F) -> JoinPipeline<O, R>
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
                build_fn: Box::new(move |producer, db, runtime, output_key| {
                    build_join_collected(db, inputs, producer, handler, runtime, output_key)
                }),
            }),
        }
    }
}

/// Completed multi-input join pipeline, ready to be registered on a record.
///
/// Produced by [`JoinBuilder::on_triggers`] and consumed by
/// [`RecordRegistrar::transform_join`]. Not normally constructed directly.
#[cfg(feature = "alloc")]
pub struct JoinPipeline<O: Send + Sync + Clone + Debug + 'static, R: JoinFanInRuntime + 'static> {
    pub(crate) spawn_factory: Box<dyn FnOnce(()) -> TransformDescriptor<O, R> + Send>,
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
// Join Transform Build (forwarders + handler future, both collected at build time)
// ============================================================================

#[cfg(feature = "alloc")]
#[allow(unused_variables)]
fn build_join_collected<O, R, F, Fut>(
    db: Arc<crate::AimDb<R>>,
    inputs: Vec<(String, JoinInputFactory<R>)>,
    producer: crate::Producer<O>,
    handler: F,
    runtime: Arc<R>,
    output_key: &str,
) -> CollectedTransform
where
    O: Send + Sync + Clone + Debug + 'static,
    R: JoinFanInRuntime + 'static,
    F: FnOnce(JoinEventRx, crate::Producer<O>) -> Fut + Send + 'static,
    Fut: core::future::Future<Output = ()> + Send + 'static,
{
    // Output key is threaded in from the descriptor so diagnostics stay
    // unambiguous when multiple records share output type `O` (design 029).
    #[cfg(feature = "tracing")]
    let output_key_owned = output_key.to_string();
    #[cfg(not(feature = "tracing"))]
    let _ = output_key;

    #[cfg(feature = "tracing")]
    {
        let input_keys: Vec<String> = inputs.iter().map(|(k, _)| k.clone()).collect();
        tracing::info!(
            "🔄 Join transform building: {:?} → '{}'",
            input_keys,
            output_key_owned
        );
    }

    let queue = match runtime.create_join_queue::<JoinTrigger>() {
        Ok(q) => q,
        Err(_e) => {
            #[cfg(feature = "tracing")]
            tracing::error!(
                "🔄 Join transform '{}' FATAL: failed to create join queue",
                output_key_owned
            );
            // Empty collected transform — caller still receives a valid descriptor.
            return CollectedTransform {
                task_future: Box::pin(async {}),
                fanin_futures: Vec::new(),
            };
        }
    };
    let (tx, rx) = queue.split();

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
        #[cfg(feature = "tracing")]
        tracing::debug!(
            "✅ Join transform '{}' handing receiver to user task",
            output_key_owned
        );

        handler(JoinEventRx::new(rx), producer).await;

        #[cfg(feature = "tracing")]
        tracing::warn!("🔄 Join transform '{}' user task exited", output_key_owned);
    });

    CollectedTransform {
        task_future,
        fanin_futures,
    }
}
