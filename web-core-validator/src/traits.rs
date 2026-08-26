use crate::types::{ValidationErrors, ValidationErrorsKind};
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::btree_map::BTreeMap;
use std::future::Future;

/// This is the original trait that was implemented by deriving `Validate`. It will still be
/// implemented for struct validations that don't take custom arguments. The call is being
/// forwarded to the `ValidateArgs<'v_a>` trait.
#[trait_variant::make(Send)]
pub trait Validate {
    fn validate(&self) -> impl Future<Output = Result<(), ValidationErrors>>;
}

impl<T: Validate + Send + Sync> Validate for &T {
    async fn validate(&self) -> Result<(), ValidationErrors> {
        T::validate(self).await
    }
}

macro_rules! impl_validate_list {
    ($container:ty) => {
        impl<T: Validate + Send + Sync> Validate for $container
        where
            T: Send,
        {
            async fn validate(&self) -> Result<(), ValidationErrors> {
                let mut vec_err: BTreeMap<usize, Box<ValidationErrors>> = BTreeMap::new();

                for (index, item) in self.iter().enumerate() {
                    if let Err(e) = item.validate().await {
                        vec_err.insert(index, Box::new(e));
                    }
                }

                if vec_err.is_empty() {
                    Ok(())
                } else {
                    let err_kind = ValidationErrorsKind::List(vec_err);
                    let errors = ValidationErrors(std::collections::HashMap::from([(
                        Cow::Borrowed("_tmp_validator"),
                        err_kind,
                    )]));
                    Err(errors)
                }
            }
        }
    };
}

impl_validate_list!(std::collections::HashSet<T>);
impl_validate_list!(std::collections::BTreeSet<T>);
impl_validate_list!(std::collections::BinaryHeap<T>);
impl_validate_list!(std::collections::LinkedList<T>);
impl_validate_list!(std::collections::VecDeque<T>);
impl_validate_list!(std::vec::Vec<T>);
impl_validate_list!([T]);

impl<T: Validate + Send + Sync, const N: usize> Validate for [T; N]
where
    T: Send,
{
    async fn validate(&self) -> Result<(), ValidationErrors> {
        let mut vec_err: BTreeMap<usize, Box<ValidationErrors>> = BTreeMap::new();

        for (index, item) in self.iter().enumerate() {
            if let Err(e) = item.validate().await {
                vec_err.insert(index, Box::new(e));
            }
        }

        if vec_err.is_empty() {
            Ok(())
        } else {
            let err_kind = ValidationErrorsKind::List(vec_err);
            let errors = ValidationErrors(std::collections::HashMap::from([(
                Cow::Borrowed("_tmp_validator"),
                err_kind,
            )]));
            Err(errors)
        }
    }
}

impl<K, V, S> Validate for HashMap<K, V, S>
where
    K: Send + Sync,
    V: Validate + Send + Sync,
    S: Send + Sync,
{
    async fn validate(&self) -> Result<(), ValidationErrors> {
        let mut vec_err: BTreeMap<usize, Box<ValidationErrors>> = BTreeMap::new();

        for (index, (_key, value)) in self.iter().enumerate() {
            if let Err(e) = value.validate().await {
                vec_err.insert(index, Box::new(e));
            }
        }

        if vec_err.is_empty() {
            Ok(())
        } else {
            let err_kind = ValidationErrorsKind::List(vec_err);
            let errors =
                ValidationErrors(HashMap::from([(Cow::Borrowed("_tmp_validator"), err_kind)]));
            Err(errors)
        }
    }
}

impl<K, V> Validate for BTreeMap<K, V>
where
    K: Send + Sync,
    V: Validate + Send + Sync,
{
    async fn validate(&self) -> Result<(), ValidationErrors> {
        let mut vec_err: BTreeMap<usize, Box<ValidationErrors>> = BTreeMap::new();

        for (index, (_key, value)) in self.iter().enumerate() {
            if let Err(e) = value.validate().await {
                vec_err.insert(index, Box::new(e));
            }
        }

        if vec_err.is_empty() {
            Ok(())
        } else {
            let err_kind = ValidationErrorsKind::List(vec_err);
            let errors =
                ValidationErrors(HashMap::from([(Cow::Borrowed("_tmp_validator"), err_kind)]));
            Err(errors)
        }
    }
}

/// A marker trait to associate a validated type with its context/state type.
/// This is used by `ValidatedJson` to ensure the type can be extracted from
/// axum requests with the correct state type.
pub trait HasContext<T> {}

/// The `Args` type can use the lifetime `'v_a` to pass references onto the validator.
#[trait_variant::make(Send)]
pub trait ValidateArgs<'v_a> {
    type Args;
    fn validate_with_args(
        &self,
        args: Self::Args,
    ) -> impl Future<Output = Result<(), ValidationErrors>>;
}

impl<'v_a, T, U> ValidateArgs<'v_a> for Option<T>
where
    T: ValidateArgs<'v_a, Args = U>,
    U: Send,
    T: Sync,
{
    type Args = U;

    async fn validate_with_args(&self, args: Self::Args) -> Result<(), ValidationErrors> {
        if let Some(nested) = self {
            T::validate_with_args(nested, args).await
        } else {
            Ok(())
        }
    }
}
