use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DeriveInput, Fields, parse_macro_input, spanned::Spanned};

pub fn expand_model(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_clone = item.clone();
    let input = parse_macro_input!(item as DeriveInput);

    match expand_model_impl(attr.into(), input, input_clone.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand_model_impl(
    attr: TokenStream2,
    input: DeriveInput,
    _original_tokens: TokenStream2,
) -> syn::Result<TokenStream2> {
    let forge = crate::utils::forge_path();
    let attr_str = attr.to_string();
    let trimmed = attr_str.trim();
    if !trimmed.is_empty() {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "Unknown attribute `{trimmed}` for #[model]. This macro accepts no attributes."
            ),
        ));
    }
    let struct_name = &input.ident;
    let vis = &input.vis;
    let table_name = get_table_name(&input)?;
    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => {
                return Err(syn::Error::new(
                    input.span(),
                    "Only named fields are supported",
                ));
            }
        },
        _ => return Err(syn::Error::new(input.span(), "Only structs are supported")),
    };

    let field_tokens: Vec<TokenStream2> = fields
        .iter()
        .map(|field| {
            let field_name = field.ident.as_ref().unwrap();
            let field_type = &field.ty;
            let type_str = quote!(#field_type).to_string();
            let name = field_name.to_string();
            let column_name = to_snake_case(&name);

            quote! {
                {
                    let rust_type = #forge::forge_core::schema::RustType::from_type_string(#type_str);
                    let mut field = #forge::forge_core::schema::FieldDef::new(#name, rust_type);
                    field.column_name = #column_name.to_string();
                    field
                }
            }
        })
        .collect();

    let field_defs: Vec<TokenStream2> = fields
        .iter()
        .map(|field| {
            let field_name = &field.ident;
            let field_type = &field.ty;
            let field_vis = &field.vis;
            quote! { #field_vis #field_name: #field_type }
        })
        .collect();

    let other_attrs: Vec<&syn::Attribute> = input
        .attrs
        .iter()
        .filter(|attr| {
            let path = attr.path();
            !path.is_ident("derive") && path.segments.first().is_none_or(|s| s.ident != "forge")
        })
        .collect();

    let expanded = quote! {
        #(#other_attrs)*
        #vis struct #struct_name {
            #(#field_defs),*
        }

        impl #forge::forge_core::schema::ModelMeta for #struct_name {
            const TABLE_NAME: &'static str = #table_name;

            fn table_def() -> #forge::forge_core::schema::TableDef {
                let mut table = #forge::forge_core::schema::TableDef::new(#table_name, stringify!(#struct_name));
                table.fields = vec![
                    #(#field_tokens),*
                ];
                table
            }

            fn primary_key_field() -> &'static str {
                "id"
            }
        }
    };

    Ok(expanded)
}

fn get_table_name(input: &DeriveInput) -> syn::Result<String> {
    // Look for #[table(name = "...")]. parse_nested_meta correctly handles
    // escaped quotes and inner whitespace, unlike the previous string-slice
    // parser which choked on `name = "with \"escape\""` and similar.
    for attr in &input.attrs {
        if attr.path().is_ident("table") {
            let mut found: Option<String> = None;
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    let lit: syn::LitStr = meta.value()?.parse()?;
                    found = Some(lit.value());
                    Ok(())
                } else {
                    Err(meta.error("expected `name = \"...\"`"))
                }
            })?;
            if let Some(name) = found {
                return Ok(name);
            }
        }
    }

    // Default: convert struct name to snake_case plural
    let name = to_snake_case(&input.ident.to_string());
    Ok(pluralize(&name))
}

use crate::utils::{pluralize, to_snake_case};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_simple() {
        assert_eq!(to_snake_case("User"), "user");
        assert_eq!(to_snake_case("UserProfile"), "user_profile");
        assert_eq!(to_snake_case("HTTPRequest"), "http_request");
    }

    #[test]
    fn snake_case_already_lowercase() {
        assert_eq!(to_snake_case("user"), "user");
        assert_eq!(to_snake_case("item"), "item");
    }

    #[test]
    fn pluralize_regular_nouns() {
        assert_eq!(pluralize("user"), "users");
        assert_eq!(pluralize("item"), "items");
        assert_eq!(pluralize("product"), "products");
        assert_eq!(pluralize("order"), "orders");
        assert_eq!(pluralize("account"), "accounts");
    }

    #[test]
    fn pluralize_sibilant_endings() {
        assert_eq!(pluralize("address"), "addresses");
        assert_eq!(pluralize("crash"), "crashes");
        assert_eq!(pluralize("match"), "matches");
        assert_eq!(pluralize("box"), "boxes");
        assert_eq!(pluralize("quiz"), "quizzes");
    }

    #[test]
    fn pluralize_consonant_y() {
        assert_eq!(pluralize("category"), "categories");
        assert_eq!(pluralize("company"), "companies");
        assert_eq!(pluralize("policy"), "policies");
        assert_eq!(pluralize("entry"), "entries");
    }

    #[test]
    fn pluralize_vowel_y() {
        assert_eq!(pluralize("key"), "keys");
        assert_eq!(pluralize("day"), "days");
        assert_eq!(pluralize("boy"), "boys");
        assert_eq!(pluralize("buy"), "buys");
    }

    // --- Table name derivation (integration of to_snake_case + pluralize) ---

    #[test]
    fn table_name_from_struct_name() {
        // Simulates what get_table_name does when no #[table] attribute is present
        let cases = vec![
            ("User", "users"),
            ("UserProfile", "user_profiles"),
            ("Category", "categories"),
            ("Address", "addresses"),
            ("TodoItem", "todo_items"),
            ("OrderStatus", "order_statuses"),
        ];

        for (struct_name, expected_table) in cases {
            let snake = to_snake_case(struct_name);
            let table = pluralize(&snake);
            assert_eq!(table, expected_table, "Failed for struct {struct_name}");
        }
    }
}
