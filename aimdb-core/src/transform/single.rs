use core::fmt::Debug;
use core::marker::PhantomData;

use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec,
};

use crate::transform::{CollectedTransform, TransformDescriptor};

// ============================================================================
// TransformBuilder → TransformPipeline
// ============================================================================

/// Configures a single-input transform pipeline.
///
/// Created by `RecordRegistrar::transform_raw()`. Use `.map()` for stateless
/// transforms or `.with_state()` for stateful transforms.
pub struct TransformBuilder<I, O, R: aimdb_executor::RuntimeAdapter + 'static> {
    input_key: String,
    _phantom: PhantomData<(I, O, R)>,
}

impl<I, O, R> TransformBuilder<I, O, R>
where
    I: Send + Sync + Clone + Debug + 'static,
    O: Send + Sync + Clone + Debug + 'static,
    R: aimdb_executor::RuntimeAdapter + 'static,
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
pub struct StatefulTransformBuilder<I, O, S, R: aimdb_executor::RuntimeAdapter + 'static> {
    input_key: String,
    initial_state: S,
    _phantom: PhantomData<(I, O, R)>,
}

impl<I, O, S, R> StatefulTransformBuilder<I, O, S, R>
where
    I: Send + Sync + Clone + Debug + 'static,
    O: Send + Sync + Clone + Debug + 'static,
    S: Send + Sync + 'static,
    R: aimdb_executor::RuntimeAdapter + 'static,
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
    R: aimdb_executor::RuntimeAdapter + 'static,
> {
    pub(crate) input_key: String,
    pub(crate) spawn_factory: Box<dyn FnOnce(String) -> TransformDescriptor<O, R> + Send + Sync>,
    pub(crate) _phantom_i: PhantomData<I>,
}

impl<I, O, R> TransformPipeline<I, O, R>
where
    I: Send + Sync + Clone + Debug + 'static,
    O: Send + Sync + Clone + Debug + 'static,
    R: aimdb_executor::RuntimeAdapter + 'static,
{
    pub(crate) fn into_descriptor(self) -> TransformDescriptor<O, R> {
        (self.spawn_factory)(self.input_key)
    }
}

fn create_single_transform_descriptor<I, O, S, R>(
    input_key: String,
    initial_state: S,
    transform_fn: impl Fn(&I, &mut S) -> Option<O> + Send + Sync + 'static,
) -> TransformDescriptor<O, R>
where
    I: Send + Sync + Clone + Debug + 'static,
    O: Send + Sync + Clone + Debug + 'static,
    S: Send + Sync + 'static,
    R: aimdb_executor::RuntimeAdapter + 'static,
{
    let input_key_clone = input_key.clone();
    let input_keys = vec![input_key];

    TransformDescriptor {
        input_keys,
        build_fn: Box::new(
            move |producer, db, _runtime, output_key| CollectedTransform {
                task_future: Box::pin(run_single_transform::<I, O, S, R>(
                    db,
                    input_key_clone,
                    output_key.to_string(),
                    producer,
                    initial_state,
                    transform_fn,
                )),
                fanin_futures: alloc::vec::Vec::new(),
            },
        ),
    }
}

// ============================================================================
// Transform Task Runner
// ============================================================================

#[allow(unused_variables)]
pub(crate) async fn run_single_transform<I, O, S, R>(
    db: Arc<crate::AimDb<R>>,
    input_key: String,
    output_key: String,
    producer: crate::Producer<O>,
    mut state: S,
    transform_fn: impl Fn(&I, &mut S) -> Option<O> + Send + Sync + 'static,
) where
    I: Send + Sync + Clone + Debug + 'static,
    O: Send + Sync + Clone + Debug + 'static,
    S: Send + 'static,
    R: aimdb_executor::RuntimeAdapter + 'static,
{
    // `Producer<O>` no longer exposes a `.key()` accessor (design 029) — the
    // output key is threaded in by the transform descriptor so diagnostics
    // remain unambiguous when multiple records share type `O`.
    #[cfg(feature = "tracing")]
    tracing::info!("🔄 Transform started: '{}' → '{}'", input_key, output_key);

    let consumer = match db.consumer::<I>(&input_key) {
        Ok(c) => c,
        Err(_e) => {
            #[cfg(feature = "tracing")]
            tracing::error!(
                "🔄 Transform '{}' → '{}' FATAL: failed to resolve consumer: {:?}",
                input_key,
                output_key,
                _e
            );
            return;
        }
    };
    let mut reader = consumer.subscribe();

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "✅ Transform '{}' → '{}' subscribed, entering event loop",
        input_key,
        output_key
    );

    loop {
        match reader.recv().await {
            Ok(input_value) => {
                if let Some(output_value) = transform_fn(&input_value, &mut state) {
                    producer.produce(output_value);
                }
            }
            Err(crate::DbError::BufferLagged { .. }) => {
                #[cfg(feature = "tracing")]
                tracing::warn!(
                    "🔄 Transform '{}' → '{}' lagged behind, some values skipped",
                    input_key,
                    output_key
                );
                continue;
            }
            Err(_) => {
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
