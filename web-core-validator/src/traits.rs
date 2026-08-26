use crate::types::{ValidationErrors, ValidationErrorsKind};
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::btree_map::BTreeMap;
use std::future::Future;

/// This is the original trait that was implemented by deriving `Validate`. It will still be
/// implemented for struct validations that don't take custom arguments. The call is being
/// forwarded to the `ValidateArgs<'v_a>` trait.
#[trait_variant::make(Send)]
pub trait Validate<S: Send + Sync> {
    fn validate(&self, state: &S) -> impl Future<Output = Result<(), ValidationErrors>>;
}

impl<T: Validate<S> + Send + Sync, S: Send + Sync> Validate<S> for &T {
    async fn validate(&self, state: &S) -> Result<(), ValidationErrors> {
        T::validate(self, state).await
    }
}

macro_rules! impl_validate_list {
    ($container:ty) => {
        impl<T: Validate<S> + Send + Sync, S: Send + Sync> Validate<S> for $container
        where
            T: Send,
        {
            async fn validate(&self, state: &S) -> Result<(), ValidationErrors> {
                let mut vec_err: BTreeMap<usize, Box<ValidationErrors>> = BTreeMap::new();

                for (index, item) in self.iter().enumerate() {
                    if let Err(e) = item.validate(state).await {
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

impl<T: Validate<S> + Send + Sync, S: Send + Sync, const N: usize> Validate<S> for [T; N]
where
    T: Send,
{
    async fn validate(&self, state: &S) -> Result<(), ValidationErrors> {
        let mut vec_err: BTreeMap<usize, Box<ValidationErrors>> = BTreeMap::new();

        for (index, item) in self.iter().enumerate() {
            if let Err(e) = item.validate(state).await {
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

impl<K, V, S, ST> Validate<ST> for HashMap<K, V, S>
where
    K: Send + Sync,
    V: Validate<ST> + Send + Sync,
    S: Send + Sync,
    ST: Send + Sync,
{
    async fn validate(&self, state: &ST) -> Result<(), ValidationErrors> {
        let mut vec_err: BTreeMap<usize, Box<ValidationErrors>> = BTreeMap::new();

        for (index, (_key, value)) in self.iter().enumerate() {
            if let Err(e) = value.validate(state).await {
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

impl<K, V, S> Validate<S> for BTreeMap<K, V>
where
    K: Send + Sync,
    V: Validate<S> + Send + Sync,
    S: Send + Sync,
{
    async fn validate(&self, state: &S) -> Result<(), ValidationErrors> {
        let mut vec_err: BTreeMap<usize, Box<ValidationErrors>> = BTreeMap::new();

        for (index, (_key, value)) in self.iter().enumerate() {
            if let Err(e) = value.validate(state).await {
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

/// This trait will be implemented by deriving `Validate`. This implementation can take one
/// argument and pass this on to custom validators. The default `Args` type will be `()` if
/// there is no custom validation with defined arguments.
///
/// The `Args` type can use the lifetime `'v_a` to pass references onto the validator.
#[trait_variant::make(Send)]
pub trait ValidateArgs<'v_a, S: Send + Sync> {
    type Args;
    fn validate_with_args(
        &self,
        args: Self::Args,
        state: &S,
    ) -> impl Future<Output = Result<(), ValidationErrors>>;
}

impl<'v_a, T, U, S: Send + Sync> ValidateArgs<'v_a, S> for Option<T>
where
    T: ValidateArgs<'v_a, S, Args = U> + Sync,
    U: Send,
{
    type Args = U;

    async fn validate_with_args(
        &self,
        args: Self::Args,
        state: &S,
    ) -> Result<(), ValidationErrors> {
        if let Some(nested) = self {
            T::validate_with_args(nested, args, state).await
        } else {
            Ok(())
        }
    }
}
