//! Wire protocol between the app and its runners.
//!
//! Every message is one JSON text frame on the WebSocket at `/runners/ws`, tagged by `"type"`
//! (snake_case). Runners send [`RunnerMessage`], the app sends [`AppMessage`]. Files never
//! travel over the socket: [`Assign`] carries signed HTTPS URLs for every input and output.

mod contract;
mod task;

use std::{collections::BTreeMap, fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub use contract::{Contract, canonical_json, contract_hash};
pub use task::{FileSlot, SlotDirection, Task};

/// Transport protocol version. Bumped only when the message shapes change; a runner with a
/// different version is treated as outdated.
pub const PROTOCOL_VERSION: u32 = 1;

/// Path of the runner WebSocket, relative to the app's base URL.
pub const WS_PATH: &str = "/runners/ws";

/// Path of the runner binary download, relative to the app's base URL.
pub const BINARY_PATH: &str = "/runners/binary";

/// Response header carrying the SHA-256 (hex) of the served runner binary.
pub const SHA256_HEADER: &str = "x-runner-sha256";

/// Response header carrying the contract hash the served runner binary was built with.
pub const CONTRACT_HASH_HEADER: &str = "x-runner-contract-hash";

/// Response header of `HEAD` on an upload URL: bytes of the slot already received.
pub const UPLOAD_OFFSET_HEADER: &str = "upload-offset";

macro_rules! uuid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// A new random id.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }
    };
}

uuid_id!(
    /// Id of a registered runner (the `runners` row).
    RunnerId
);
uuid_id!(
    /// Id of a submitted task (the `runner_tasks` row).
    TaskId
);
uuid_id!(
    /// Id of one dispatch of a task to a runner (the `runner_attempts` row).
    AttemptId
);

impl RunnerId {
    /// Reserved id of the app's local fallback runner; it has no `runners` row.
    pub const LOCAL: RunnerId = RunnerId(Uuid::nil());
}

/// Messages a runner sends to the app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunnerMessage {
    /// First message on every connection.
    Hello(Hello),
    /// Sent every `heartbeat_ms`; renews the leases of the listed attempts.
    Heartbeat {
        /// Attempts currently executing on the runner.
        running: Vec<AttemptId>,
    },
    /// Progress of one attempt; renews its lease.
    Progress {
        /// The attempt this progress belongs to.
        attempt: AttemptId,
        /// 0..=100.
        percent: f32,
        /// Structured facts the task learned early; the app keeps the last one per task.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint: Option<Value>,
    },
    /// Final outcome of one attempt.
    Result {
        /// The attempt this result belongs to.
        attempt: AttemptId,
        /// What happened.
        outcome: Outcome,
        /// Monotonic run time on the runner (never a wall-clock time).
        elapsed_ms: u64,
    },
    /// The runner is shutting down: give it no more work; its attempts end with the connection.
    Draining,
    /// A log line worth showing in the app's logs.
    Log {
        /// Severity.
        level: LogLevel,
        /// The line.
        message: String,
        /// The attempt it concerns, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<AttemptId>,
    },
}

/// Payload of [`RunnerMessage::Hello`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    /// Host name of the runner machine (`local` for the local fallback).
    pub runner_name: String,
    /// [`PROTOCOL_VERSION`] the runner was built with.
    pub protocol_version: u32,
    /// Contract hash the runner binary was built with.
    pub contract_hash: String,
    /// Versions of external tools (ffmpeg, libreoffice); informational.
    #[serde(default)]
    pub tool_versions: BTreeMap<String, String>,
    /// Attempts still executing on the runner (non-empty only after a reconnect).
    #[serde(default)]
    pub in_flight: Vec<AttemptId>,
    /// Whether the runner runs in local mode (the app's local fallback).
    #[serde(default)]
    pub local: bool,
    /// Crate version of the runner binary; informational.
    #[serde(default)]
    pub runner_version: String,
}

/// Final outcome of an attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// The task succeeded; every output slot was uploaded.
    Ok {
        /// The task's output JSON.
        output: Value,
        /// Hash and size of every uploaded output slot, for the app to verify.
        #[serde(default)]
        files: Vec<UploadedFile>,
    },
    /// The task reported a failure (bad input, unsupported media). Final: no retry.
    Failed {
        /// Human-readable reason.
        reason: String,
    },
    /// The task process died (panic, signal, non-zero exit). Retried as a new attempt.
    Crashed {
        /// Exit code, `None` when killed by a signal.
        exit_code: Option<i32>,
        /// Last few KiB of the task's stderr.
        stderr_tail: String,
    },
    /// The task ran past the `timeout_ms` of its assignment and was killed. Retried.
    TimedOut,
    /// Downloading an input or uploading an output failed. Retried.
    TransferFailed {
        /// Human-readable reason.
        reason: String,
    },
}

