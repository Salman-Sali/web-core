use quote::quote;

/// Generate tokens for nested validation.
/// For use_context = true, it validates with the parent's context args.
/// For Vec types, it iterates and validates each element.
/// For Option types (when inside a wrapper), it validates the already-bound value.
/// For scalar types, it validates directly.
pub fn nested_tokens(
    field_name: &proc_macro2::TokenStream,
    field_name_str: &str,
    use_context: bool,
    _is_option: bool,
    is_vec: bool,
) -> proc_macro2::TokenStream {
    if use_context {
        // Use context - call validate_with_args
        if is_vec {
            // Vec: iterate and validate each element with context
            quote! {
                {
                    if let ::std::collections::hash_map::Entry::Vacant(_) = errors.0.entry(::std::borrow::Cow::Borrowed(#field_name_str)) {
                        let mut vec_err: ::std::collections::BTreeMap<usize, Box<::web_core_validator::ValidationErrors>> = ::std::collections::BTreeMap::new();
                        for (index, item) in #field_name.iter().enumerate() {
                            use ::web_core_validator::ValidateArgs;
                            if let ::std::result::Result::Err(e) = item.validate_with_args(args).await {
                                vec_err.insert(index, Box::new(e));
                            }
                        }
                        if !vec_err.is_empty() {
                            let err_kind = ::web_core_validator::ValidationErrorsKind::List(vec_err);
                            let child_errors = ::web_core_validator::ValidationErrors(std::collections::HashMap::from([(
                                ::std::borrow::Cow::Borrowed("_tmp_validator"),
                                err_kind,
                            )]));
                            errors.merge_self(#field_name_str, ::std::result::Result::Err(child_errors));
                        }
                    }
                }
            }
        } else {
            // Scalar or Option (Option value is already bound via wrapper): validate directly
            quote! {
                {
                    if let ::std::collections::hash_map::Entry::Vacant(_) = errors.0.entry(::std::borrow::Cow::Borrowed(#field_name_str)) {
                        use ::web_core_validator::ValidateArgs;
                        errors.merge_self(#field_name_str, #field_name.validate_with_args(args).await);
                    }
                }
            }
        }
    } else {
        // No context - call validate (existing behavior)
        quote! {
            if let ::std::collections::hash_map::Entry::Vacant(entry) = errors.0.entry(::std::borrow::Cow::Borrowed(#field_name_str)) {
                errors.merge_self(#field_name_str, (&#field_name).validate().await);
            }
        }
    }
}
