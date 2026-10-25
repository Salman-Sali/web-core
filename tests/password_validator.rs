use web_core::validators::password_validator::PasswordValidator;

#[test]
fn default_requires_10_to_20_chars() {
    assert!(PasswordValidator::default().validate("Abcdefgh12"));
    assert!(!PasswordValidator::default().validate("Abcdefg12"));
    assert!(!PasswordValidator::default().validate("Abcdefghij1234567890x"));
}

#[test]
fn with_min_length_sets_minimum_only() {
    let short = "Abc12";
    assert!(!PasswordValidator::default().validate(short));
    assert!(
        PasswordValidator::default()
            .with_min_length(5)
            .validate(short)
    );
    assert!(
        !PasswordValidator::default()
            .with_min_length(6)
            .validate(short)
    );
    // the maximum (20) is untouched
    assert!(
        !PasswordValidator::default()
            .with_min_length(5)
            .validate("Abcdefghij1234567890x")
    );
}

#[test]
fn with_max_length_sets_maximum_only() {
    let long = "Abcdefghij1234567890xyz"; // 23 chars
    assert!(!PasswordValidator::default().validate(long));
    assert!(
        PasswordValidator::default()
            .with_max_length(23)
            .validate(long)
    );
    assert!(
        !PasswordValidator::default()
            .with_max_length(22)
            .validate(long)
    );
    // the minimum (10) is untouched
    assert!(
        !PasswordValidator::default()
            .with_max_length(30)
            .validate("Abc12")
    );
}