impl Outcome {
    /// Whether this outcome ends the task (`Ok` and `Failed`) rather than calling for another
    /// attempt.
    pub fn is_final(&self) -> bool {
        matches!(self, Outcome::Ok { .. } | Outcome::Failed { .. })
    }

    /// Short description for logs and attempt history.
    pub fn describe(&self) -> String {
        match self {
            Outcome::Ok { .. } => "ok".into(),
            Outcome::Failed { reason } => format!("failed: {reason}"),
            Outcome::Crashed {
                exit_code,
                stderr_tail,
            } => match exit_code {
                Some(code) => format!("crashed with exit code {code}: {stderr_tail}"),
                None => format!("killed by a signal: {stderr_tail}"),
            },
            Outcome::TimedOut => "timed out".into(),
            Outcome::TransferFailed { reason } => format!("transfer failed: {reason}"),
        }
    }
}

/// An output slot the runner uploaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadedFile {
    /// Slot name.
    pub slot: String,
    /// SHA-256 of the uploaded bytes, lowercase hex.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
}

/// Log severity of [`RunnerMessage::Log`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Informational.
    Info,
    /// Something went wrong but the runner carries on.
    Warn,
    /// An error.
    Error,
}

/// Messages the app sends to a runner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppMessage {
    /// Answer to [`Hello`].
    Welcome {
        /// The runner's id.
        runner_id: RunnerId,
        /// Capacity and timing set in admin.
        settings: PushedSettings,
        /// In-flight attempts the runner must kill and forget.
        abort: Vec<AttemptId>,
    },
    /// The runner's contract or protocol does not match: it gets no work and should update
    /// itself from `binary_url`. Sent before `Welcome`.
    Outdated {
        /// Absolute URL of the current runner binary (token-authenticated).
        binary_url: String,
        /// SHA-256 of that binary, lowercase hex.
        sha256: String,
        /// Contract hash the app expects.
        contract_hash: String,
    },
    /// Run one attempt of a task.
    Assign(Assign),
    /// Kill an attempt and forget it; no result is expected.
    Cancel {
        /// The attempt to kill.
        attempt: AttemptId,
    },
    /// New settings from admin.
    Settings(PushedSettings),
    /// Stop accepting work (the app sends none); stop managed services once idle.
    Drain,
    /// Undo `Drain`: start managed services again.
    Resume,
    /// Finish running attempts within `grace_ms` (then kill them), stop services and exit.
    Exit {
        /// How long running attempts may continue.
        grace_ms: u64,
    },
}

/// Settings the app pushes to a runner at `Welcome` and on change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushedSettings {
    /// Maximum concurrent attempts (enforced by the app; informational for the runner).
    pub capacity: u32,
    /// Heartbeat interval.
    pub heartbeat_ms: u64,
}

/// Payload of [`AppMessage::Assign`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assign {
    /// The attempt id every later message about this run carries.
    pub attempt: AttemptId,
    /// The task this attempt belongs to.
    pub task_id: TaskId,
    /// 1-based attempt number.
    pub number: u32,
    /// [`Task::TYPE`] of the task.
    pub task_type: String,
    /// The task's input JSON.
    pub input: Value,
    /// Input files to download before running.
    #[serde(default)]
    pub inputs: Vec<InputFile>,
    /// Output slots to upload after a successful run.
    #[serde(default)]
    pub outputs: Vec<OutputSlot>,
    /// The runner kills the task after this long and reports `timed_out`.
    pub timeout_ms: u64,
}

/// One input file of an [`Assign`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputFile {
    /// Slot name; the task finds the file at `<work dir>/in/<slot>`.
    pub slot: String,
    /// Signed download URL (supports `Range`).
    pub url: String,
    /// Expected SHA-256, lowercase hex.
    pub sha256: String,
    /// Expected size in bytes.
    pub size: u64,
}

/// One output slot of an [`Assign`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputSlot {
    /// Slot name; the task writes the file to `<work dir>/out/<slot>`.
    pub slot: String,
    /// Signed upload URL (`HEAD` for the received offset, `PUT` with `Content-Range`).
    pub url: String,
    /// Whether the task may leave this slot empty (no file) on success.
    #[serde(default)]
    pub optional: bool,
}

