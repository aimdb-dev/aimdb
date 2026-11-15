//! Codec-independent, explicit-version migration dispatch (design 062 §7.2).

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    braced,
    parse::{Parse, ParseStream},
    parse_macro_input, Error, Token, Type,
};

mod kw {
    syn::custom_keyword!(Current);
    syn::custom_keyword!(version_field);
    syn::custom_keyword!(steps);
}
struct StepDef {
    step_ty: Type,
    older_ty: Type,
    newer_ty: Type,
}
impl Parse for StepDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let step_ty = input.parse()?;
        input.parse::<Token![:]>()?;
        let older_ty = input.parse()?;
        input.parse::<Token![=>]>()?;
        let newer_ty = input.parse()?;
        Ok(Self {
            step_ty,
            older_ty,
            newer_ty,
        })
    }
}
struct MigrationChainInput {
    current: Type,
    steps: Vec<StepDef>,
}
impl Parse for MigrationChainInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<Token![type]>()?;
        input.parse::<kw::Current>()?;
        input.parse::<Token![=]>()?;
        let current = input.parse()?;
        input.parse::<Token![;]>()?;
        if input.peek(kw::version_field) {
            return Err(Error::new(input.span(), "version_field was removed; pass the envelope version to migrate_from (design 062 §9: docs/design/062-value-envelope.md#9-what-breaks)"));
        }
        input.parse::<kw::steps>()?;
        let content;
        braced!(content in input);
        let mut steps = Vec::new();
        while !content.is_empty() {
            steps.push(content.parse()?);
            if content.is_empty() {
                break;
            }
            content.parse::<Token![,]>()?;
        }
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after migration steps"));
        }
        Ok(Self { current, steps })
    }
}
pub fn migration_chain(input: TokenStream) -> TokenStream {
    expand(parse_macro_input!(input as MigrationChainInput)).into()
}
fn expand(input: MigrationChainInput) -> TokenStream2 {
    let MigrationChainInput { current, steps } = input;
    let mut asserts = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let ty = &step.step_ty;
        asserts.push(quote! {
            assert!(<#ty as ::aimdb_data_contracts::MigrationStep>::FROM_VERSION < u32::MAX
                && <#ty as ::aimdb_data_contracts::MigrationStep>::TO_VERSION
                    == <#ty as ::aimdb_data_contracts::MigrationStep>::FROM_VERSION + 1,
                "migration step must increment version by exactly 1");
        });
        if let Some(next) = steps.get(i + 1) {
            let next = &next.step_ty;
            asserts.push(quote! {
                assert!(<#ty as ::aimdb_data_contracts::MigrationStep>::TO_VERSION
                    == <#next as ::aimdb_data_contracts::MigrationStep>::FROM_VERSION,
                    "migration steps must be sequential");
            });
        }
    }
    let minimum = if let Some(first) = steps.first() {
        let first = &first.step_ty;
        let last = &steps[steps.len() - 1].step_ty;
        asserts.push(quote! {
            assert!(<#first as ::aimdb_data_contracts::MigrationStep>::FROM_VERSION == 1,
                "first migration step must start at version 1");
            assert!(<#last as ::aimdb_data_contracts::MigrationStep>::TO_VERSION
                == <#current as ::aimdb_data_contracts::SchemaType>::VERSION,
                "last migration step must end at current VERSION");
        });
        quote! { 1 }
    } else {
        quote! { <#current as ::aimdb_data_contracts::SchemaType>::VERSION }
    };

    let mut helpers = Vec::new();
    let mut from_arms = Vec::new();
    let mut into_arms = Vec::new();
    let mut shape_arms = Vec::new();
    let mut proofs = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let ty = &step.step_ty;
        let older = &step.older_ty;
        let newer = &step.newer_ty;
        asserts.push(quote! {
            assert!(<#ty as ::aimdb_data_contracts::MigrationStep>::FROM_VERSION
                == <#older as ::aimdb_data_contracts::SchemaType>::VERSION,
                "migration step FROM_VERSION must match Older::VERSION");
            assert!(<#ty as ::aimdb_data_contracts::MigrationStep>::TO_VERSION
                == <#newer as ::aimdb_data_contracts::SchemaType>::VERSION,
                "migration step TO_VERSION must match Newer::VERSION");
        });
        let up = format_ident!("__up_{}", i);
        let down = format_ident!("__down_{}", i);
        let up_body = if i + 1 == steps.len() {
            quote! { Ok(newer) }
        } else {
            let next = format_ident!("__up_{}", i + 1);
            quote! { #next(newer) }
        };
        let down_input = if i + 1 == steps.len() {
            quote! { current }
        } else {
            let next = format_ident!("__down_{}", i + 1);
            quote! { #next(current)? }
        };
        helpers.push(quote! {
            fn #up(older: #older) -> Result<#current, ::aimdb_data_contracts::MigrationError> {
                let newer: #newer = <#ty as ::aimdb_data_contracts::MigrationStep>::up(older)?;
                #up_body
            }
            fn #down(current: #current) -> Result<#older, ::aimdb_data_contracts::MigrationError> {
                let newer: #newer = #down_input;
                <#ty as ::aimdb_data_contracts::MigrationStep>::down(newer)
            }
        });
        from_arms.push(quote! {
            v if v == <#older as ::aimdb_data_contracts::SchemaType>::VERSION => {
                let (older, _) = decoder.decode_payload::<#older>(payload).map_err(|_| {
                    ::aimdb_data_contracts::MigrationError::DeserializationFailed(concat!("failed to parse as ", stringify!(#older)))
                })?;
                #up(older)
            }
        });
        into_arms.push(quote! {
            v if v == <#older as ::aimdb_data_contracts::SchemaType>::VERSION => {
                let older = #down(self.clone())?;
                encoder.encode_payload(&older, out).map_err(::aimdb_data_contracts::MigrationError::from)
            }
        });
        shape_arms.push(quote! {
            v if v == <#older as ::aimdb_data_contracts::SchemaType>::VERSION => <#older as ::aimdb_data_contracts::ContractShape>::SHAPE,
        });
        proofs.push(quote! {
            impl ::aimdb_data_contracts::Reaches<{ <#older as ::aimdb_data_contracts::SchemaType>::VERSION }> for #current {}
            impl ::aimdb_data_contracts::VersionOf<#current> for #older {}
        });
    }
    quote! {
        const _: () = {
            #(#asserts)*
            #(#helpers)*
            #(#proofs)*
            impl ::aimdb_data_contracts::Reaches<{ <#current as ::aimdb_data_contracts::SchemaType>::VERSION }> for #current {}
            impl ::aimdb_data_contracts::VersionOf<#current> for #current {}
            impl ::aimdb_data_contracts::MigrationChain for #current {
                const MIN_VERSION: u32 = #minimum;
                fn shape_of(version: u32) -> Option<u64> {
                    match version {
                        #(#shape_arms)*
                        v if v == <Self as ::aimdb_data_contracts::SchemaType>::VERSION => <Self as ::aimdb_data_contracts::ContractShape>::SHAPE,
                        _ => None,
                    }
                }
                fn migrate_from<D: ::aimdb_data_contracts::PayloadDecoder>(version: u32, payload: &[u8], decoder: &D)
                    -> Result<Self, ::aimdb_data_contracts::MigrationError>
                {
                    if version > <Self as ::aimdb_data_contracts::SchemaType>::VERSION {
                        return Err(::aimdb_data_contracts::MigrationError::VersionTooNew {
                            source: version, current: <Self as ::aimdb_data_contracts::SchemaType>::VERSION,
                        });
                    }
                    match version {
                        #(#from_arms)*
                        v if v == <Self as ::aimdb_data_contracts::SchemaType>::VERSION => {
                            decoder.decode_payload::<Self>(payload).map(|(value, _)| value).map_err(|_| {
                                ::aimdb_data_contracts::MigrationError::DeserializationFailed(concat!("failed to parse as ", stringify!(#current)))
                            })
                        }
                        _ => Err(::aimdb_data_contracts::MigrationError::VersionTooOld { target: version, minimum: Self::MIN_VERSION }),
                    }
                }
                fn migrate_into<E: ::aimdb_data_contracts::PayloadEncoder>(&self, version: u32, encoder: &E, out: &mut [u8])
                    -> Result<usize, ::aimdb_data_contracts::MigrationError>
                {
                    if version > <Self as ::aimdb_data_contracts::SchemaType>::VERSION {
                        return Err(::aimdb_data_contracts::MigrationError::VersionTooNew {
                            source: version, current: <Self as ::aimdb_data_contracts::SchemaType>::VERSION,
                        });
                    }
                    match version {
                        #(#into_arms)*
                        v if v == <Self as ::aimdb_data_contracts::SchemaType>::VERSION => {
                            encoder.encode_payload(self, out).map_err(::aimdb_data_contracts::MigrationError::from)
                        }
                        _ => Err(::aimdb_data_contracts::MigrationError::VersionTooOld { target: version, minimum: Self::MIN_VERSION }),
                    }
                }
            }
        };
    }
}
