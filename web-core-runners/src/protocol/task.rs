use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};

/// A task type of a product's contract: one pure unit of work with a JSON input and output.
///
/// Implemented in the product's contract crate on a marker type, so the app (submitting) and
/// the runner (executing) agree on the name and the shapes:
///
/// ```
/// use schemars::JsonSchema;
/// use serde::{Deserialize, Serialize};
/// use web_core_runners::protocol::{FileSlot, Task};
///
/// pub struct Thumbnail;
///
/// #[derive(Serialize, Deserialize, JsonSchema)]
/// pub struct ThumbnailInput { pub width: u32 }
///
/// #[derive(Serialize, Deserialize, JsonSchema)]
/// pub struct ThumbnailOutput { pub height: u32 }
///
/// impl Task for Thumbnail {
///     const TYPE: &'static str = "thumbnail";
///     type Input = ThumbnailInput;
///     type Output = ThumbnailOutput;
///
///     fn files(_input: &ThumbnailInput) -> Vec<FileSlot> {
///         vec![FileSlot::input("image"), FileSlot::output("thumbnail")]
///     }
/// }
/// ```
pub trait Task {
    /// Stable name of the task type, sent in `Assign.task_type`.
    const TYPE: &'static str;
    /// Input parameters.
    type Input: Serialize + DeserializeOwned + JsonSchema + Send + 'static;
    /// Output result.
    type Output: Serialize + DeserializeOwned + JsonSchema + Send + 'static;

    /// File slots this input needs: the app must supply every input slot and every
    /// non-optional output slot when submitting. Default: none.
    fn files(_input: &Self::Input) -> Vec<FileSlot> {
        Vec::new()
    }
}

/// Direction of a [`FileSlot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotDirection {
    /// Downloaded by the runner before the task runs.
    In,
    /// Written by the task, uploaded by the runner after it succeeds.
    Out,
}

/// A named file a task reads or writes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileSlot {
    /// Slot name (`[A-Za-z0-9_.-]`).
    pub name: String,
    /// Input or output.
    pub direction: SlotDirection,
    /// For outputs: the task may succeed without writing it.
    pub optional: bool,
}

impl FileSlot {
    /// A required input slot.
    pub fn input(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            direction: SlotDirection::In,
            optional: false,
        }
    }

    /// A required output slot.
    pub fn output(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            direction: SlotDirection::Out,
            optional: false,
        }
    }

    /// An output slot the task may leave unwritten.
    pub fn optional_output(name: impl Into<String>) -> Self {
        Self {
            optional: true,
            ..Self::output(name)
        }
    }
}
