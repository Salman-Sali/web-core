use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, parse_macro_input};

pub fn generate_diesel_enum_string(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let expanded = quote! {
        #[derive(
            diesel::expression::AsExpression,
            diesel::deserialize::FromSqlRow
        )]
        #[diesel(sql_type = diesel::sql_types::Text)]
        #input

        impl #impl_generics diesel::serialize::ToSql<diesel::sql_types::Text, diesel::pg::Pg>
            for #name #ty_generics #where_clause
        {
            fn to_sql<'b>(
                &'b self,
                out: &mut diesel::serialize::Output<'b, '_, diesel::pg::Pg>,
            ) -> diesel::serialize::Result {
                use std::io::Write;
                out.write_all(self.to_string().as_bytes())?;
                Ok(diesel::serialize::IsNull::No)
            }
        }

        impl #impl_generics diesel::deserialize::FromSql<diesel::sql_types::Text, diesel::pg::Pg>
            for #name #ty_generics #where_clause
        {
            fn from_sql(
                bytes: diesel::pg::PgValue<'_>,
            ) -> diesel::deserialize::Result<Self> {
                let s = std::str::from_utf8(bytes.as_bytes())?;
                s.parse::<Self>().map_err(Into::into)
            }
        }
    };
    TokenStream::from(expanded)
}
