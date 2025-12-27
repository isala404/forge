use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    parse_macro_input, spanned::Spanned, Attribute, Data, DeriveInput, Expr, Fields, Lit, Meta,
};

/// Expand the #[forge::model] macro.
pub fn expand_model(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);

    match expand_model_impl(attr.into(), input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand_model_impl(_attr: TokenStream2, input: DeriveInput) -> syn::Result<TokenStream2> {
    let struct_name = &input.ident;
    let table_name = get_table_name(&input)?;
    let vis = &input.vis;

    // Extract fields
    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => {
                return Err(syn::Error::new(
                    input.span(),
                    "Only named fields are supported",
                ))
            }
        },
        _ => return Err(syn::Error::new(input.span(), "Only structs are supported")),
    };

    // Parse field information
    let mut field_defs = Vec::new();
    let mut primary_key_field = None;

    for field in fields.iter() {
        let field_name = field.ident.as_ref().unwrap();
        let field_type = &field.ty;
        let type_str = quote!(#field_type).to_string();

        // Check for #[id] attribute
        let is_id = has_attribute(&field.attrs, "id");
        let is_indexed = has_attribute(&field.attrs, "indexed");
        let is_unique = has_attribute(&field.attrs, "unique");
        let is_encrypted = has_attribute(&field.attrs, "encrypted");
        let is_updated_at = has_attribute(&field.attrs, "updated_at");
        let default_value = get_attribute_value(&field.attrs, "default");

        if is_id {
            primary_key_field = Some(field_name.to_string());
        }

        let column_name = to_snake_case(&field_name.to_string());

        field_defs.push(FieldInfo {
            name: field_name.to_string(),
            column_name,
            rust_type: type_str,
            is_id,
            is_indexed,
            is_unique,
            is_encrypted,
            is_updated_at,
            default_value,
        });
    }

    let primary_key = primary_key_field.unwrap_or_else(|| "id".to_string());
    let primary_key_lit = &primary_key;

    // Generate field definitions for TableDef
    let field_tokens: Vec<TokenStream2> = field_defs
        .iter()
        .map(|f| {
            let name = &f.name;
            let column_name = &f.column_name;
            let rust_type = &f.rust_type;
            let is_id = f.is_id;
            let is_indexed = f.is_indexed;
            let is_unique = f.is_unique;
            let is_encrypted = f.is_encrypted;
            let is_updated_at = f.is_updated_at;

            let mut attributes = Vec::new();
            if is_id {
                attributes.push(quote!(forge::forge_core::schema::FieldAttribute::Id));
            }
            if is_indexed {
                attributes.push(quote!(forge::forge_core::schema::FieldAttribute::Indexed));
            }
            if is_unique {
                attributes.push(quote!(forge::forge_core::schema::FieldAttribute::Unique));
            }
            if is_encrypted {
                attributes.push(quote!(forge::forge_core::schema::FieldAttribute::Encrypted));
            }
            if is_updated_at {
                attributes.push(quote!(forge::forge_core::schema::FieldAttribute::UpdatedAt));
            }

            let default_token = if let Some(ref default) = f.default_value {
                quote!(Some(#default.to_string()))
            } else {
                quote!(None)
            };

            quote! {
                {
                    let rust_type = forge::forge_core::schema::RustType::from_type_string(#rust_type);
                    let mut field = forge::forge_core::schema::FieldDef::new(#name, rust_type);
                    field.column_name = #column_name.to_string();
                    field.attributes = vec![#(#attributes),*];
                    field.default = #default_token;
                    field
                }
            }
        })
        .collect();

    // Generate the impl
    let expanded = quote! {
        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
        #vis struct #struct_name {
            #fields
        }

        impl forge::forge_core::schema::ModelMeta for #struct_name {
            const TABLE_NAME: &'static str = #table_name;

            fn table_def() -> forge::forge_core::schema::TableDef {
                let mut table = forge::forge_core::schema::TableDef::new(#table_name, stringify!(#struct_name));
                table.fields = vec![
                    #(#field_tokens),*
                ];
                table
            }

            fn primary_key_field() -> &'static str {
                #primary_key_lit
            }
        }
    };

    Ok(expanded)
}

struct FieldInfo {
    name: String,
    column_name: String,
    rust_type: String,
    is_id: bool,
    is_indexed: bool,
    is_unique: bool,
    is_encrypted: bool,
    is_updated_at: bool,
    default_value: Option<String>,
}

fn get_table_name(input: &DeriveInput) -> syn::Result<String> {
    // Look for #[table(name = "...")]
    for attr in &input.attrs {
        if attr.path().is_ident("table") {
            let meta = attr.meta.clone();
            if let Meta::List(list) = meta {
                let tokens: TokenStream2 = list.tokens;
                let tokens_str = tokens.to_string();
                if tokens_str.starts_with("name") {
                    // Parse name = "value"
                    if let Some(value) = extract_string_value(&tokens_str) {
                        return Ok(value);
                    }
                }
            }
        }
    }

    // Default: convert struct name to snake_case plural
    let name = to_snake_case(&input.ident.to_string());
    Ok(pluralize(&name))
}

fn has_attribute(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident(name))
}

fn get_attribute_value(attrs: &[Attribute], name: &str) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident(name) {
            if let Meta::NameValue(nv) = &attr.meta {
                if let Expr::Lit(lit) = &nv.value {
                    if let Lit::Str(s) = &lit.lit {
                        return Some(s.value());
                    }
                }
            }
        }
    }
    None
}

fn extract_string_value(s: &str) -> Option<String> {
    // Parse "name = \"value\"" pattern
    let parts: Vec<&str> = s.splitn(2, '=').collect();
    if parts.len() == 2 {
        let value = parts[1].trim();
        if let Some(stripped) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            return Some(stripped.to_string());
        }
    }
    None
}

fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
}

fn pluralize(s: &str) -> String {
    // Simple English pluralization rules
    if s.ends_with('s')
        || s.ends_with("sh")
        || s.ends_with("ch")
        || s.ends_with('x')
        || s.ends_with('z')
    {
        format!("{}es", s)
    } else if let Some(stem) = s.strip_suffix('y') {
        if !s.ends_with("ay") && !s.ends_with("ey") && !s.ends_with("oy") && !s.ends_with("uy") {
            format!("{}ies", stem)
        } else {
            format!("{}s", s)
        }
    } else {
        format!("{}s", s)
    }
}
