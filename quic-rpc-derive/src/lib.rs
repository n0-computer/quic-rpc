use proc_macro::TokenStream;
use quote::quote;
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input,
    spanned::Spanned,
    Data, DeriveInput, Fields, Ident, Token, Type,
};

struct TxArgs {
    tx_type: Type,
}

impl Parse for TxArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let tx_type: Type = input.parse()?;
        Ok(TxArgs { tx_type })
    }
}

struct RxArgs {
    rx_type: Type,
}

impl Parse for RxArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let rx_type: Type = input.parse()?;
        Ok(RxArgs { rx_type })
    }
}

/// A macro to generate a Message enum from a Protocol enum.
///
/// This macro takes a Protocol enum and generates a corresponding Message enum
/// where each variant is wrapped in a WithChannels type.
///
/// # Example
///
/// ```
/// #[message_enum(StorageMessage, StorageService)]
/// #[derive(derive_more::From, Serialize, Deserialize)]
/// enum StorageProtocol {
///     Get(Get),
///     Set(Set),
///     List(List),
/// }
/// ```
///
/// Will generate:
///
/// ```
/// #[derive(derive_more::From)]
/// enum StorageMessage {
///     Get(WithChannels<Get, StorageService>),
///     Set(WithChannels<Set, StorageService>),
///     List(WithChannels<List, StorageService>),
/// }
/// ```
#[proc_macro_attribute]
pub fn message_enum(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Parse the input
    let input = parse_macro_input!(item as DeriveInput);
    let attrs = parse_macro_input!(attr as syn::AttributeArgs);

    // Parse the attribute arguments
    if attrs.len() != 2 {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "Expected two arguments: message_enum_name and service_type",
        )
        .to_compile_error()
        .into();
    }

    // Extract message enum name and service type from attributes
    let message_enum_name = match &attrs[0] {
        syn::NestedMeta::Meta(syn::Meta::Path(path)) => path.get_ident().unwrap(),
        _ => {
            return syn::Error::new(
                proc_macro2::Span::call_site(),
                "First argument must be an identifier for the message enum name",
            )
            .to_compile_error()
            .into();
        }
    };

    let service_type = match &attrs[1] {
        syn::NestedMeta::Meta(syn::Meta::Path(path)) => path.get_ident().unwrap(),
        _ => {
            return syn::Error::new(
                proc_macro2::Span::call_site(),
                "Second argument must be an identifier for the service type",
            )
            .to_compile_error()
            .into();
        }
    };

    // Extract the variants from the enum
    let data_enum = match &input.data {
        Data::Enum(data_enum) => data_enum,
        _ => {
            return syn::Error::new(input.ident.span(), "This macro only works on enums")
                .to_compile_error()
                .into();
        }
    };

    // Generate variants for the message enum
    let message_variants = data_enum.variants.iter().map(|variant| {
        let variant_name = &variant.ident;

        // Extract the inner type from the variant
        let inner_type = match &variant.fields {
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                if let Type::Path(type_path) = &fields.unnamed.first().unwrap().ty {
                    if let Some(last_segment) = type_path.path.segments.last() {
                        &last_segment.ident
                    } else {
                        return syn::Error::new(
                            variant.ident.span(),
                            "Unable to extract type name from variant",
                        )
                        .to_compile_error();
                    }
                } else {
                    return syn::Error::new(
                        variant.ident.span(),
                        "Variant must contain exactly one unnamed field of a simple type",
                    )
                    .to_compile_error();
                }
            }
            _ => {
                return syn::Error::new(
                    variant.ident.span(),
                    "Variant must contain exactly one unnamed field",
                )
                .to_compile_error();
            }
        };

        // Generate the message variant
        quote! {
            #variant_name(WithChannels<#inner_type, #service_type>)
        }
    });

    // Generate the message enum
    let expanded = quote! {
        // Keep the original protocol enum
        #input

        // Generate the message enum
        #[derive(derive_more::From)]
        enum #message_enum_name {
            #(#message_variants),*
        }
    };

    expanded.into()
}

