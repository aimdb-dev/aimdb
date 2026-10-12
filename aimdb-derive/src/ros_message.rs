//! `#[derive(RosMessage)]`: `SchemaType`, a CDR `Linkable` and `RosMessage`.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{Data, DeriveInput, Error, Fields, LitInt, LitStr, Result};

const DEFAULT_ENCODE_CAPACITY: usize = 256;

struct Container {
    ros_type: LitStr,
    dds_name: String,
    hash: LitStr,
    encode_capacity: usize,
}

pub fn derive(input: DeriveInput) -> Result<TokenStream> {
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(Error::new_spanned(
            &input.generics,
            "RosMessage cannot be derived for a generic type",
        ));
    }
    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) if !fields.named.is_empty() => &fields.named,
            _ => {
                return Err(Error::new_spanned(
                    name,
                    "RosMessage needs a struct with named fields; for an empty message, \
                     add `structure_needs_at_least_one_member: u8` as ROS does",
                ))
            }
        },
        _ => {
            return Err(Error::new_spanned(
                name,
                "RosMessage can only be derived for a struct",
            ))
        }
    };

    let container = parse_container(&input)?;
    let mut bound_checks = Vec::new();
    for field in fields {
        let (Some(max_len), Some(ident)) = (parse_max_len(&field.attrs)?, &field.ident) else {
            continue;
        };
        let field_name = ident.to_string();
        bound_checks.push(quote! {
            if self.#ident.len() > #max_len {
                return ::core::result::Result::Err(#field_name);
            }
        });
    }

    let Container {
        ros_type,
        dds_name,
        hash,
        encode_capacity,
    } = container;
    let dc = quote!(::aimdb_data_contracts);
    let p = quote!(::aimdb_data_contracts::__private);

    Ok(quote! {
        impl #dc::SchemaType for #name {
            const NAME: &'static str = #ros_type;
        }

        impl #name {
            /// The first field whose `#[ros(max_len)]` bound the value exceeds.
            #[doc(hidden)]
            fn __aimdb_ros_check_bounds(&self) -> ::core::result::Result<(), &'static str> {
                #(#bound_checks)*
                ::core::result::Result::Ok(())
            }
        }

        impl #dc::Linkable for #name {
            const ENCODE_BUFFER_CAPACITY: ::core::option::Option<usize> =
                ::core::option::Option::Some(#encode_capacity);
            const WIRE_FORMAT: #dc::WireFormat = #dc::WireFormat::Cdr;

            fn from_bytes(data: &[u8]) -> ::core::result::Result<Self, #p::alloc::string::String> {
                #p::aimdb_cdr::from_bytes(data)
                    .map_err(|e| #p::alloc::string::ToString::to_string(&e))
            }

            fn to_bytes(
                &self,
            ) -> ::core::result::Result<#p::alloc::vec::Vec<u8>, #p::alloc::string::String> {
                self.__aimdb_ros_check_bounds().map_err(|field| {
                    #p::alloc::format!("field `{}` exceeds its #[ros(max_len)] bound", field)
                })?;
                #p::aimdb_cdr::to_vec(self).map_err(|e| #p::alloc::string::ToString::to_string(&e))
            }

            fn encode_into(
                &self,
                buf: &mut [u8],
            ) -> ::core::result::Result<usize, #p::SerializeError> {
                self.__aimdb_ros_check_bounds()
                    .map_err(|_| #p::SerializeError::InvalidData)?;
                match #p::aimdb_cdr::to_slice(self, buf) {
                    ::core::result::Result::Ok(used) => ::core::result::Result::Ok(used),
                    ::core::result::Result::Err(#p::aimdb_cdr::Error::BufferTooSmall) => {
                        ::core::result::Result::Err(#p::SerializeError::BufferTooSmall)
                    }
                    ::core::result::Result::Err(_) => {
                        ::core::result::Result::Err(#p::SerializeError::InvalidData)
                    }
                }
            }
        }

        impl #dc::RosMessage for #name {
            const ROS_TYPE_NAME: &'static str = #dds_name;
            const ROS_TYPE_HASH: &'static str = #hash;
        }
    })
}

fn parse_container(input: &DeriveInput) -> Result<Container> {
    let mut ros_type: Option<LitStr> = None;
    let mut hash: Option<LitStr> = None;
    let mut encode_capacity = DEFAULT_ENCODE_CAPACITY;

    for attr in input.attrs.iter().filter(|a| a.path().is_ident("ros")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("type") {
                ros_type = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("hash") {
                hash = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("encode_capacity") {
                let lit: LitInt = meta.value()?.parse()?;
                encode_capacity = lit.base10_parse()?;
                if encode_capacity == 0 {
                    return Err(Error::new_spanned(
                        lit,
                        "encode_capacity must be at least 1",
                    ));
                }
            } else {
                return Err(meta.error("expected `type`, `hash` or `encode_capacity`"));
            }
            Ok(())
        })?;
    }

    let missing = |what: &str| {
        Error::new(
            Span::call_site(),
            format!("RosMessage needs #[ros({what} = \"…\")]"),
        )
    };
    let ros_type = ros_type.ok_or_else(|| missing("type"))?;
    let hash = hash.ok_or_else(|| missing("hash"))?;

    let dds_name =
        dds_type_name(&ros_type.value()).map_err(|why| Error::new_spanned(&ros_type, why))?;
    check_hash(&hash.value()).map_err(|why| Error::new_spanned(&hash, why))?;

    Ok(Container {
        ros_type,
        dds_name,
        hash,
        encode_capacity,
    })
}

fn parse_max_len(attrs: &[syn::Attribute]) -> Result<Option<usize>> {
    let mut max_len = None;
    for attr in attrs.iter().filter(|a| a.path().is_ident("ros")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("max_len") {
                let lit: LitInt = meta.value()?.parse()?;
                max_len = Some(lit.base10_parse()?);
                Ok(())
            } else {
                Err(meta.error("expected `max_len` on a field"))
            }
        })?;
    }
    Ok(max_len)
}

// The rules match `aimdb_data_contracts::ros2`, which this crate cannot
// depend on; the data-contracts tests check that both agree.

fn dds_type_name(ros_type: &str) -> core::result::Result<String, &'static str> {
    let mut parts = ros_type.split('/');
    let (Some(package), Some("msg"), Some(message), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("expected a ROS message type `pkg/msg/Name`");
    };
    let pb = package.as_bytes();
    let package_ok = pb.first().is_some_and(u8::is_ascii_lowercase)
        && pb.last() != Some(&b'_')
        && !package.contains("__")
        && pb
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_');
    if !package_ok {
        return Err(
            "package must be lowercase letters, digits and single underscores, \
                    starting with a letter and not ending with '_'",
        );
    }
    let mb = message.as_bytes();
    if !(mb.first().is_some_and(u8::is_ascii_uppercase) && mb.iter().all(u8::is_ascii_alphanumeric))
    {
        return Err("message name must start with an uppercase letter, then letters and digits");
    }
    Ok(format!("{package}::msg::dds_::{message}_"))
}

fn check_hash(hash: &str) -> core::result::Result<(), &'static str> {
    match hash.strip_prefix("RIHS01_") {
        Some(digits)
            if digits.len() == 64
                && digits
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
        {
            Ok(())
        }
        _ => Err("type hash must be 'RIHS01_' followed by 64 lowercase hex digits"),
    }
}
