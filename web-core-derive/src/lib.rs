use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, parse_macro_input};

#[cfg(feature = "diesel")]
#[proc_macro_attribute]
pub fn diesel_jsonb(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    let name = &input.ident;

    let expanded = quote! {
        #[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]
        #[diesel(sql_type = diesel::sql_types::Jsonb)]
        #input

        impl diesel::serialize::ToSql<diesel::sql_types::Jsonb, diesel::pg::Pg> for #name {
            fn to_sql<'b>(
                &'b self,
                out: &mut diesel::serialize::Output<'b, '_, diesel::pg::Pg>,
            ) -> diesel::serialize::Result {
                use std::io::Write;
                out.write_all(&[1])?;
                serde_json::to_writer(out, self)?;
                Ok(diesel::serialize::IsNull::No)
            }
        }

        impl diesel::deserialize::FromSql<diesel::sql_types::Jsonb, diesel::pg::Pg> for #name {
            fn from_sql(
                bytes: diesel::pg::PgValue<'_>,
            ) -> diesel::deserialize::Result<Self> {
                Ok(serde_json::from_slice(&bytes.as_bytes()[1..])?)
            }
        }
    };

    TokenStream::from(expanded)
}
