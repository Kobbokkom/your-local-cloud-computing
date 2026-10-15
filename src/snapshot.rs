use crate::model::{FileEntry, safe_relative};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Serialize)]
pub struct Excluded {
    pub path: String,
    pub reason: String,
}
pub struct Snapshot {
    pub files: Vec<FileEntry>,
    pub excluded: Vec<Excluded>,
    pub blobs: tempfile::TempDir,
}

fn git_paths(root: &Path, ignored: bool) -> Option<Vec<String>> {
    let mut c = Command::new("git");
    c.arg("-C").arg(root).args(["ls-files", "-z"]);
    if ignored {
        c.args(["--others", "--ignored", "--exclude-standard"]);
    } else {
        c.args(["--cached", "--others", "--exclude-standard"]);
    }
    let o = c.output().ok()?;
    if !o.status.success() {
        return None;
    }
    let text = String::from_utf8(o.stdout).ok()?;
    Some(
        text.split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}
fn in_selection(path: &Path, dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|d| path.starts_with(d))
}

pub fn create(root: &Path, inputs: &[PathBuf], whole: bool) -> Result<Snapshot> {
    let root = root.canonicalize()?;
    if !whole && inputs.is_empty() {
        bail!("select --input files/folders, or explicitly use --project")
    }
    let blobs = tempfile::Builder::new()
        .prefix("ylcc-snapshot-")
        .tempdir()?;
    let mut selected = BTreeSet::new();
    let mut dirs = Vec::new();
    let mut excluded = Vec::new();
    if whole {
        dirs.push(PathBuf::new());
    }
    for p in inputs {
        let abs = if p.is_absolute() {
            p.clone()
        } else {
            root.join(p)
        };
        let rel = abs
            .strip_prefix(&root)
            .context("input is outside project root")?;
        if rel.as_os_str().is_empty() || rel == Path::new(".") {
            dirs.push(PathBuf::new());
            continue;
        }
        let name = rel.to_str().context("input path is not UTF-8")?;
        safe_relative(name)?;
        let meta = fs::symlink_metadata(&abs).with_context(|| format!("input {}", p.display()))?;
        if meta.file_type().is_symlink() {
            bail!("explicit symlink input is not supported: {name}")
        }
        if meta.is_file() {
            selected.insert(name.to_string());
        } else if meta.is_dir() {
            dirs.push(rel.to_path_buf());
        } else {
            bail!("input is not a regular file or directory: {name}")
        }
    }
    if let Some(paths) = git_paths(&root, false) {
        for name in paths {
            if in_selection(Path::new(&name), &dirs) {
                selected.insert(name);
            }
        }
        if let Some(paths) = git_paths(&root, true) {
            for path in paths {
                if in_selection(Path::new(&path), &dirs) && !selected.contains(&path) {
                    excluded.push(Excluded {
                        path,
                        reason: "Git ignore rule".into(),
                    });
                }
            }
        }
    } else {
        for dir in &dirs {
            let mut walk = ignore::WalkBuilder::new(root.join(dir));
            walk.hidden(false).require_git(false).follow_links(false);
            for entry in walk.build() {
                let e = entry?;
                let p = e.path();
                if p == root || e.file_type().is_some_and(|t| t.is_dir()) {
                    continue;
                }
                let name = p
                    .strip_prefix(&root)?
                    .to_str()
                    .context("input path is not UTF-8")?
                    .to_string();
                selected.insert(name);
            }
        }
    }
    let mut files = Vec::new();
    for name in selected {
        safe_relative(&name)?;
        if Path::new(&name)
            .components()
            .any(|c| c.as_os_str() == ".git")
        {
            excluded.push(Excluded {
                path: name,
                reason: "Git internal data".into(),
            });
            continue;
        }
        let path = root.join(&name);
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                excluded.push(Excluded {
                    path: name,
                    reason: "deleted from working tree".into(),
                });
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        if !meta.is_file() || meta.file_type().is_symlink() {
            excluded.push(Excluded {
                path: name,
                reason: "not a regular file; symlinks are not followed".into(),
            });
            continue;
        }
        if !path.canonicalize()?.starts_with(&root) {
            bail!("input escaped root: {name}")
        }
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let before = input.metadata()?;
        let mut tmp = tempfile::NamedTempFile::new_in(blobs.path())?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut buf = [0u8; 65536];
        loop {
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            tmp.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
            size += n as u64;
        }
        let after = input.metadata()?;
        let current = fs::symlink_metadata(&path)?;
        if size != before.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ino() != current.ino()
            || before.len() != current.len()
        {
            bail!("input changed while creating snapshot: {name}; submit again")
        }
        let hash = hex::encode(hasher.finalize());
        let dest = blobs.path().join(&hash);
        if !dest.exists() {
            tmp.persist(&dest)?;
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o400))?;
        }
        files.push(FileEntry {
            path: name,
            hash,
            size,
            mode: before.permissions().mode() & 0o777,
        });
    }
    Ok(Snapshot {
        files,
        excluded,
        blobs,
    })
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn ensure_safe_directory(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(m) => {
                if m.file_type().is_symlink() || !m.is_dir() {
                    bail!(
                        "result directory contains non-directory or symlink: {}",
                        current.display()
                    )
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn git(root: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .status()
                .unwrap()
                .success()
        )
    }
    #[test]
    fn snapshot_tracks_dirty_and_ignores_except_explicit() {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        fs::write(d.path().join(".gitignore"), "ignored\ntracked\n").unwrap();
        fs::write(d.path().join("tracked"), "old").unwrap();
        git(d.path(), &["add", "-f", "tracked"]);
        fs::write(d.path().join("tracked"), "dirty").unwrap();
        fs::write(d.path().join("ignored"), "secret").unwrap();
        fs::write(d.path().join("new"), "new").unwrap();
        let s = create(d.path(), &[], true).unwrap();
        assert!(s.files.iter().any(|f| f.path == "tracked"));
        assert!(!s.files.iter().any(|f| f.path == "ignored"));
        fs::write(d.path().join("tracked"), "later").unwrap();
        let f = s.files.iter().find(|f| f.path == "tracked").unwrap();
        assert_eq!(
            fs::read_to_string(s.blobs.path().join(&f.hash)).unwrap(),
            "dirty"
        );
        let explicit = create(d.path(), &["ignored".into()], false).unwrap();
        assert_eq!(explicit.files.len(), 1);
    }
    #[test]
    fn no_stale_deleted_file_or_symlink() {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        fs::write(d.path().join("gone"), "x").unwrap();
        git(d.path(), &["add", "gone"]);
        fs::remove_file(d.path().join("gone")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", d.path().join("link")).unwrap();
        let s = create(d.path(), &[], true).unwrap();
        assert!(s.files.is_empty());
        assert_eq!(s.excluded.len(), 2);
    }
    #[test]
    fn nested_directory_scope() {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        fs::create_dir(d.path().join("src")).unwrap();
        fs::write(d.path().join("src/a"), "a").unwrap();
        fs::write(d.path().join("other"), "b").unwrap();
        let s = create(d.path(), &["src".into()], false).unwrap();
        assert_eq!(s.files.len(), 1);
        assert_eq!(s.files[0].path, "src/a");
    }
    #[test]
    fn result_parent_symlink_rejected() {
        let d = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/tmp", d.path().join("evil")).unwrap();
        assert!(ensure_safe_directory(&d.path().join("evil/sub")).is_err());
    }
}
