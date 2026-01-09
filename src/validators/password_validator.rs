pub struct PasswordValidator {
    must_have_uppercase: bool,
    must_have_lowercase: bool,
    must_have_digits: bool,
    must_have_minimum_length: Option<usize>,
    must_have_maxiumum_length: Option<usize>,
    must_have_special_charecters: bool,
}

pub const SPECIAL_SYMBOLS: [char; 32] = [
    '!', '"', '#', '$', '%', '&', '\'', '(', ')', '*', '+', ',', '-', '.', '/', ':', ';', '<', '=', '>', '?', '@', '[', '\\', ']', '^', '_', '`', '{', '|', '}', '~',
];

impl PasswordValidator {
    pub fn validate(self, password: &str) -> bool {
        let mut has_upper = false;
        let mut has_lower = false;
        let mut has_digit = false;
        let mut has_special_symbols = false;

        for c in password.chars() {
            has_lower |= c.is_lowercase();
            has_upper |= c.is_uppercase();
            has_digit |= c.is_digit(10);
            has_special_symbols |= SPECIAL_SYMBOLS.contains(&c);
        }

        let uppercase_check_pass = match self.must_have_uppercase {
            true => has_upper,
            false => true,
        };

        let lowercase_check_pass = match self.must_have_lowercase {
            true => has_lower,
            false => true,
        };

        let digit_check_pass = match self.must_have_digits {
            true => has_digit,
            false => true,
        };

        let special_symbols_check_pass = match self.must_have_special_charecters {
            true => has_special_symbols,
            false => true,
        };

        let min_length_check_pass = match self.must_have_minimum_length {
            Some(l) => password.len() >= l,
            None => true,
        };

        let max_length_check_pass = match self.must_have_maxiumum_length {
            Some(l) => password.len() <= l,
            None => true,
        };

        uppercase_check_pass && lowercase_check_pass && digit_check_pass && special_symbols_check_pass && min_length_check_pass && max_length_check_pass
    }

    pub fn with_min_length(mut self, length: usize) -> Self {
        self.must_have_maxiumum_length = Some(length);
        self
    }

    pub fn with_max_length(mut self, length: usize) -> Self {
        self.must_have_maxiumum_length = Some(length);
        self
    }

    pub fn with_special_charecters(mut self) -> Self {
        self.must_have_special_charecters = true;
        self
    }
}

impl Default for PasswordValidator {
    fn default() -> Self {
        Self {
            must_have_uppercase: true,
            must_have_lowercase: true,
            must_have_digits: true,
            must_have_minimum_length: Some(10),
            must_have_maxiumum_length: Some(20),
            must_have_special_charecters: false,
        }
    }
}
