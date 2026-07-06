//! `migration_chain!` — variable-arity proc-macro replacement for the old
//! 3-arm `macro_rules!` in `aimdb-data-contracts`.
//!
//! Grammar (unchanged from the declarative macro):
//!
//! ```text
//! migration_chain! {
//!     type Current = MyType;
//!     version_field = "schema_version";
//!     steps {
//!         StepV1ToV2: TypeV1 => TypeV2,
//!         StepV2ToV3: TypeV2 => MyType,
//!     }
//! }
//! ```
//!
//! Generated code refers back to `aimdb-data-contracts` by hardcoded
//! absolute path (`::aimdb_data_contracts::...`), matching the
//! `RecordKey` → `aimdb_core` precedent in this same crate — a proc-macro
//! has no `$crate`, and `aimdb-derive` deliberately does not depend on
//! `aimdb-data-contracts` (no cycle), so the path is spelled out instead
//! of resolved via `proc-macro-crate`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    braced,
    parse::{Parse, ParseStream},
    parse_macro_input, Error, LitStr, Token, Type,
};

mod kw {
    syn::custom_keyword!(Current);
    syn::custom_keyword!(version_field);
    syn::custom_keyword!(steps);
}

struct StepDef {
    step_ty: Type,
    older_ty: Type,
}

impl Parse for StepDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let step_ty: Type = input.parse()?;
        input.parse::<Token![:]>()?;
        let older_ty: Type = input.parse()?;
        input.parse::<Token![=>]>()?;
        // The newer-side type isn't needed for codegen (each step's Newer
        // is only ever referenced through `MigrationStep::Newer`), but it
        // must still be consumed to stay grammar-compatible.
        let _newer_ty: Type = input.parse()?;
        Ok(StepDef { step_ty, older_ty })
    }
}

struct MigrationChainInput {
    current: Type,
    version_field: LitStr,
    steps: Vec<StepDef>,
}

impl Parse for MigrationChainInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<Token![type]>()?;
        input.parse::<kw::Current>()?;
        input.parse::<Token![=]>()?;
        let current: Type = input.parse()?;
        input.parse::<Token![;]>()?;

        input.parse::<kw::version_field>()?;
        input.parse::<Token![=]>()?;
        let version_field: LitStr = input.parse()?;
        input.parse::<Token![;]>()?;

        input.parse::<kw::steps>()?;
        let content;
        braced!(content in input);

        let mut steps = Vec::new();
        while !content.is_empty() {
            steps.push(content.parse::<StepDef>()?);
            if content.is_empty() {
                break;
            }
            content.parse::<Token![,]>()?;
        }

        if steps.is_empty() {
            return Err(Error::new(
                proc_macro2::Span::call_site(),
                "migration_chain! requires at least one step",
            ));
        }

        Ok(MigrationChainInput {
            current,
            version_field,
            steps,
        })
    }
}

pub fn migration_chain(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as MigrationChainInput);
    expand(parsed).into()
}

