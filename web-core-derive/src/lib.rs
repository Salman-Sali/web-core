use proc_macro::TokenStream;

mod diesel_enum_string;
mod diesel_jsonb;

#[cfg(feature = "diesel")]
#[proc_macro_attribute]
pub fn diesel_jsonb(attr: TokenStream, item: TokenStream) -> TokenStream {
    diesel_jsonb::generate_diesel_jsonb(attr, item)
}

#[cfg(feature = "diesel")]
#[proc_macro_attribute]
pub fn diesel_enum_string(attr: TokenStream, item: TokenStream) -> TokenStream {
    diesel_enum_string::generate_diesel_enum_string(attr, item)
}
