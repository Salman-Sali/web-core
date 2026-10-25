use crate::{backup, config::Backup, notify, restore, s3, sys};

#[derive(Debug, PartialEq)]
enum Command {
    Backup,
    List {
        host: Option<String>,
    },
    Restore {
        timestamp: String,
        host: Option<String>,
    },
}

const USAGE: &str = "usage:
  <app> backup
  <app> restore --list [--host <hostname>]
  <app> restore <timestamp> [--host <hostname>]";

/// `None` when the arguments are not ours, so the app starts normally.
fn parse(args: &[String]) -> Option<Result<Command, String>> {
    let (first, rest) = args.split_first()?;
    match first.as_str() {
        "backup" if rest.is_empty() => Some(Ok(Command::Backup)),
        "backup" => Some(Err("`backup` takes no arguments".into())),
        "restore" => Some(parse_restore(rest)),
        _ => None,
    }
}

fn parse_restore(args: &[String]) -> Result<Command, String> {
    let mut list = false;
    let mut host = None;
    let mut timestamp = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--list" => list = true,
            "--host" => {
                let value = args.next().ok_or("`--host` needs a hostname")?;
                let valid = !value.is_empty()
                    && value
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
                if !valid {
                    return Err(format!("`{value}` is not a hostname"));
                }
                host = Some(value.clone());
            }
            value if !value.starts_with('-') && timestamp.is_none() => {
                if sys::parse_timestamp(value).is_none() {
                    return Err(format!(
                        "`{value}` is not a backup timestamp (like 2026-10-25T03-00-00Z); see `restore --list`"
                    ));
                }
                timestamp = Some(value.to_string());
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    match (list, timestamp) {
        (true, None) => Ok(Command::List { host }),
        (false, Some(timestamp)) => Ok(Command::Restore { timestamp, host }),
        (true, Some(_)) => Err("use either `--list` or a timestamp, not both".into()),
        (false, None) => Err("`restore` needs a timestamp or `--list`".into()),
    }
}

impl Backup {
    /// Handles `<app> backup` and `<app> restore ...`, then exits the process with 0 on
    /// success or 1 on failure. With any other arguments it returns at once.
    ///
    /// Call it at the top of `main`, before connecting to databases, running migrations or
    /// binding a port, so the subcommands never start the app itself.
    pub async fn handle_cli(&self) {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let Some(command) = parse(&args) else {
            return;
        };
        let command = match command {
            Ok(command) => command,
            Err(e) => {
                eprintln!("{e}\n\n{USAGE}");
                std::process::exit(2);
            }
        };
        let code = if self.run_command(command).await {
            0
        } else {
            1
        };
        std::process::exit(code);
    }

    async fn run_command(&self, command: Command) -> bool {
        sys::lower_priority();
        let cfg = &self.cfg;
        let sdk = s3::sdk_config(&cfg.s3).await;
        let this_host = sys::hostname();

        match command {
            Command::Backup => match backup::run(cfg.clone(), &sdk).await {
                Ok(report) => {
                    let message = format!(
                        "Backup of {} on {this_host} uploaded to s3://{}/{} ({:.1} MiB) in {}s.",
                        cfg.app,
                        cfg.s3.bucket,
                        report.key,
                        report.size as f64 / (1024.0 * 1024.0),
                        report.elapsed.as_secs()
                    );
                    println!("backup: {message}");
                    if cfg.notify.as_ref().is_some_and(|n| n.on_success) {
                        let subject = format!("[{}] backup succeeded on {this_host}", cfg.app);
                        notify::send_or_log(&sdk, cfg, subject, message).await;
                    }
                    true
                }
                Err(e) => {
                    eprintln!("backup failed: {e}");
                    let subject = format!("[{}] backup FAILED on {this_host}", cfg.app);
                    let text = format!(
                        "The backup of {} on {this_host} failed at {}.\n\n{e}",
                        cfg.app,
                        chrono::Utc::now().to_rfc3339()
                    );
                    notify::send_or_log(&sdk, cfg, subject, text).await;
                    false
                }
            },
            Command::List { host } => {
                let host = host.unwrap_or(this_host);
                match restore::list(cfg, &sdk, &host).await {
                    Ok(timestamps) if timestamps.is_empty() => {
                        println!(
                            "no backups for {host} in s3://{}/{}",
                            cfg.s3.bucket,
                            s3::host_prefix(&cfg.s3, &host)
                        );
                        true
                    }
                    Ok(timestamps) => {
                        for timestamp in timestamps {
                            println!("{timestamp}");
                        }
                        true
                    }
                    Err(e) => {
                        eprintln!("could not list backups: {e}");
                        false
                    }
                }
            }
            Command::Restore { timestamp, host } => {
                let host = host.unwrap_or(this_host);
                match restore::run(cfg.clone(), &sdk, &timestamp, &host).await {
                    Ok(()) => {
                        println!("restore: done");
                        true
                    }
                    Err(e) => {
                        eprintln!("restore failed: {e}");
                        false
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn ignores_other_arguments() {
        assert_eq!(parse(&args("")), None);
        assert_eq!(parse(&args("--port 8080")), None);
        assert_eq!(parse(&args("serve")), None);
    }

    #[test]
    fn parses_commands() {
        assert_eq!(parse(&args("backup")), Some(Ok(Command::Backup)));
        assert_eq!(
            parse(&args("restore --list")),
            Some(Ok(Command::List { host: None }))
        );
        assert_eq!(
            parse(&args("restore --list --host old-box.internal")),
            Some(Ok(Command::List {
                host: Some("old-box.internal".into())
            }))
        );
        assert_eq!(
            parse(&args("restore 2026-10-25T03-00-00Z --host old-box")),
            Some(Ok(Command::Restore {
                timestamp: "2026-10-25T03-00-00Z".into(),
                host: Some("old-box".into())
            }))
        );
    }

    #[test]
    fn rejects_bad_commands() {
        for bad in [
            "backup now",
            "restore",
            "restore yesterday",
            "restore --list 2026-10-25T03-00-00Z",
            "restore --host",
            "restore --host ../x --list",
            "restore 2026-10-25T03-00-00Z --force",
        ] {
            assert!(matches!(parse(&args(bad)), Some(Err(_))), "{bad}");
        }
    }
}