/// A macro to generate a Message enum from a Protocol enum and implement Channels for each variant.
/// 
/// This macro:
/// 1. Takes a Protocol enum and generates a corresponding Message enum with variants wrapped in WithChannels
/// 2. Implements the Channels trait for each variant's inner type based on #[tx(...)] and #[rx(...)] attributes
///
/// Channel receiver (rx) defaults to NoReceiver if not specified.
///
/// # Example
///
/// ```
/// #[rpc_protocol(StorageMessage, StorageService)]
/// enum StorageProtocol {
///     #[tx(oneshot::Sender<Option<String>>)]
///     Get(Get),
///     #[tx(oneshot::Sender<()>)]
///     Set(Set),
///     #[tx(mpsc::Sender<String>)]
///     List(List),
///     #[rx(mpsc::Receiver<Query>)]
///     #[tx(mpsc::Sender<Result>)]
///     Query(Query),
/// }
/// ```
#[proc_macro_attribute]
pub fn rpc_protocol(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Parse the input
    let mut input = parse_macro_input!(item as DeriveInput);
    let attr_args = parse_macro_input!(attr as syn::AttributeArgs);
    
    // Extract message enum name and service type from attributes
    if attr_args.len() != 2 {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "Expected two arguments: message_enum_name and service_type",
        )
        .to_compile_error()
        .into();
    }
    
    let message_enum_name = match &attr_args[0] {
        syn::NestedMeta::Meta(syn::Meta::Path(path)) => {
            if let Some(ident) = path.get_ident() {
                ident
            } else {
                return syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "First argument must be an identifier for the message enum name",
                ).to_compile_error().into();
            }
        },
        _ => {
            return syn::Error::new(
                proc_macro2::Span::call_site(),
                "First argument must be an identifier for the message enum name",
            ).to_compile_error().into();
        }
    };
    
    let service_type = match &attr_args[1] {
        syn::NestedMeta::Meta(syn::Meta::Path(path)) => {
            if let Some(ident) = path.get_ident() {
                ident
            } else {
                return syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "Second argument must be an identifier for the service type",
                ).to_compile_error().into();
            }
        },
        _ => {
            return syn::Error::new(
                proc_macro2::Span::call_site(),
                "Second argument must be an identifier for the service type",
            ).to_compile_error().into();
        }
    };
    
    // Extract the variants from the enum
    let data_enum = match &mut input.data {
        Data::Enum(data_enum) => data_enum,
        _ => {
            return syn::Error::new(
                input.ident.span(),
                "This macro only works on enums",
            ).to_compile_error().into();
        }
    };
    
    // Generate variants for the message enum
    let message_variants = data_enum.variants.iter().map(|variant| {
        let variant_name = &variant.ident;
        
        // Extract the inner type from the variant
        let inner_type = match &variant.fields {
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                if let Type::Path(type_path) = &fields.unnamed.first().unwrap().ty {
                    if let Some(last_segment) = type_path.path.segments.last() {
                        &last_segment.ident
                    } else {
                        return syn::Error::new(
                            variant.ident.span(),
                            "Unable to extract type name from variant",
                        ).to_compile_error();
                    }
                } else {
                    return syn::Error::new(
                        variant.ident.span(),
                        "Variant must contain exactly one unnamed field of a simple type",
                    ).to_compile_error();
                }
            },
            _ => {
                return syn::Error::new(
                    variant.ident.span(),
                    "Variant must contain exactly one unnamed field",
                ).to_compile_error();
            }
        };
        
        quote! {
            #variant_name(WithChannels<#inner_type, #service_type>)
        }
    });
    
    // Storage for additional implementations
    let mut channel_impls = Vec::new();
    
    // Process each variant
    for variant in &mut data_enum.variants {
        // Extract the inner type from the variant
        let inner_type = match &variant.fields {
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                if let Type::Path(type_path) = &fields.unnamed.first().unwrap().ty {
                    if let Some(last_segment) = type_path.path.segments.last() {
                        last_segment.ident.clone()
                    } else {
                        return syn::Error::new(
                            variant.ident.span(),
                            "Unable to extract type name from variant",
                        ).to_compile_error().into();
                    }
                } else {
                    return syn::Error::new(
                        variant.ident.span(),
                        "Variant must contain exactly one unnamed field of a simple type",
                    ).to_compile_error().into();
                }
            },
            _ => {
                return syn::Error::new(
                    variant.ident.span(),
                    "Variant must contain exactly one unnamed field",
                ).to_compile_error().into();
            }
        };
        
        // Default rx to NoReceiver
        let mut rx_type = quote! { NoReceiver };
        let mut tx_type = None;
        
        // Extract and remove rx attributes
        let mut rx_attr = None;
        variant.attrs.retain(|attr| {
            if attr.path.is_ident("rx") {
                rx_attr = Some(attr.clone());
                false
            } else {
                true
            }
        });
        
        // Extract and remove tx attributes
        let mut tx_attr = None;
        variant.attrs.retain(|attr| {
            if attr.path.is_ident("tx") {
                tx_attr = Some(attr.clone());
                false
            } else {
                true
            }
        });
        
        // Parse rx attribute if found
        if let Some(attr) = rx_attr {
            match attr.parse_args::<RxArgs>() {
                Ok(args) => {
                    rx_type = quote! { #args.rx_type };
                },
                Err(e) => return e.to_compile_error().into(),
            }
        }
        
        // Parse tx attribute - required
        let tx_attr = match tx_attr {
            Some(attr) => attr,
            None => {
                return syn::Error::new(
                    variant.ident.span(),
                    format!("Missing #[tx(...)] attribute for variant {}", variant.ident),
                ).to_compile_error().into();
            }
        };
        
        match tx_attr.parse_args::<TxArgs>() {
            Ok(args) => {
                tx_type = Some(quote! { #args.tx_type });
            },
            Err(e) => return e.to_compile_error().into(),
        }
        
        // Generate the Channel implementation
        let tx_type = tx_type.unwrap();
        channel_impls.push(quote! {
            impl Channels<#service_type> for #inner_type {
                type Rx = #rx_type;
                type Tx = #tx_type;
            }
        });
    }
    
    // Generate the complete output
    let expanded = quote! {
        // Keep the original protocol enum
        #input
        
        // Generate the message enum
        #[derive(derive_more::From)]
        enum #message_enum_name {
            #(#message_variants),*
        }
        
        // Implement Channels for each type
        #(#channel_impls)*
    };
    
    expanded.into()
}