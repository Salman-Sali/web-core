use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, parse_macro_input};

#[cfg(feature = "diesel")]
#[proc_macro_attribute]
pub fn diesel_jsonb(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    let name = &input.ident;
    let (_, ty_generics, _) = input.generics.split_for_impl();

    // Add Serialize + DeserializeOwned bounds to each generic type param
    let mut generics_with_bounds = input.generics.clone();
    for param in generics_with_bounds.type_params_mut() {
        param.bounds.push(syn::parse_quote!(serde::Serialize));
        param
            .bounds
            .push(syn::parse_quote!(serde::de::DeserializeOwned));
    }
    let (bounded_impl_generics, _, bounded_where_clause) = generics_with_bounds.split_for_impl();

    let expanded = quote! {
        #[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]
        #[diesel(sql_type = diesel::sql_types::Jsonb)]
        #input

        impl #bounded_impl_generics diesel::serialize::ToSql<diesel::sql_types::Jsonb, diesel::pg::Pg>
            for #name #ty_generics #bounded_where_clause
        {
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

        impl #bounded_impl_generics diesel::deserialize::FromSql<diesel::sql_types::Jsonb, diesel::pg::Pg>
            for #name #ty_generics #bounded_where_clause
        {
            fn from_sql(
                bytes: diesel::pg::PgValue<'_>,
            ) -> diesel::deserialize::Result<Self> {
                Ok(serde_json::from_slice(&bytes.as_bytes()[1..])?)
            }
        }
    };

    TokenStream::from(expanded)
}
