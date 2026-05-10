use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DeriveInput, parse_macro_input};

/// Expand the #[forge::forge_enum] macro.
pub fn expand_enum(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr2: TokenStream2 = attr.into();
    let input = parse_macro_input!(item as DeriveInput);

    match expand_enum_impl(attr2, input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand_enum_impl(attr: TokenStream2, input: DeriveInput) -> syn::Result<TokenStream2> {
    let attr_str = attr.to_string();
    let trimmed = attr_str.trim();
    if !trimmed.is_empty() {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "Unknown attribute `{trimmed}` for #[forge_enum]. This macro accepts no attributes."
            ),
        ));
    }
    let enum_name = &input.ident;
    let vis = &input.vis;
    let sql_name = to_snake_case(&enum_name.to_string());
    let enum_attrs = &input.attrs;

    // Extract variants
    let variants = match &input.data {
        Data::Enum(data) => &data.variants,
        _ => {
            return Err(syn::Error::new_spanned(
                &input,
                "forge_enum can only be used on enums",
            ));
        }
    };

    let mut variant_infos = Vec::new();

    for variant in variants.iter() {
        let name = &variant.ident;
        let sql_value = to_snake_case(&name.to_string());

        variant_infos.push(VariantInfo {
            name: name.clone(),
            sql_value,
        });
    }

    // Generate variant arms for Display (to SQL string)
    let to_string_arms: Vec<TokenStream2> = variant_infos
        .iter()
        .map(|v| {
            let name = &v.name;
            let sql_value = &v.sql_value;
            quote! {
                Self::#name => #sql_value
            }
        })
        .collect();

    // Generate variant arms for FromStr (from SQL string)
    let from_string_arms: Vec<TokenStream2> = variant_infos
        .iter()
        .map(|v| {
            let name = &v.name;
            let sql_value = &v.sql_value;
            quote! {
                #sql_value => Ok(Self::#name)
            }
        })
        .collect();

    // Generate variant definitions for the original enum
    let variant_defs: Vec<TokenStream2> = variants
        .iter()
        .map(|v| {
            let name = &v.ident;
            let attrs = &v.attrs;
            quote! {
                #(#attrs)*
                #name
            }
        })
        .collect();

    let expanded = quote! {
        #(#enum_attrs)*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(rename_all = "snake_case")]
        #vis enum #enum_name {
            #(#variant_defs),*
        }

        impl #enum_name {
            /// Get the SQL string representation.
            pub fn as_sql_str(&self) -> &'static str {
                match self {
                    #(#to_string_arms),*
                }
            }

            /// Get the PostgreSQL type name.
            pub fn sql_type_name() -> &'static str {
                #sql_name
            }
        }

        impl std::fmt::Display for #enum_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.as_sql_str())
            }
        }

        impl std::str::FromStr for #enum_name {
            type Err = std::string::String;

            fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
                match s {
                    #(#from_string_arms,)*
                    _ => std::result::Result::Err(format!("Unknown {} value: {}", stringify!(#enum_name), s))
                }
            }
        }

        impl<'r> sqlx::Decode<'r, sqlx::Postgres> for #enum_name {
            fn decode(value: sqlx::postgres::PgValueRef<'r>) -> std::result::Result<Self, sqlx::error::BoxDynError> {
                let s = <&str as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
                s.parse().map_err(|e: std::string::String| e.into())
            }
        }

        impl sqlx::Encode<'_, sqlx::Postgres> for #enum_name {
            fn encode_by_ref(&self, buf: &mut sqlx::postgres::PgArgumentBuffer) -> std::result::Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
                <&str as sqlx::Encode<sqlx::Postgres>>::encode(self.as_sql_str(), buf)
            }
        }

        impl sqlx::Type<sqlx::Postgres> for #enum_name {
            fn type_info() -> sqlx::postgres::PgTypeInfo {
                sqlx::postgres::PgTypeInfo::with_name(#sql_name)
            }
        }
    };

    Ok(expanded)
}

struct VariantInfo {
    name: syn::Ident,
    sql_value: String,
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

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_expand_enum_preserves_item_attributes() {
        let input: DeriveInput = parse_quote! {
            #[derive(forge::forge_core::schemars::JsonSchema)]
            pub enum TicketStatus {
                New,
                Working,
                Resolved,
            }
        };

        let expanded = expand_enum_impl(TokenStream2::new(), input).expect("macro expansion");
        let tokens = expanded.to_string();
        assert!(tokens.contains("forge :: forge_core :: schemars :: JsonSchema"));
    }
}