/// Whether a slot name is safe in a URL path and a file name: `[A-Za-z0-9_.-]`, 1..=64 chars,
/// not starting with a dot.
pub fn is_valid_slot_name(slot: &str) -> bool {
    !slot.is_empty()
        && slot.len() <= 64
        && !slot.starts_with('.')
        && slot
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + fmt::Debug>(value: T) {
        let text = serde_json::to_string(&value).expect("serialize");
        let back: T = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(value, back, "{text}");
    }

    #[test]
    fn runner_messages_round_trip() {
        let attempt = AttemptId::new();
        round_trip(RunnerMessage::Hello(Hello {
            runner_name: "office-1".into(),
            protocol_version: PROTOCOL_VERSION,
            contract_hash: "abc".into(),
            tool_versions: BTreeMap::from([("ffmpeg".into(), "7.1".into())]),
            in_flight: vec![attempt],
            local: false,
            runner_version: "0.5.0".into(),
        }));
        round_trip(RunnerMessage::Heartbeat {
            running: vec![attempt],
        });
        round_trip(RunnerMessage::Progress {
            attempt,
            percent: 42.5,
            checkpoint: Some(json!({"duration_ms": 1000})),
        });
        round_trip(RunnerMessage::Progress {
            attempt,
            percent: 1.0,
            checkpoint: None,
        });
        for outcome in [
            Outcome::Ok {
                output: json!({"a": 1}),
                files: vec![UploadedFile {
                    slot: "out".into(),
                    sha256: "00".into(),
                    size: 3,
                }],
            },
            Outcome::Failed {
                reason: "bad".into(),
            },
            Outcome::Crashed {
                exit_code: Some(101),
                stderr_tail: "panic".into(),
            },
            Outcome::Crashed {
                exit_code: None,
                stderr_tail: String::new(),
            },
            Outcome::TimedOut,
            Outcome::TransferFailed {
                reason: "reset".into(),
            },
        ] {
            round_trip(RunnerMessage::Result {
                attempt,
                outcome,
                elapsed_ms: 5,
            });
        }
        round_trip(RunnerMessage::Draining);
        round_trip(RunnerMessage::Log {
            level: LogLevel::Warn,
            message: "retrying".into(),
            attempt: Some(attempt),
        });
    }

    #[test]
    fn app_messages_round_trip() {
        let attempt = AttemptId::new();
        let settings = PushedSettings {
            capacity: 2,
            heartbeat_ms: 15_000,
        };
        round_trip(AppMessage::Welcome {
            runner_id: RunnerId::new(),
            settings,
            abort: vec![attempt],
        });
        round_trip(AppMessage::Outdated {
            binary_url: "https://x/runners/binary".into(),
            sha256: "ff".into(),
            contract_hash: "abc".into(),
        });
        round_trip(AppMessage::Assign(Assign {
            attempt,
            task_id: TaskId::new(),
            number: 1,
            task_type: "prepare_media".into(),
            input: json!({"x": [1, 2]}),
            inputs: vec![InputFile {
                slot: "upload".into(),
                url: "https://x/a".into(),
                sha256: "aa".into(),
                size: 10,
            }],
            outputs: vec![OutputSlot {
                slot: "stored".into(),
                url: "https://x/b".into(),
                optional: false,
            }],
            timeout_ms: 1000,
        }));
        round_trip(AppMessage::Cancel { attempt });
        round_trip(AppMessage::Settings(settings));
        round_trip(AppMessage::Drain);
        round_trip(AppMessage::Resume);
        round_trip(AppMessage::Exit { grace_ms: 0 });
    }

    #[test]
    fn message_tags_are_snake_case() {
        let text = serde_json::to_string(&RunnerMessage::Draining).expect("serialize");
        assert_eq!(text, r#"{"type":"draining"}"#);
        let text = serde_json::to_string(&Outcome::TimedOut).expect("serialize");
        assert_eq!(text, r#"{"kind":"timed_out"}"#);
    }

    #[test]
    fn slot_names() {
        assert!(is_valid_slot_name("stored.opus"));
        assert!(is_valid_slot_name("peaks_1"));
        assert!(!is_valid_slot_name(""));
        assert!(!is_valid_slot_name("../x"));
        assert!(!is_valid_slot_name(".hidden"));
        assert!(!is_valid_slot_name("a/b"));
    }
}
