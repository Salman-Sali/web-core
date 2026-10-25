//! The backup archive: a zstd-compressed tar laid out as
//!
//! ```text
//! manifest.json
//! databases/<name>.dump      pg_dump custom format
//! folders/<name>/...         folder contents
//! ```
//!
//! Everything here is blocking.

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::error::{BackupError, Result};

const MANIFEST: &str = "manifest.json";
const FORMAT: u32 = 1;
const ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub format: u32,
    pub app: String,
    pub app_version: Option<String>,
    pub hostname: String,
    pub created_at: String,
    pub pg_dump_version: Option<String>,
    pub databases: Vec<String>,
    pub folders: Vec<String>,
}

impl Manifest {
    pub fn new(
        app: String,
        app_version: Option<String>,
        hostname: String,
        created_at: String,
        pg_dump_version: Option<String>,
        databases: Vec<String>,
        folders: Vec<String>,
    ) -> Self {
        Self {
            format: FORMAT,
            app,
            app_version,
            hostname,
            created_at,
            pg_dump_version,
            databases,
            folders,
        }
    }
}

/// Writes the whole archive into `out` and returns it once the zstd frame is finished.
pub(crate) fn write<W: Write>(
    out: W,
    manifest: &Manifest,
    dumps: &[(String, PathBuf)],
    folders: &[(String, PathBuf)],
) -> Result<W> {
    let encoder = zstd::Encoder::new(out, ZSTD_LEVEL)?;
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);

    let json = serde_json::to_vec_pretty(manifest)
        .map_err(|e| BackupError::Archive(format!("could not serialise manifest: {e}")))?;
    let mut header = tar::Header::new_gnu();
    header.set_size(json.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(chrono::Utc::now().timestamp().max(0) as u64);
    header.set_cksum();
    builder.append_data(&mut header, MANIFEST, json.as_slice())?;

    for (name, path) in dumps {
        builder.append_path_with_name(path, format!("databases/{name}.dump"))?;
    }
    for (name, path) in folders {
        append_folder(&mut builder, path, &Path::new("folders").join(name))?;
    }

    let encoder = builder.into_inner()?;
    Ok(encoder.finish()?)
}

/// Like `Builder::append_dir_all`, but files deleted while the walk is running (normal on a
/// live server) are skipped instead of failing the backup. A missing root is archived empty.
fn append_folder<W: Write>(
    builder: &mut tar::Builder<W>,
    root: &Path,
    archive_root: &Path,
) -> Result<()> {
    match fs::metadata(root) {
        Ok(meta) if meta.is_dir() => builder.append_dir(archive_root, root)?,
        Ok(_) => {
            return Err(BackupError::Archive(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            eprintln!(
                "backup: folder {} does not exist, archiving it as empty",
                root.display()
            );
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o755);
            header.set_mtime(chrono::Utc::now().timestamp().max(0) as u64);
            header.set_cksum();
            builder.append_data(&mut header, archive_root, io::empty())?;
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    }

    let mut stack = vec![(root.to_path_buf(), archive_root.to_path_buf())];
    while let Some((dir, archive_dir)) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let archive_path = archive_dir.join(entry.file_name());
            let file_type = entry.file_type()?;
            let appended = if file_type.is_dir() {
                builder
                    .append_dir(&archive_path, &path)
                    .map(|()| stack.push((path.clone(), archive_path)))
            } else if file_type.is_file() {
                File::open(&path).and_then(|mut file| builder.append_file(&archive_path, &mut file))
            } else if file_type.is_symlink() {
                builder.append_path_with_name(&path, &archive_path)
            } else {
                // sockets, fifos and devices do not belong in an app's storage folder
                continue;
            };
            match appended {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    eprintln!("backup: {} vanished during backup, skipped", path.display());
                }
                other => other?,
            }
        }
    }
    Ok(())
}

pub(crate) fn read_manifest(archive: &Path) -> Result<Manifest> {
    let mut tar = tar::Archive::new(zstd::Decoder::new(File::open(archive)?)?);
    let mut entries = tar.entries()?;
    let mut entry = entries
        .next()
        .ok_or_else(|| BackupError::Archive("archive is empty".into()))??;
    if entry.path()?.as_ref() != Path::new(MANIFEST) {
        return Err(BackupError::Archive(
            "archive does not start with a manifest".into(),
        ));
    }
    let mut json = Vec::new();
    entry.read_to_end(&mut json)?;
    let manifest: Manifest = serde_json::from_slice(&json)
        .map_err(|e| BackupError::Archive(format!("invalid manifest: {e}")))?;
    if manifest.format != FORMAT {
        return Err(BackupError::Archive(format!(
            "unsupported archive format {} (this version reads {FORMAT})",
            manifest.format
        )));
    }
    Ok(manifest)
}

