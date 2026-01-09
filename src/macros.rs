#[macro_export]
macro_rules! something_went_wrong {
    () => {
        $crate::error::Error::new_something_went_wrong("")
    };    
    ($($arg:tt)*) => {{
        $crate::error::Error::new_something_went_wrong(format!($($arg)*))
    }};
    ($val:expr) => {{
        $crate::error::Error::new_something_went_wrong(format!("{:?}", $val))
    }};
}

#[macro_export]
macro_rules! unauthorized {
    () => {
        $crate::error::Error::new_unauthorized("")
    };
    ($($arg:tt)*) => {{
        $crate::error::Error::new_unauthorized(&format!($($arg)*))
    }};
}

#[macro_export]
macro_rules! bad_request {
    ($a:expr, $b:tt) => {
        $crate::error::Error::BadRequestError($crate::error::bad_request::BadRequestError::new_with_data(format!($a), $crate::serde_json::json!($b)))
    };

    ($($arg:tt)*) => {{
        $crate::error::Error::BadRequestError($crate::error::bad_request::BadRequestError::new(format!($($arg)*)))
    }};

}

#[macro_export]
macro_rules! not_found {
    () => {
        $crate::error::Error::new_not_found("")
    };
    ($($arg:tt)*) => {{
        $crate::error::Error::new_not_found(&format!($($arg)*))
    }};
}
