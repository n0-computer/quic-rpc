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
    syn::Error::new(span, message).to_compile_error().into()
}

/// The only attribute we care about
const ATTR_NAME: &str = "rpc";
/// the tx type name
const TX_ATTR: &str = "tx";
/// the rx type name
const RX_ATTR: &str = "rx";
/// Fully qualified path to the default rx type
const DEFAULT_RX_TYPE: &str = "::quic_rpc::channel::none::NoReceiver";

fn generate_channels_impl(
    mut args: NamedTypeArgs,
    service_name: &Ident,
    request_type: &Type,
    attr_span: Span,
) -> syn::Result<TokenStream2> {
    // Try to get rx, default to NoReceiver if not present
    // Use unwrap_or_else for a cleaner default
    let rx = args.types.remove(RX_ATTR).unwrap_or_else(|| {
        // We can safely unwrap here because this is a known valid type
        syn::parse_str::<Type>(DEFAULT_RX_TYPE).expect("Failed to parse default rx type")
    });
    let tx = args.get(TX_ATTR, attr_span)?;

    let res = quote! {
        impl ::quic_rpc::Channels<#service_name> for #request_type {
            type Tx = #tx;
            type Rx = #rx;
        }
    };

    args.check_empty(attr_span)?;
    Ok(res)
}
fn generate_from_impls(
    message_enum_name: &Ident,
    variants: &[(Ident, Type)],
    service_name: &Ident,
    original_enum_name: &Ident,
    additional_items: &mut Vec<proc_macro2::TokenTree>,
) {
    // Generate and add From impls for the message enum
    for (variant_name, inner_type) in variants {
        let message_impl = quote! {
            impl From<::quic_rpc::WithChannels<#inner_type, #service_name>> for #message_enum_name {
                fn from(value: ::quic_rpc::WithChannels<#inner_type, #service_name>) -> Self {
                    #message_enum_name::#variant_name(value)
                }
            }
        };
        additional_items.extend(message_impl);

        // Generate and add From impls for the original enum
        let original_impl = quote! {
            impl From<#inner_type> for #original_enum_name {
                fn from(value: #inner_type) -> Self {
                    #original_enum_name::#variant_name(value)
                }
            }
        };
        additional_items.extend(original_impl);
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

    // builder for the trait impls
    let mut additional_items = Vec::new();
    // types to check for uniqueness
    let mut types = HashSet::new();
    // variant names and types
    let mut variants = Vec::new();

    for variant in &mut data_enum.variants {
        // Check field structure for every variant
        let request_type = match &variant.fields {
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => &fields.unnamed[0].ty,
            _ => {
                return error_tokens(
                    variant.span(),
                    "Each variant must have exactly one unnamed field",
                )
            }
        };
        variants.push((variant.ident.clone(), request_type.clone()));

        if !types.insert(request_type.to_token_stream().to_string()) {
            return error_tokens(input_span, "Each variant must have a unique request type");
        }
        // Find and remove the rpc attribute
        let mut rpc_attr = None;
        let mut multiple_rpc_attrs = false;

        variant.attrs.retain(|attr| {
            if attr.path.is_ident(ATTR_NAME) {
                if rpc_attr.is_some() {
                    multiple_rpc_attrs = true;
                    true // Keep this duplicate attribute
                } else {
                    rpc_attr = Some(attr.clone());
                    false // Remove this attribute
                }
            } else {
                true // Keep other attributes
            }
        });

        // Check for multiple rpc attributes
        if multiple_rpc_attrs {
            return error_tokens(
                variant.span(),
                "Each variant can only have one rpc attribute",
            );
        }

        // if there is no attr, the user has to impl Channels manually
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

    let message_variants = variants
        .iter()
        .map(|(variant_name, inner_type)| {
            quote! {
                #variant_name(::quic_rpc::WithChannels<#inner_type, #service_name>)
            }
        })
        .collect::<Vec<_>>();

    let message_enum = quote! {
        enum #message_enum_name {
            #(#message_variants),*
        }
    };

    // Generate the From implementations
    generate_from_impls(
        &message_enum_name,
        &variants,
        &service_name,
        &input.ident,
        &mut additional_items,
    );

    let output = quote! {
        #input

        #message_enum

        #(#additional_items)*
    };

    output.into()
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
