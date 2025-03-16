use std::collections::{BTreeMap, HashSet};

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{quote, ToTokens};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input,
    spanned::Spanned,
    Data, DeriveInput, Fields, Ident, Token, Type,
};

const RPC: &str = "rpc";
const ATTRS: [&str; 1] = [RPC];

fn generate_channels_impl(
    pat: &str,
    mut args: NamedTypeArgs,
    service_name: &Ident,
    request_type: &Type,
    attr_span: Span,
) -> syn::Result<TokenStream2> {
    let res = match pat {
        RPC => {
            // Try to get rx, default to NoReceiver if not present
            let rx = match args.types.remove("rx") {
                Some(rx_type) => rx_type,
                None => {
                    // Parse "NoReceiver" into a Type
                    syn::parse_str::<Type>("::quic_rpc::channel::none::NoReceiver").map_err(
                        |e| {
                            syn::Error::new(
                                attr_span,
                                format!("Failed to parse default rx type: {}", e),
                            )
                        },
                    )?
                }
            };
            let tx = args.get("tx", pat, attr_span)?;
            quote! {
                impl ::quic_rpc::Channels<#service_name> for #request_type {
                    type Tx = #tx;
                    type Rx = #rx;
                }
            }
        }
        _ => return Err(syn::Error::new(attr_span, "Unknown RPC pattern")),
    };
    args.check_empty(attr_span)?;

    Ok(res)
}

#[proc_macro_attribute]
pub fn rpc_requests(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as DeriveInput);
    let service_name = parse_macro_input!(attr as Ident);

    let input_span = input.span();
    let data_enum = match &mut input.data {
        Data::Enum(data_enum) => data_enum,
        _ => {
            return syn::Error::new(input.span(), "RpcRequests can only be applied to enums")
                .to_compile_error()
                .into()
        }
    };

    let mut additional_items = Vec::new();
    let mut types = HashSet::new();

    for variant in &mut data_enum.variants {
        // Check field structure for every variant
        let request_type = match &variant.fields {
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => &fields.unnamed[0].ty,
            _ => {
                return syn::Error::new(
                    variant.span(),
                    "Each variant must have exactly one unnamed field",
                )
                .to_compile_error()
                .into()
            }
        };

        if !types.insert(request_type.to_token_stream().to_string()) {
            return syn::Error::new(input_span, "Each variant must have a unique request type")
                .to_compile_error()
                .into();
        }

        // Extract and remove RPC attributes
        let mut rpc_attr = Vec::new();
        variant.attrs.retain(|attr| {
            for ident in ATTRS {
                if attr.path.is_ident(ident) {
                    rpc_attr.push((ident, attr.clone()));
                    return false;
                }
            }
            true
        });

        // Fail if there are multiple RPC patterns
        if rpc_attr.len() > 1 {
            return syn::Error::new(variant.span(), "Each variant can only have one RPC pattern")
                .to_compile_error()
                .into();
        }

        if let Some((ident, attr)) = rpc_attr.pop() {
            let args = match attr.parse_args::<NamedTypeArgs>() {
                Ok(info) => info,
                Err(e) => return e.to_compile_error().into(),
            };

            match generate_channels_impl(ident, args, &service_name, request_type, attr.span()) {
                Ok(impls) => additional_items.extend(impls),
                Err(e) => return e.to_compile_error().into(),
            }
        }
    }

    let output = quote! {
        #input

        #(#additional_items)*
    };

    output.into()
}

struct NamedTypeArgs {
    types: BTreeMap<String, Type>,
}

impl NamedTypeArgs {
    /// Get and remove a type from the map, failing if it doesn't exist
    fn get(&mut self, key: &str, kind: &str, span: Span) -> syn::Result<Type> {
        self.types
            .remove(key)
            .ok_or_else(|| syn::Error::new(span, format!("{kind} requires a {key} type")))
    }

    /// Fail if there are any unknown arguments remaining
    fn check_empty(&self, span: Span) -> syn::Result<()> {
        if self.types.is_empty() {
            Ok(())
        } else {
            Err(syn::Error::new(
                span,
                format!(
                    "Unknown arguments provided: {:?}",
                    self.types.keys().collect::<Vec<_>>()
                ),
            ))
        }
    }
}

/// Parse the rpc args as a comma separated list of name=type pairs
impl Parse for NamedTypeArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut types = BTreeMap::new();

        loop {
            if input.is_empty() {
                break;
            }

            let key: Ident = input.parse()?;
            let _: Token![=] = input.parse()?;
            let value: Type = input.parse()?;

            types.insert(key.to_string(), value);

            if !input.peek(Token![,]) {
                break;
            }
            let _: Token![,] = input.parse()?;
        }

        Ok(NamedTypeArgs { types })
    }
}
