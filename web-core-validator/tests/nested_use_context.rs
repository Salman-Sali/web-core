#[cfg(feature = "derive")]
mod tests {
    use web_core_validator::{ValidateArgs, ValidationError};

    // Define a context type for tests
    #[derive(Clone, Copy)]
    struct TestContext {
        min_length: usize,
    }

    // Custom validator that uses context
    async fn validate_name_with_context(
        name: &str,
        context: &TestContext,
    ) -> Result<(), ValidationError> {
        if name.len() < context.min_length {
            return Err(ValidationError::new("too_short_name"));
        }
        Ok(())
    }

    // A child struct that requires context for validation
    #[derive(web_core_validator_derive::Validate)]
    #[validate(context = "TestContext")]
    struct ChildWithContext {
        #[validate(custom(function = "validate_name_with_context", use_context))]
        name: String,
    }

    // Parent struct using nested(use_context) - single nested struct
    #[derive(web_core_validator_derive::Validate)]
    #[validate(context = "TestContext")]
    struct ParentWithNestedContext {
        #[validate(nested(use_context))]
        child: ChildWithContext,
    }

    // Parent struct using nested(use_context) - Option<NestedStruct>
    #[derive(web_core_validator_derive::Validate)]
    #[validate(context = "TestContext")]
    struct ParentWithOptionalNestedContext {
        #[validate(nested(use_context))]
        child: Option<ChildWithContext>,
    }

    // Parent struct using nested(use_context) - Vec<NestedStruct>
    #[derive(web_core_validator_derive::Validate)]
    #[validate(context = "TestContext")]
    struct ParentWithVecNestedContext {
        #[validate(nested(use_context))]
        children: Vec<ChildWithContext>,
    }

    // Deep nesting - context propagated through multiple levels
    #[derive(web_core_validator_derive::Validate)]
    #[validate(context = "TestContext")]
    struct DeepParent {
        #[validate(nested(use_context))]
        middle: MiddleParent,
    }

    #[derive(web_core_validator_derive::Validate)]
    #[validate(context = "TestContext")]
    struct MiddleParent {
        #[validate(nested(use_context))]
        child: ChildWithContext,
    }

    // Test: basic nested(use_context) with validation error
    #[tokio::test]
    async fn test_nested_use_context_basic() {
        let mut context = TestContext { min_length: 3 };
        let parent = ParentWithNestedContext {
            child: ChildWithContext {
                name: "ab".to_string(), // too short
            },
        };

        let result = parent.validate_with_args(&mut context).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        let err_str = format!("{}", err);
        assert!(err_str.contains("child"));
    }

    // Test: basic nested(use_context) with valid data
    #[tokio::test]
    async fn test_nested_use_context_valid() {
        let mut context = TestContext { min_length: 3 };
        let parent = ParentWithNestedContext {
            child: ChildWithContext {
                name: "abcd".to_string(), // valid
            },
        };

        let result = parent.validate_with_args(&mut context).await;
        assert!(result.is_ok());
    }

    // Test: nested(use_context) with Option<T> - Some case with error
    #[tokio::test]
    async fn test_nested_use_context_with_option_some() {
        let mut context = TestContext { min_length: 3 };
        let parent = ParentWithOptionalNestedContext {
            child: Some(ChildWithContext {
                name: "ab".to_string(), // too short
            }),
        };

        let result = parent.validate_with_args(&mut context).await;
        assert!(result.is_err());
    }

    // Test: nested(use_context) with Option<T> - None case (skips validation)
    #[tokio::test]
    async fn test_nested_use_context_with_option_none() {
        let mut context = TestContext { min_length: 3 };
        let parent = ParentWithOptionalNestedContext { child: None };

        let result = parent.validate_with_args(&mut context).await;
        assert!(result.is_ok()); // No child to validate
    }

    // Test: nested(use_context) with Option<T> - Some case valid
    #[tokio::test]
    async fn test_nested_use_context_with_option_valid() {
        let mut context = TestContext { min_length: 3 };
        let parent = ParentWithOptionalNestedContext {
            child: Some(ChildWithContext {
                name: "abcd".to_string(), // valid
            }),
        };

        let result = parent.validate_with_args(&mut context).await;
        assert!(result.is_ok());
    }

    // Test: nested(use_context) with Vec<T> - all valid
    #[tokio::test]
    async fn test_nested_use_context_with_vec_valid() {
        let mut context = TestContext { min_length: 3 };
        let parent = ParentWithVecNestedContext {
            children: vec![
                ChildWithContext {
                    name: "abcd".to_string(), // valid
                },
                ChildWithContext {
                    name: "efgh".to_string(), // valid
                },
            ],
        };

        let result = parent.validate_with_args(&mut context).await;
        assert!(result.is_ok());
    }

    // Test: nested(use_context) with Vec<T> - one item invalid
    #[tokio::test]
    async fn test_nested_use_context_with_vec_invalid() {
        let mut context = TestContext { min_length: 3 };
        let parent = ParentWithVecNestedContext {
            children: vec![ChildWithContext {
                name: "ab".to_string(), // too short
            }],
        };

        let result = parent.validate_with_args(&mut context).await;
        assert!(result.is_err());
    }

    // Test: nested(use_context) with Vec<T> - multiple errors
    #[tokio::test]
    async fn test_nested_use_context_with_multiple_vec_errors() {
        let mut context = TestContext { min_length: 3 };
        let parent = ParentWithVecNestedContext {
            children: vec![
                ChildWithContext {
                    name: "ab".to_string(), // too short
                },
                ChildWithContext {
                    name: "a".to_string(), // also too short
                },
            ],
        };

        let result = parent.validate_with_args(&mut context).await;
        assert!(result.is_err());
    }

    // Test: deep nesting with context propagation
    #[tokio::test]
    async fn test_deep_nesting_use_context() {
        let mut context = TestContext { min_length: 3 };
        let deep = DeepParent {
            middle: MiddleParent {
                child: ChildWithContext {
                    name: "ab".to_string(), // too short
                },
            },
        };

        let result = deep.validate_with_args(&mut context).await;
        assert!(result.is_err());
        // The error should be at middle.child path
        let err = result.unwrap_err();
        let err_str = format!("{}", err);
        assert!(err_str.contains("middle"));
        assert!(err_str.contains("child"));
    }

    // Test: deep nesting with context propagation - valid case
    #[tokio::test]
    async fn test_deep_nesting_use_context_valid() {
        let mut context = TestContext { min_length: 3 };
        let deep = DeepParent {
            middle: MiddleParent {
                child: ChildWithContext {
                    name: "valid".to_string(),
                },
            },
        };

        let result = deep.validate_with_args(&mut context).await;
        assert!(result.is_ok());
    }

    // Test: context propagation changes validation behavior
    #[tokio::test]
    async fn test_context_propagation_changes_validation() {
        // First test with min_length = 5 (should fail)
        let mut strict_context = TestContext { min_length: 5 };
        let parent = ParentWithNestedContext {
            child: ChildWithContext {
                name: "abcd".to_string(), // too short for strict
            },
        };
        assert!(
            parent
                .validate_with_args(&mut strict_context)
                .await
                .is_err()
        );

        // Same data, but with min_length = 3 (should pass)
        let mut lenient_context = TestContext { min_length: 3 };
        assert!(
            parent
                .validate_with_args(&mut lenient_context)
                .await
                .is_ok()
        );
    }
}
