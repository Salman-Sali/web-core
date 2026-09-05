// This file tests that using `nested(use_context)` without a parent context
// produces a clear compile-time error.
//
// To manually test: uncomment the code below and verify it produces an error
// about "Cannot use `nested(use_context)` when the struct has no context"

// #[cfg(feature = "derive")]
// mod tests {
//     use web_core_validator::Validate;
//
//     #[derive(web_core_validator_derive::Validate)]
//     struct Child {
//         #[validate(length(min = 3))]
//         name: String,
//     }
//
//     // This should produce a compile error: "Cannot use `nested(use_context)`
//     // when the struct has no `#[validate(context = ...)]` attribute"
//     #[derive(web_core_validator_derive::Validate)]
//     struct Parent {
//         #[validate(nested(use_context))]
//         child: Child,
//     }
// }
