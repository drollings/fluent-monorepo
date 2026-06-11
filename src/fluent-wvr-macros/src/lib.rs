use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, Data, DataStruct, DeriveInput, Fields};

/// Derive macro for `fluent_wvr::FieldAccess`.
///
/// Generates `set_field`, `get_field`, and `field_names` implementations
/// that intern property names via `ArcIntern<str>` for O(1) pointer-sized
/// key matching in routing boundaries.
#[proc_macro_derive(FieldAccess)]
pub fn derive_field_access(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let fields = match &input.data {
        Data::Struct(DataStruct {
            fields: Fields::Named(fields),
            ..
        }) => &fields.named,
        _ => {
            return syn::Error::new_spanned(
                input,
                "FieldAccess can only be derived for structs with named fields",
            )
            .to_compile_error()
            .into();
        }
    };

    let field_name_strs: Vec<String> = fields
        .iter()
        .map(|f| f.ident.as_ref().unwrap().to_string())
        .collect();

    let mut set_body = TokenStream2::new();
    let mut get_body = TokenStream2::new();

    for (idx, f) in fields.iter().enumerate() {
        let field_ident = f.ident.as_ref().unwrap();
        let field_name_str = field_ident.to_string();
        let ty = &f.ty;
        let ty_str = quote!(#ty).to_string();

        let parse_expr = if ty_str == "String"
            || ty_str == "std::string::String"
            || ty_str.ends_with("::String")
        {
            quote! { value.into() }
        } else if ty_str == "bool" {
            quote! {
                value.parse().map_err(|_| fluent_wvr::FieldError::Parse(value.into()))?
            }
        } else if ty_str.starts_with("ArcIntern") || ty_str.contains("ArcIntern") {
            quote! { fluent_wvr::ArcIntern::from(value) }
        } else {
            quote! {
                value.parse::<#ty>().map_err(|_| fluent_wvr::FieldError::Parse(value.into()))?
            }
        };

        if idx == 0 {
            set_body.extend(quote! {
                if name == #field_name_str {
                    self.#field_ident = #parse_expr;
                    Ok(())
                }
            });
        } else {
            set_body.extend(quote! {
                else if name == #field_name_str {
                    self.#field_ident = #parse_expr;
                    Ok(())
                }
            });
        }

        let to_string_expr = if ty_str == "String"
            || ty_str == "std::string::String"
            || ty_str.ends_with("::String")
        {
            quote! { self.#field_ident.clone() }
        } else {
            quote! { self.#field_ident.to_string() }
        };

        if idx == 0 {
            get_body.extend(quote! {
                if name == #field_name_str {
                    Ok(#to_string_expr)
                }
            });
        } else {
            get_body.extend(quote! {
                else if name == #field_name_str {
                    Ok(#to_string_expr)
                }
            });
        }
    }

    let expanded = if fields.is_empty() {
        quote! {
            impl #impl_generics fluent_wvr::FieldAccess for #name #ty_generics #where_clause {
                fn set_field(&mut self, name: &str, _value: &str) -> Result<(), fluent_wvr::FieldError> {
                    let _key = fluent_wvr::ArcIntern::<str>::from(name);
                    Err(fluent_wvr::FieldError::NotFound(name.into()))
                }

                fn get_field(&self, name: &str) -> Result<String, fluent_wvr::FieldError> {
                    let _key = fluent_wvr::ArcIntern::<str>::from(name);
                    Err(fluent_wvr::FieldError::NotFound(name.into()))
                }

                fn field_names(&self) -> &'static [&'static str] {
                    static NAMES: &[&str] = &[];
                    NAMES
                }
            }
        }
    } else {
        quote! {
            impl #impl_generics fluent_wvr::FieldAccess for #name #ty_generics #where_clause {
                fn set_field(&mut self, name: &str, value: &str) -> Result<(), fluent_wvr::FieldError> {
                    let _key = fluent_wvr::ArcIntern::<str>::from(name);
                    #set_body else {
                        Err(fluent_wvr::FieldError::NotFound(name.into()))
                    }
                }

                fn get_field(&self, name: &str) -> Result<String, fluent_wvr::FieldError> {
                    let _key = fluent_wvr::ArcIntern::<str>::from(name);
                    #get_body else {
                        Err(fluent_wvr::FieldError::NotFound(name.into()))
                    }
                }

                fn field_names(&self) -> &'static [&'static str] {
                    static NAMES: &[&str] = &[#(#field_name_strs),*];
                    NAMES
                }
            }
        }
    };

    TokenStream::from(expanded)
}
