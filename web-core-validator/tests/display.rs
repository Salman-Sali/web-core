#[cfg(feature = "derive")]
mod tests {
    use web_core_validator::Validate;

    #[derive(Validate, Clone)]
    struct Foo {
        #[validate(length(equal = 5, message = "Please provide a valid foo!"))]
        foo: String,
    }

    #[tokio::test]
    async fn test_message() {
        let bad_foo = Foo { foo: "hi!".into() };
        let err = format!("{}", bad_foo.validate().await.unwrap_err());
        assert_eq!(err, "foo: Please provide a valid foo!");
    }

    #[derive(Validate)]
    struct Bar {
        #[validate(nested)]
        bar: Foo,
    }

    #[derive(Validate)]
    struct DeepBar {
        #[validate(nested)]
        deep_bar: Bar,
    }

    #[tokio::test]
    async fn test_nested_struct() {
        let bad_foo = Foo { foo: "hi!".into() };
        let bad_bar = Bar { bar: bad_foo };
        let err = format!("{}", bad_bar.validate().await.unwrap_err());
        assert_eq!(err, "bar.foo: Please provide a valid foo!");

        let bad_deep_bar = DeepBar { deep_bar: bad_bar };
        let err = format!("{}", bad_deep_bar.validate().await.unwrap_err());
        assert_eq!(err, "deep_bar.bar.foo: Please provide a valid foo!");
    }

    #[derive(Validate)]
    struct Baz {
        #[validate(nested)]
        baz: Vec<Foo>,
    }

    #[tokio::test]
    async fn test_nested_vec() {
        let bad_foo = Foo { foo: "hi!".into() };
        let bad_baz = Baz { baz: vec![bad_foo] };
        let err = format!("{}", bad_baz.validate().await.unwrap_err());
        assert_eq!(err, "baz[0].foo: Please provide a valid foo!");
    }
}
