//! Derive macros for AimDB record key types
//!
//! This crate provides the `#[derive(RecordKey)]` macro for defining
//! compile-time checked record keys with optional connector metadata.
//!
//! # Example
//!
//! ```rust,ignore
//! use aimdb_derive::RecordKey;
//!
//! #[derive(RecordKey, Clone, Copy, PartialEq, Eq, Hash)]
//! pub enum AppKey {
//!     #[key = "temp.indoor"]
//!     #[link_address = "mqtt://sensors/temp/indoor"]
//!     TempIndoor,
//!
//!     #[key = "temp.outdoor"]
//!     #[link_address = "knx://9/1/0"]
//!     TempOutdoor,
//! }
//! ```

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Error, Fields, Lit, Meta};

/// Derive the `RecordKey` trait for an enum
///
/// Each variant must have a `#[key = "..."]` attribute specifying its string key.
/// Optionally, the enum can have a `#[key_prefix = "..."]` attribute to prepend
/// a common prefix to all keys.
///
/// ## Connector Metadata
///
/// Variants can have a `#[link_address = "..."]` attribute to associate a connector
/// URL/address with the key (MQTT topics, KNX group addresses, etc.):
///
/// # Example
///
/// ```rust,ignore
/// #[derive(RecordKey, Clone, Copy, PartialEq, Eq, Hash)]
/// #[key_prefix = "sensors."]
/// pub enum SensorKey {
///     #[key = "temp"]           // → "sensors.temp"
///     #[link_address = "mqtt://sensors/temp/indoor"]
///     Temperature,
///
///     #[key = "humid"]          // → "sensors.humid"
///     #[link_address = "knx://9/0/1"]
///     Humidity,
/// }
/// ```
///
/// # Generated Code
///
/// The macro generates:
/// - `impl aimdb_core::RecordKey for YourEnum` with `as_str()` and `link_address()` methods
/// - `impl core::borrow::Borrow<str> for YourEnum` for O(1) HashMap lookups
#[proc_macro_derive(RecordKey, attributes(key, key_prefix, link_address))]
pub fn derive_record_key(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    match derive_record_key_impl(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn derive_record_key_impl(input: DeriveInput) -> Result<proc_macro2::TokenStream, Error> {
    let name = &input.ident;

    // Check it's an enum
    let data_enum = match &input.data {
        Data::Enum(data) => data,
        _ => {
            return Err(Error::new_spanned(
                &input,
                "RecordKey can only be derived for enums",
            ));
        }
    };

    // Get optional key_prefix from enum attributes
    let prefix = get_key_prefix(&input.attrs)?;

    // Collect variant data
    struct VariantData {
        name: syn::Ident,
        key: String,
        link: Option<String>,
    }

    let mut variants = Vec::new();
    let mut seen_keys = std::collections::HashSet::new();

    for variant in &data_enum.variants {
        // Ensure unit variant (no fields)
        match &variant.fields {
            Fields::Unit => {}
            _ => {
                return Err(Error::new_spanned(
                    variant,
                    "RecordKey variants must be unit variants (no fields)",
                ));
            }
        }

        let variant_name = &variant.ident;
        let key = get_variant_key(&variant.attrs, variant_name)?;
        let link = get_optional_attr(&variant.attrs, "link_address")?;

        // Build full key with prefix
        let full_key = if let Some(ref p) = prefix {
            format!("{}{}", p, key)
        } else {
            key
        };

        // Check for duplicate keys
        if !seen_keys.insert(full_key.clone()) {
            return Err(Error::new_spanned(
                variant,
                format!(
                    "Duplicate key \"{}\" - each variant must have a unique key",
                    full_key
                ),
            ));
        }

        variants.push(VariantData {
            name: variant_name.clone(),
            key: full_key,
            link,
        });
    }

    // Check if any variant has link
    let has_link = variants.iter().any(|v| v.link.is_some());

    // Generate match arms for as_str()
    let as_str_arms = variants.iter().map(|v| {
        let variant_name = &v.name;
        let key = &v.key;
        quote! {
            Self::#variant_name => #key
        }
    });

    // Generate link_address() implementation if any variant has it
    let link_impl = if has_link {
        let arms = variants.iter().map(|v| {
            let variant_name = &v.name;
            match &v.link {
                Some(url) => quote! { Self::#variant_name => Some(#url) },
                None => quote! { Self::#variant_name => None },
            }
        });
        quote! {
            #[inline]
            fn link_address(&self) -> Option<&str> {
                match self {
                    #(#arms),*
                }
            }
        }
    } else {
        quote! {}
    };

    // Generate the implementations
    let expanded = quote! {
        impl aimdb_core::RecordKey for #name {
            #[inline]
            fn as_str(&self) -> &str {
                match self {
                    #(#as_str_arms),*
                }
            }

            #link_impl
        }

        impl core::borrow::Borrow<str> for #name {
            #[inline]
            fn borrow(&self) -> &str {
                <Self as aimdb_core::RecordKey>::as_str(self)
            }
        }
    };

    Ok(expanded)
}

/// Extract `#[key_prefix = "..."]` from enum attributes
fn get_key_prefix(attrs: &[syn::Attribute]) -> Result<Option<String>, Error> {
    get_optional_attr(attrs, "key_prefix")
}

/// Extract an optional `#[attr_name = "..."]` from attributes
fn get_optional_attr(attrs: &[syn::Attribute], attr_name: &str) -> Result<Option<String>, Error> {
    for attr in attrs {
        if attr.path().is_ident(attr_name) {
            let meta = &attr.meta;
            if let Meta::NameValue(nv) = meta {
                if let syn::Expr::Lit(expr_lit) = &nv.value {
                    if let Lit::Str(lit_str) = &expr_lit.lit {
                        return Ok(Some(lit_str.value()));
                    }
                }
            }
            return Err(Error::new_spanned(
                attr,
                format!("Expected #[{} = \"...\"]", attr_name),
            ));
        }
    }
    Ok(None)
}

/// Extract `#[key = "..."]` from variant attributes
fn get_variant_key(attrs: &[syn::Attribute], variant_name: &syn::Ident) -> Result<String, Error> {
    for attr in attrs {
        if attr.path().is_ident("key") {
            let meta = &attr.meta;
            if let Meta::NameValue(nv) = meta {
                if let syn::Expr::Lit(expr_lit) = &nv.value {
                    if let Lit::Str(lit_str) = &expr_lit.lit {
                        return Ok(lit_str.value());
                    }
                }
            }
            return Err(Error::new_spanned(attr, "Expected #[key = \"...\"]"));
        }
    }

    Err(Error::new_spanned(
        variant_name,
        format!(
            "Missing #[key = \"...\"] attribute on variant `{}`",
            variant_name
        ),
    ))
}

#[cfg(test)]
mod tests {
    // Proc-macro crate tests are limited - integration tests are in aimdb-core
}
