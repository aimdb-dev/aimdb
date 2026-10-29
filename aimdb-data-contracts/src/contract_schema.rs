//! Schema registrar extension: report a record's [`SchemaType`] to core.
//!
//! Core cannot ask a record's type for its schema, so the registrar carries a
//! slot for it ([`RecordRegistrar::schema`]), reported in the dependency graph
//! (`graph.nodes`) and in `record.list`. The link and observe verbs fill it as
//! they wire the record; [`SchemaRegistrarExt::contract_schema`] fills it for
//! records that use neither.

use aimdb_core::typed_api::RecordRegistrar;

use crate::SchemaType;

/// Adds `.contract_schema()` to [`RecordRegistrar`] for [`SchemaType`] types.
pub trait SchemaRegistrarExt<'a, T>
where
    T: SchemaType + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    /// Report `T::NAME` and `T::VERSION` as the record's schema.
    fn contract_schema(&mut self) -> &mut RecordRegistrar<'a, T>;
}

impl<'a, T> SchemaRegistrarExt<'a, T> for RecordRegistrar<'a, T>
where
    T: SchemaType + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn contract_schema(&mut self) -> &mut RecordRegistrar<'a, T> {
        self.schema(T::NAME, T::VERSION)
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use aimdb_core::executor::test_support::NoopRuntimeOps;
    use aimdb_core::graph::SchemaInfo;
    use aimdb_core::AimDbBuilder;

    use super::SchemaRegistrarExt;
    use crate::SchemaType;

    #[derive(Clone, Debug)]
    struct Flow;

    impl SchemaType for Flow {
        const NAME: &'static str = "flow";
        const VERSION: u32 = 3;
    }

    #[test]
    fn contract_schema_reaches_the_graph() {
        futures::executor::block_on(async {
            let mut builder = AimDbBuilder::new().runtime(Arc::new(NoopRuntimeOps));
            builder.configure::<Flow>("flow", |reg| {
                reg.contract_schema();
            });
            let (db, _runner) = builder.build().await.expect("build");
            let node = db.inner().dependency_graph().node("flow").unwrap();
            assert_eq!(
                node.schema,
                Some(SchemaInfo {
                    name: "flow".into(),
                    version: 3
                })
            );
        });
    }
}