/// Unpacks an archive. Database dumps are written to `dump_dir` and handed to
/// `restore_databases` (as `(name, path)`) before the first folder is touched, so a failed
/// database restore leaves the folders alone. Folder contents go into `folders[name]`.
pub(crate) fn extract(
    archive: &Path,
    dump_dir: &Path,
    folders: &HashMap<String, PathBuf>,
    mut restore_databases: impl FnMut(&[(String, PathBuf)]) -> Result<()>,
) -> Result<()> {
    let mut tar = tar::Archive::new(zstd::Decoder::new(File::open(archive)?)?);
    let mut dumps = Vec::new();
    let mut databases_restored = false;

    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let mut components = path.components();
        let section = components.next();
        let name = components
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .map(str::to_string);
        let rest: PathBuf = components.collect();

        match (section, name) {
            (Some(Component::Normal(s)), _) if s == MANIFEST => {}
            (Some(Component::Normal(s)), Some(file)) if s == "databases" => {
                let name = file
                    .strip_suffix(".dump")
                    .filter(|n| {
                        !n.is_empty() && !n.contains(['/', '\\']) && rest.as_os_str().is_empty()
                    })
                    .ok_or_else(|| {
                        BackupError::Archive(format!("unexpected entry {}", path.display()))
                    })?
                    .to_string();
                let target = dump_dir.join(format!("{name}.dump"));
                io::copy(&mut entry, &mut File::create(&target)?)?;
                dumps.push((name, target));
            }
            (Some(Component::Normal(s)), Some(name)) if s == "folders" => {
                if !databases_restored {
                    restore_databases(&dumps)?;
                    databases_restored = true;
                }
                let root = folders.get(&name).ok_or_else(|| {
                    BackupError::Archive(format!("folder `{name}` is not registered in this app"))
                })?;
                unpack_into(&mut entry, root, &rest)?;
            }
            _ => {
                return Err(BackupError::Archive(format!(
                    "unexpected entry {}",
                    path.display()
                )));
            }
        }
    }

    if !databases_restored {
        restore_databases(&dumps)?;
    }
    Ok(())
}

fn unpack_into<R: Read>(entry: &mut tar::Entry<R>, root: &Path, rest: &Path) -> Result<()> {
    if !rest.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(BackupError::Archive(format!(
            "refusing to unpack unsafe path {}",
            rest.display()
        )));
    }
    fs::create_dir_all(root)?;
    if rest.as_os_str().is_empty() {
        return Ok(());
    }

    let target = root.join(rest);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
        // a symlink unpacked earlier must not lead the write outside the folder
        if !fs::canonicalize(parent)?.starts_with(fs::canonicalize(root)?) {
            return Err(BackupError::Archive(format!(
                "refusing to unpack {} outside {}",
                rest.display(),
                root.display()
            )));
        }
    }
    entry.unpack(&target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest::new(
            "my-api".into(),
            Some("1.2.3".into()),
            "host".into(),
            "2026-10-25T03:00:00Z".into(),
            None,
            vec!["main".into()],
            vec!["uploads".into(), "missing".into()],
        )
    }

    #[test]
    fn round_trips_dumps_and_folders() {
        let src = tempfile::tempdir().unwrap();
        let uploads = src.path().join("uploads");
        fs::create_dir_all(uploads.join("a/b")).unwrap();
        fs::create_dir_all(uploads.join("empty")).unwrap();
        fs::write(uploads.join("top.txt"), "top").unwrap();
        fs::write(uploads.join("a/b/deep.bin"), vec![7u8; 100_000]).unwrap();
        let long_name = "x".repeat(150);
        fs::write(uploads.join("a").join(&long_name), "long").unwrap();
        std::os::unix::fs::symlink("top.txt", uploads.join("link")).unwrap();
        let dump = src.path().join("main.dump");
        fs::write(&dump, "PGDMP fake").unwrap();

        let archive_path = src.path().join("backup.tar.zst");
        let file = write(
            File::create(&archive_path).unwrap(),
            &manifest(),
            &[("main".into(), dump)],
            &[
                ("uploads".into(), uploads),
                ("missing".into(), src.path().join("does-not-exist")),
            ],
        )
        .unwrap();
        drop(file);

        let read = read_manifest(&archive_path).unwrap();
        assert_eq!(read.app, "my-api");
        assert_eq!(read.folders, ["uploads", "missing"]);

        let dst = tempfile::tempdir().unwrap();
        let dump_dir = dst.path().join("dumps");
        fs::create_dir(&dump_dir).unwrap();
        let restored_uploads = dst.path().join("restored/uploads");
        let restored_missing = dst.path().join("restored/missing");
        let folders = HashMap::from([
            ("uploads".to_string(), restored_uploads.clone()),
            ("missing".to_string(), restored_missing.clone()),
        ]);
        let mut seen_dumps = Vec::new();
        extract(&archive_path, &dump_dir, &folders, |dumps| {
            assert!(
                !restored_uploads.exists(),
                "databases must be restored before folders"
            );
            seen_dumps = dumps.to_vec();
            Ok(())
        })
        .unwrap();

        assert_eq!(seen_dumps.len(), 1);
        assert_eq!(fs::read_to_string(&seen_dumps[0].1).unwrap(), "PGDMP fake");
        assert_eq!(
            fs::read_to_string(restored_uploads.join("top.txt")).unwrap(),
            "top"
        );
        assert_eq!(
            fs::read(restored_uploads.join("a/b/deep.bin")).unwrap(),
            vec![7u8; 100_000]
        );
        assert_eq!(
            fs::read_to_string(restored_uploads.join("a").join(&long_name)).unwrap(),
            "long"
        );
        assert!(restored_uploads.join("empty").is_dir());
        assert_eq!(
            fs::read_link(restored_uploads.join("link")).unwrap(),
            Path::new("top.txt")
        );
        assert!(restored_missing.is_dir());
    }

    #[test]
    fn rejects_unregistered_folder() {
        let src = tempfile::tempdir().unwrap();
        let uploads = src.path().join("uploads");
        fs::create_dir(&uploads).unwrap();
        fs::write(uploads.join("file.txt"), "x").unwrap();
        let archive_path = src.path().join("backup.tar.zst");
        write(
            File::create(&archive_path).unwrap(),
            &manifest(),
            &[],
            &[("uploads".into(), uploads)],
        )
        .unwrap();
        let result = extract(&archive_path, src.path(), &HashMap::new(), |_| Ok(()));
        assert!(matches!(result, Err(BackupError::Archive(_))));
    }
}
