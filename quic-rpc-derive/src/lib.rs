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

// Helper function for error reporting
fn error_tokens(span: Span, message: &str) -> TokenStream {
    syn::Error::new(span, message)
        .to_compile_error()
        .into()
}

fn generate_channels_impl(
    mut args: NamedTypeArgs,
    service_name: &Ident,
    request_type: &Type,
    attr_span: Span,
) -> syn::Result<TokenStream2> {
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
    let tx = args.get("tx", attr_span)?;
    
    let res = quote! {
        impl ::quic_rpc::Channels<#service_name> for #request_type {
            type Tx = #tx;
            type Rx = #rx;
        }
    };
    
    args.check_empty(attr_span)?;
    Ok(res)
}

// Parse arguments in the format (ServiceType, MessageEnumName)
struct MacroArgs {
    service_name: Ident,
    message_enum_name: Ident,
}

impl Parse for MacroArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let service_name: Ident = input.parse()?;
        let _: Token![,] = input.parse()?;
        let message_enum_name: Ident = input.parse()?;

        Ok(MacroArgs {
            service_name,
            message_enum_name,
        })
    }
}

#[proc_macro_attribute]
pub fn rpc_requests(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as DeriveInput);
    let MacroArgs {
        service_name,
        message_enum_name,
    } = parse_macro_input!(attr as MacroArgs);

    let input_span = input.span();
    let data_enum = match &mut input.data {
        Data::Enum(data_enum) => data_enum,
        _ => return error_tokens(input.span(), "RpcRequests can only be applied to enums"),
    };

    let mut additional_items = Vec::new();
    let mut types = HashSet::new();

    for variant in &mut data_enum.variants {
        // Check field structure for every variant
        let request_type = match &variant.fields {
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => &fields.unnamed[0].ty,
            _ => return error_tokens(
                variant.span(),
                "Each variant must have exactly one unnamed field",
            ),
        };

        if !types.insert(request_type.to_token_stream().to_string()) {
            return error_tokens(input_span, "Each variant must have a unique request type");
        }

        // Find and remove the rpc attribute
        let mut rpc_attr = None;
        variant.attrs.retain(|attr| {
            if attr.path.is_ident("rpc") {
                if rpc_attr.is_some() {
                    // This should never happen since we're removing them as we go
                    panic!("Multiple rpc attributes found");
                }
                rpc_attr = Some(attr.clone());
                false // Remove this attribute
            } else {
                true // Keep other attributes
            }
        });

        if let Some(attr) = rpc_attr {
            let args = match attr.parse_args::<NamedTypeArgs>() {
                Ok(info) => info,
                Err(e) => return e.to_compile_error().into(),
            };

            match generate_channels_impl(args, &service_name, request_type, attr.span()) {
                Ok(impls) => additional_items.extend(impls),
                Err(e) => return e.to_compile_error().into(),
            }
        }
    }

    let message_variants = data_enum
        .variants
        .iter()
        .map(|variant| {
            let variant_name = &variant.ident;

            // Extract the inner type - we know it's already valid
            let inner_type = match &variant.fields {
                Fields::Unnamed(fields) => {
                    if let Type::Path(type_path) = &fields.unnamed.first().unwrap().ty {
                        if let Some(last_segment) = type_path.path.segments.last() {
                            &last_segment.ident
                        } else {
                            &type_path.path.segments.first().unwrap().ident
                        }
                    } else {
                        panic!("Unexpected type"); // Should never happen due to prior validation
                    }
                }
                _ => panic!("Unexpected field type"), // Should never happen due to prior validation
            };

            quote! {
                #variant_name(::quic_rpc::WithChannels<#inner_type, #service_name>)
            }
        })
        .collect::<Vec<_>>();

    let message_enum = quote! {
        #[derive(derive_more::From)]
        enum #message_enum_name {
            #(#message_variants),*
        }
    };

    let output = quote! {
        #input

        #message_enum

        #(#additional_items)*
    };

    output.into()
}

struct NamedTypeArgs {
    types: BTreeMap<String, Type>,
}

impl NamedTypeArgs {
    /// Get and remove a type from the map, failing if it doesn't exist
    fn get(&mut self, key: &str, span: Span) -> syn::Result<Type> {
        self.types
            .remove(key)
            .ok_or_else(|| syn::Error::new(span, format!("rpc requires a {key} type")))
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