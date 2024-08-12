<<<<<<< HEAD
use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{quote, ToTokens};
use std::collections::{BTreeMap, HashSet};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input,
    spanned::Spanned,
    Data, DeriveInput, Fields, Ident, Token, Type,
};

const SERVER_STREAMING: &str = "server_streaming";
const CLIENT_STREAMING: &str = "client_streaming";
const BIDI_STREAMING: &str = "bidi_streaming";
const RPC: &str = "rpc";
const TRY_SERVER_STREAMING: &str = "try_server_streaming";
const IDENTS: [&str; 5] = [
    SERVER_STREAMING,
    CLIENT_STREAMING,
    BIDI_STREAMING,
    RPC,
    TRY_SERVER_STREAMING,
];

fn generate_rpc_impls(
    pat: &str,
    mut args: RpcArgs,
    service_name: &Ident,
    request_type: &Type,
    attr_span: Span,
) -> syn::Result<TokenStream2> {
    let res = match pat {
        RPC => {
            let response = args.get("response", pat, attr_span)?;
            quote! {
                impl ::quic_rpc::pattern::rpc::RpcMsg<#service_name> for #request_type {
                    type Response = #response;
                }
            }
        }
        SERVER_STREAMING => {
            let response = args.get("response", pat, attr_span)?;
            quote! {
                impl ::quic_rpc::message::Msg<#service_name> for #request_type {
                    type Pattern = ::quic_rpc::pattern::server_streaming::ServerStreaming;
                }
                impl ::quic_rpc::pattern::server_streaming::ServerStreamingMsg<#service_name> for #request_type {
                    type Response = #response;
                }
            }
        }
        BIDI_STREAMING => {
            let update = args.get("update", pat, attr_span)?;
            let response = args.get("response", pat, attr_span)?;
            quote! {
                impl ::quic_rpc::message::Msg<#service_name> for #request_type {
                    type Pattern = ::quic_rpc::pattern::bidi_streaming::BidiStreaming;
                }
                impl ::quic_rpc::pattern::bidi_streaming::BidiStreamingMsg<#service_name> for #request_type {
                    type Update = #update;
                    type Response = #response;
                }
            }
        }
        CLIENT_STREAMING => {
            let update = args.get("update", pat, attr_span)?;
            let response = args.get("response", pat, attr_span)?;
            quote! {
                impl ::quic_rpc::message::Msg<#service_name> for #request_type {
                    type Pattern = ::quic_rpc::pattern::client_streaming::ClientStreaming;
                }
                impl ::quic_rpc::pattern::client_streaming::ClientStreamingMsg<#service_name> for #request_type {
                    type Update = #update;
                    type Response = #response;
                }
            }
        }
        TRY_SERVER_STREAMING => {
            let create_error = args.get("create_error", pat, attr_span)?;
            let item_error = args.get("item_error", pat, attr_span)?;
            let item = args.get("item", pat, attr_span)?;
            quote! {
                impl ::quic_rpc::message::Msg<#service_name> for #request_type {
                    type Pattern = ::quic_rpc::pattern::try_server_streaming::TryServerStreaming;
                }
                impl ::quic_rpc::pattern::try_server_streaming::TryServerStreamingMsg<#service_name> for #request_type {
                    type CreateError = #create_error;
                    type ItemError = #item_error;
                    type Item = #item;
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
            for ident in IDENTS {
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
            let args = match attr.parse_args::<RpcArgs>() {
                Ok(info) => info,
                Err(e) => return e.to_compile_error().into(),
            };

            match generate_rpc_impls(ident, args, &service_name, request_type, attr.span()) {
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

struct RpcArgs {
    types: BTreeMap<String, Type>,
}

impl RpcArgs {
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
impl Parse for RpcArgs {
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

        Ok(RpcArgs { types })
    }
}
||||||| parent of 66d32e2 (deps: Upgrade to Quinn 0.11 and Rustls 0.23)
=======
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
>>>>>>> 66d32e2 (deps: Upgrade to Quinn 0.11 and Rustls 0.23)
