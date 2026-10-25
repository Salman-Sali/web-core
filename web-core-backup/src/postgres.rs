//! Thin wrappers around `pg_dump`, `pg_restore` and `psql`. All of these block.

use std::{
    path::Path,
    process::{Command, Output, Stdio},
};

use crate::error::{BackupError, Result};

/// A connection string with any password moved out of the command line (where other users
/// could read it from `/proc`) and into `PGPASSWORD`.
pub(crate) struct PgConn {
    dbname: String,
    password: Option<String>,
}

impl PgConn {
    pub fn parse(connection_string: &str) -> Self {
        let is_uri = connection_string.starts_with("postgres://")
            || connection_string.starts_with("postgresql://");
        if is_uri && let Ok(mut url) = url::Url::parse(connection_string) {
            let password = url.password().map(|p| {
                percent_encoding::percent_decode_str(p)
                    .decode_utf8_lossy()
                    .into_owned()
            });
            if password.is_some() {
                let _ = url.set_password(None);
                return Self {
                    dbname: url.to_string(),
                    password,
                };
            }
        }
        Self {
            dbname: connection_string.to_string(),
            password: None,
        }
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.stdin(Stdio::null());
        if let Some(password) = &self.password {
            command.env("PGPASSWORD", password);
        }
        command
    }
}

fn run(mut command: Command, what: &str) -> Result<Output> {
    let output = command
        .output()
        .map_err(|e| BackupError::Postgres(format!("could not run {what}: {e}")))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(BackupError::Postgres(format!(
            "{what} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Full `--version` line of a Postgres client tool, e.g. `pg_dump (PostgreSQL) 16.4`.
pub(crate) fn tool_version(tool: &str) -> Result<String> {
    let output = Command::new(tool).arg("--version").output().map_err(|e| {
        BackupError::Config(format!(
            "`{tool}` is not runnable ({e}); install postgresql-client on this server"
        ))
    })?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(crate) fn tool_major(tool: &str) -> Result<u32> {
    let version = tool_version(tool)?;
    parse_tool_major(&version).ok_or_else(|| {
        BackupError::Config(format!("could not read the version of `{tool}`: {version}"))
    })
}

pub(crate) fn parse_tool_major(version: &str) -> Option<u32> {
    let mut words = version.split_whitespace();
    words.find(|w| *w == "(PostgreSQL)")?;
    leading_number(words.next()?)
}

fn leading_number(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn query(conn: &PgConn, sql: &str) -> Result<String> {
    let mut command = conn.command("psql");
    command
        .args(["-X", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-c", sql])
        .arg(format!("--dbname={}", conn.dbname));
    let output = run(command, "psql")?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(crate) fn server_major(conn: &PgConn) -> Result<u32> {
    let version = query(conn, "SHOW server_version_num")?;
    let num: u32 = version
        .parse()
        .map_err(|_| BackupError::Postgres(format!("unexpected server_version_num `{version}`")))?;
    Ok(num / 10000)
}

/// Dumps every schema of the database in custom format, uncompressed (the archive is
/// compressed as a whole), without ownership or grants.
pub(crate) fn dump(conn: &PgConn, file: &Path) -> Result<()> {
    let mut command = conn.command("pg_dump");
    command
        .args(["--format=custom", "-Z0", "--no-owner", "--no-privileges"])
        .arg("--file")
        .arg(file)
        .arg(format!("--dbname={}", conn.dbname));
    run(command, "pg_dump")?;
    Ok(())
}

pub(crate) fn restore(conn: &PgConn, file: &Path) -> Result<()> {
    let mut command = conn.command("pg_restore");
    command
        .args([
            "--exit-on-error",
            "--single-transaction",
            "--no-owner",
            "--no-privileges",
        ])
        .arg(format!("--dbname={}", conn.dbname))
        .arg(file);
    run(command, "pg_restore")?;
    Ok(())
}

/// Counts user-created objects in any schema. Zero means a freshly created database.
pub(crate) fn user_object_count(conn: &PgConn) -> Result<u64> {
    const SQL: &str = r"
SELECT
  (SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
      AND n.nspname NOT LIKE 'pg\_toast%' AND n.nspname NOT LIKE 'pg\_temp\_%')
+ (SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname NOT IN ('pg_catalog', 'information_schema'))
+ (SELECT count(*) FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
    WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
      AND t.typtype IN ('e', 'd', 'r'))
+ (SELECT count(*) FROM pg_namespace
    WHERE nspname NOT IN ('public', 'pg_catalog', 'information_schema', 'pg_toast')
      AND nspname NOT LIKE 'pg\_temp\_%' AND nspname NOT LIKE 'pg\_toast\_temp\_%')
+ (SELECT count(*) FROM pg_extension WHERE extname <> 'plpgsql')";
    let count = query(conn, SQL)?;
    count
        .parse()
        .map_err(|_| BackupError::Postgres(format!("unexpected object count `{count}`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_password_out_of_uri() {
        let conn =
            PgConn::parse("postgres://app:p%40ss%3Aword@db.internal:5432/app?sslmode=require");
        assert_eq!(
            conn.dbname,
            "postgres://app@db.internal:5432/app?sslmode=require"
        );
        assert_eq!(conn.password.as_deref(), Some("p@ss:word"));
    }

    #[test]
    fn leaves_other_connection_strings_alone() {
        for s in [
            "postgres://app@localhost/app",
            "host=localhost dbname=app user=app",
        ] {
            let conn = PgConn::parse(s);
            assert_eq!(conn.dbname, s);
            assert!(conn.password.is_none());
        }
    }

    #[test]
    fn parses_tool_versions() {
        assert_eq!(
            parse_tool_major("pg_dump (PostgreSQL) 16.15 (Ubuntu 16.15-0ubuntu0.24.04.1)"),
            Some(16)
        );
        assert_eq!(parse_tool_major("pg_restore (PostgreSQL) 9.6.24"), Some(9));
        assert_eq!(parse_tool_major("psql (PostgreSQL) 18beta1"), Some(18));
        assert_eq!(parse_tool_major("something else"), None);
    }
}
