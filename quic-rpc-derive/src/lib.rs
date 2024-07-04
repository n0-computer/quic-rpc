use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote, ToTokens};
use std::{collections::HashMap, fmt::Debug};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input,
    spanned::Spanned,
    Attribute, Data, DeriveInput, Fields, Ident, Lit, Meta, NestedMeta, Token, Type,
};

struct RpcArgs {
    types: HashMap<String, Type>,
}

impl Debug for RpcArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcInfo")
            .field(
                "types",
                &self
                    .types
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_token_stream().to_string()))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}
impl Parse for RpcArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut types = HashMap::new();

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

        Ok(RpcArgs { types })
    }
}

fn parse_rpc_attr(attr: &Attribute) -> syn::Result<RpcArgs> {
    attr.parse_args()
}

fn generate_rpc_impls(
    ident: &Ident,
    rpc_info: &RpcArgs,
    service_name: &Ident,
    request_type: &Type,
) -> syn::Result<Vec<proc_macro2::TokenStream>> {
    let pattern = ident;
    let pattern_ident = format_ident!("{}", pattern);

    let mut impls = vec![];

    let specific_impl = match pattern.to_string().as_str() {
        "rpc" => {
            let response_type = rpc_info.types.get("response").ok_or_else(|| {
                syn::Error::new(Span::call_site(), "Unary RPC requires a response type")
            })?;
            quote! {
                impl ::quic_rpc::pattern::rpc::RpcMsg<#service_name> for #request_type {
                    type Response = #response_type;
                }
            }
        }
        "server_streaming" => {
            let response_type = rpc_info.types.get("response").ok_or_else(|| {
                syn::Error::new(
                    Span::call_site(),
                    "Server streaming RPC requires an update type",
                )
            })?;
            quote! {
                impl ::quic_rpc::message::Msg<#service_name> for #request_type {
                    type Pattern = ::quic_rpc::pattern::server_streaming::ServerStreaming;
                }
                impl ::quic_rpc::pattern::server_streaming::ServerStreamingMsg<#service_name> for #request_type {
                    type Response = #response_type;
                }
            }
        }
        "bidi_streaming" => {
            let update_type = rpc_info.types.get("update").ok_or_else(|| {
                syn::Error::new(
                    Span::call_site(),
                    "Bidi streaming RPC requires an update type",
                )
            })?;
            let response_type = rpc_info.types.get("response").ok_or_else(|| {
                syn::Error::new(
                    Span::call_site(),
                    "Bidi streaming RPC requires a response type",
                )
            })?;
            quote! {
                impl ::quic_rpc::message::Msg<#service_name> for #request_type {
                    type Pattern = ::quic_rpc::pattern::bidi_streaming::BidiStreaming;
                }
                impl ::quic_rpc::pattern::bidi_streaming::BidiStreamingMsg<#service_name> for #request_type {
                    type Update = #update_type;
                    type Response = #response_type;
                }
            }
        }
        "client_streaming" => {
            let update_type = rpc_info.types.get("update").ok_or_else(|| {
                syn::Error::new(
                    Span::call_site(),
                    "Client streaming RPC requires an update type",
                )
            })?;
            let response_type = rpc_info.types.get("response").ok_or_else(|| {
                syn::Error::new(
                    Span::call_site(),
                    "Client streaming RPC requires a response type",
                )
            })?;
            quote! {
                impl ::quic_rpc::message::Msg<#service_name> for #request_type {
                    type Pattern = ::quic_rpc::pattern::client_streaming::ClientStreaming;
                }
                impl ::quic_rpc::pattern::client_streaming::ClientStreamingMsg<#service_name> for #request_type {
                    type Update = #update_type;
                    type Response = #response_type;
                }
            }
        }
        _ => return Err(syn::Error::new(Span::call_site(), "Unknown RPC pattern")),
    };

    impls.push(specific_impl);
    Ok(impls)
}

#[proc_macro_attribute]
pub fn rpc_requests(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as DeriveInput);
    let service_name = parse_macro_input!(attr as Ident);

    let data_enum = match &mut input.data {
        Data::Enum(data_enum) => data_enum,
        _ => {
            return syn::Error::new(
                Span::call_site(),
                "RpcRequests can only be applied to enums",
            )
            .to_compile_error()
            .into()
        }
    };

    let mut additional_items = Vec::new();

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

        let mut rpc_attr = None;
        variant.attrs.retain(|attr| {
            if attr.path.is_ident("rpc")
                || attr.path.is_ident("bidi_streaming")
                || attr.path.is_ident("server_streaming")
                || attr.path.is_ident("client_streaming")
            {
                rpc_attr = Some(attr.clone());
                false
            } else {
                true
            }
        });

        if let Some(attr) = rpc_attr {
            let ident = attr.path.get_ident().unwrap();
            println!("{}", attr.to_token_stream());
            let rpc_info = match parse_rpc_attr(&attr) {
                Ok(info) => info,
                Err(e) => return e.to_compile_error().into(),
            };
            println!("{:?}", rpc_info);

            match generate_rpc_impls(ident, &rpc_info, &service_name, request_type) {
                Ok(impls) => additional_items.extend(impls),
                Err(e) => return e.to_compile_error().into(),
            }
        }
    }

    let output = quote! {
        #input

        #(#additional_items)*
    };

    println!("{}", output);

    output.into()
}
