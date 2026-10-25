use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("invalid backup configuration: {0}")]
    Config(String),
    #[error("postgres: {0}")]
    Postgres(String),
    #[error("s3: {0}")]
    S3(String),
    #[error("email: {0}")]
    Email(String),
    #[error("archive: {0}")]
    Archive(String),
    #[error("restore refused: {0}")]
    Refused(String),
    #[error("another backup or restore is already running (lock file {0})")]
    Locked(PathBuf),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, BackupError>;
