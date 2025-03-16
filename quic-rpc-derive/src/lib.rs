use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields, Type};

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