fn expand(input: MigrationChainInput) -> TokenStream2 {
    let MigrationChainInput {
        current,
        version_field,
        steps,
    } = input;
    let n = steps.len();

    // ── Compile-time validation (same assertions the declarative macro
    // generated, produced here by zipping adjacent steps instead of by
    // hand-unrolling one arm per arity). ─────────────────────────────
    let mut asserts = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let step_ty = &step.step_ty;
        asserts.push(quote! {
            assert!(
                <#step_ty as ::aimdb_data_contracts::MigrationStep>::TO_VERSION
                    == <#step_ty as ::aimdb_data_contracts::MigrationStep>::FROM_VERSION + 1,
                "migration step must increment version by exactly 1"
            );
        });
        if i + 1 < n {
            let next_step_ty = &steps[i + 1].step_ty;
            asserts.push(quote! {
                assert!(
                    <#step_ty as ::aimdb_data_contracts::MigrationStep>::TO_VERSION
                        == <#next_step_ty as ::aimdb_data_contracts::MigrationStep>::FROM_VERSION,
                    "migration steps must be sequential"
                );
            });
        }
    }
    let first_step_ty = &steps[0].step_ty;
    asserts.push(quote! {
        assert!(
            <#first_step_ty as ::aimdb_data_contracts::MigrationStep>::FROM_VERSION == 1,
            "first migration step must start at version 1"
        );
    });
    let last_step_ty = &steps[n - 1].step_ty;
    asserts.push(quote! {
        assert!(
            <#last_step_ty as ::aimdb_data_contracts::MigrationStep>::TO_VERSION
                == <#current as ::aimdb_data_contracts::SchemaType>::VERSION,
            "last migration step must end at current VERSION"
        );
    });

    // ── Per-step helper functions: O(N) total, not O(N²). Each `__up_k`
    // applies step k then recurses into `__up_{k+1}`; each `__down_k`
    // recurses into `__down_{k+1}` first, then applies step k on the way
    // back out. `k` is 1-indexed and matches `MigrationStep::FROM_VERSION`
    // for that step. ───────────────────────────────────────────────────
    let mut up_fns = Vec::new();
    let mut down_fns = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let k = i + 1;
        let step_ty = &step.step_ty;
        let older_ty = &step.older_ty;

        let up_fn = format_ident!("__up_{}", k);
        let up_body = if i + 1 == n {
            quote! { <#step_ty as ::aimdb_data_contracts::MigrationStep>::up(older) }
        } else {
            let next_up_fn = format_ident!("__up_{}", k + 1);
            quote! {
                let newer = <#step_ty as ::aimdb_data_contracts::MigrationStep>::up(older)?;
                #next_up_fn(newer)
            }
        };
        up_fns.push(quote! {
            fn #up_fn(older: #older_ty) -> Result<#current, ::aimdb_data_contracts::MigrationError> {
                #up_body
            }
        });

        let down_fn = format_ident!("__down_{}", k);
        let down_body = if i + 1 == n {
            quote! { <#step_ty as ::aimdb_data_contracts::MigrationStep>::down(current) }
        } else {
            let next_down_fn = format_ident!("__down_{}", k + 1);
            quote! {
                let newer = #next_down_fn(current)?;
                <#step_ty as ::aimdb_data_contracts::MigrationStep>::down(newer)
            }
        };
        down_fns.push(quote! {
            fn #down_fn(current: #current) -> Result<#older_ty, ::aimdb_data_contracts::MigrationError> {
                #down_body
            }
        });
    }

    // ── `migrate_from_bytes` / `migrate_to_version` match arms — one
    // O(1) arm per historical version, calling the matching helper. ────
    let mut from_arms = Vec::new();
    let mut to_arms = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let k = i + 1;
        let k_lit = k as u32;
        let older_ty = &step.older_ty;
        let up_fn = format_ident!("__up_{}", k);
        let down_fn = format_ident!("__down_{}", k);

        from_arms.push(quote! {
            #k_lit => {
                let older: #older_ty = ::aimdb_data_contracts::__private::serde_json::from_slice(data).map_err(|_| {
                    ::aimdb_data_contracts::MigrationError::DeserializationFailed(concat!(
                        "failed to parse as ", stringify!(#older_ty)
                    ))
                })?;
                #up_fn(older)
            }
        });

        to_arms.push(quote! {
            #k_lit => {
                let older = #down_fn(self.clone())?;
                ::aimdb_data_contracts::__private::serde_json::to_vec(&older).map_err(|_| {
                    ::aimdb_data_contracts::MigrationError::SerializationFailed(concat!(
                        "failed to serialize as ", stringify!(#older_ty)
                    ))
                })
            }
        });
    }

    quote! {
        const _: () = {
            #(#asserts)*

            #(#up_fns)*
            #(#down_fns)*

            impl ::aimdb_data_contracts::MigrationChain for #current {
                const MIN_VERSION: u32 = 1;

                fn migrate_from_bytes(data: &[u8]) -> Result<Self, ::aimdb_data_contracts::MigrationError> {
                    // Two-pass, tree-free version scan: pass 1 reads just the version
                    // field via a tiny probe struct (peak allocation O(concrete struct),
                    // not O(payload tree)); pass 2 re-parses the same bytes directly
                    // into the matched concrete type below.
                    #[derive(::serde::Deserialize)]
                    struct __VersionProbe {
                        #[serde(rename = #version_field)]
                        version: u32,
                    }

                    let version = match ::aimdb_data_contracts::__private::serde_json::from_slice::<__VersionProbe>(data) {
                        Ok(probe) => probe.version,
                        Err(e) if e.is_data() => {
                            return Err(::aimdb_data_contracts::MigrationError::MissingVersion);
                        }
                        Err(_) => {
                            return Err(::aimdb_data_contracts::MigrationError::DeserializationFailed(
                                "invalid JSON",
                            ));
                        }
                    };

                    if version > <#current as ::aimdb_data_contracts::SchemaType>::VERSION {
                        return Err(::aimdb_data_contracts::MigrationError::VersionTooNew {
                            source: version,
                            current: <#current as ::aimdb_data_contracts::SchemaType>::VERSION,
                        });
                    }

                    match version {
                        #(#from_arms)*
                        v if v == <#current as ::aimdb_data_contracts::SchemaType>::VERSION => {
                            ::aimdb_data_contracts::__private::serde_json::from_slice(data).map_err(|_| {
                                ::aimdb_data_contracts::MigrationError::DeserializationFailed(concat!(
                                    "failed to parse as ", stringify!(#current)
                                ))
                            })
                        }
                        // Only reachable for a version below MIN_VERSION (the
                        // `> VERSION` guard above and the `1..=VERSION` arms cover
                        // everything else), so this is a too-old version, not too-new.
                        _ => Err(::aimdb_data_contracts::MigrationError::VersionTooOld {
                            target: version,
                            minimum: Self::MIN_VERSION,
                        }),
                    }
                }

                fn migrate_to_version(
                    &self,
                    target_version: u32,
                ) -> Result<::aimdb_data_contracts::__private::alloc::vec::Vec<u8>, ::aimdb_data_contracts::MigrationError> {
                    if target_version < Self::MIN_VERSION {
                        return Err(::aimdb_data_contracts::MigrationError::VersionTooOld {
                            target: target_version,
                            minimum: Self::MIN_VERSION,
                        });
                    }
                    if target_version > <#current as ::aimdb_data_contracts::SchemaType>::VERSION {
                        return Err(::aimdb_data_contracts::MigrationError::VersionTooNew {
                            source: target_version,
                            current: <#current as ::aimdb_data_contracts::SchemaType>::VERSION,
                        });
                    }

                    match target_version {
                        #(#to_arms)*
                        v if v == <#current as ::aimdb_data_contracts::SchemaType>::VERSION => {
                            ::aimdb_data_contracts::__private::serde_json::to_vec(self).map_err(|_| {
                                ::aimdb_data_contracts::MigrationError::SerializationFailed(concat!(
                                    "failed to serialize as ", stringify!(#current)
                                ))
                            })
                        }
                        _ => unreachable!(),
                    }
                }
            }
        };
    }
}
