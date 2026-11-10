//! Build the merged tree a plan describes, in a staging directory outside
//! the library.
//!
//! The staging directory starts as a copy of the staged upstream tree (so
//! files outside the content scope, such as `.gitignore`, come from upstream
//! exactly as a plain update would bring them), and the plan then decides
//! every content path:
//! - kept and locally changed files are the library's bytes;
//! - upstream's files stay as upstream staged them;
//! - text changed differently on both sides goes through
//!   [`merge_text`]: CRLF folded to LF on the three inputs, git2
//!   `merge_trees` on an in-memory object database with rename detection
//!   off, CRLF restored afterwards when the library file was uniformly CRLF;
//! - everything else that needs a decision is a conflict.
//!
//! Nothing here touches the library. A conflict never produces a file with
//! markers: libgit2 reports conflicts in the index, and a conflicted path is
//! left as upstream had it in the staging directory, which is never swapped
//! in while a conflict is unresolved.

use anyhow::{Context, Result};
use git2::{MergeOptions, Odb, Oid, Repository};
use std::collections::HashMap;
use std::path::Path;

use super::plan::{FileAction, MergePlan};
use super::{raw_sha256, ConflictKind, MergeConflict};
use crate::core::content_hash;

/// The pristine baseline content of the paths a merge needs.
#[derive(Debug, Clone)]
pub enum BaseContent {
    /// Raw bytes by path, each verified against the baseline's folded hash.
    Files(HashMap<String, Vec<u8>>),
    /// The baseline revision is gone upstream ("not our ref"): the base is
    /// unrecoverable, so each such file needs a person.
    Gone(String),
    /// The base could not be had now (the fetch failed for another reason).
    /// Retrying the update may work.
    Unavailable(String),
}

impl BaseContent {
    pub fn none_needed() -> Self {
        Self::Files(HashMap::new())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeResult {
    /// No conflicts: the staging directory is the merged tree.
    pub clean: bool,
    pub conflicts: Vec<MergeConflict>,
    /// Paths three-way merged cleanly.
    pub merged: Vec<String>,
}

/// Build `plan` into `staging`, which must not exist yet.
pub fn build(
    plan: &MergePlan,
    base: &BaseContent,
    local: &Path,
    upstream: &Path,
    staging: &Path,
) -> Result<MergeResult> {
    if staging.exists() {
        anyhow::bail!("staging directory already exists: {}", staging.display());
    }
    copy_tree(upstream, staging)?;

    let mut result = MergeResult::default();
    for entry in &plan.entries {
        let path = entry.path.as_str();
        match entry.action {
            FileAction::Keep | FileAction::KeepLocal | FileAction::TakeEither => {
                copy_file(&local.join(path), &staging.join(path))?;
            }
            FileAction::TakeUpstream | FileAction::Delete | FileAction::BothDeleted => {}
            FileAction::StaysDeleted => remove_file(staging, path)?,
            FileAction::Merge => {
                let kind = match base {
                    BaseContent::Files(files) => match files.get(path) {
                        Some(base_bytes) => {
                            let local_bytes = read(local, path)?;
                            match merge_text(base_bytes, &local_bytes, &read(upstream, path)?)? {
                                Some(merged) => {
                                    let merged = if uses_crlf(&local_bytes) {
                                        to_crlf(&merged)
                                    } else {
                                        merged
                                    };
                                    write_file(&staging.join(path), &merged)?;
                                    result.merged.push(path.to_string());
                                    continue;
                                }
                                None => ConflictKind::Text,
                            }
                        }
                        None => ConflictKind::BaseUnavailable,
                    },
                    BaseContent::Gone(_) => ConflictKind::BaseGone,
                    BaseContent::Unavailable(_) => ConflictKind::BaseUnavailable,
                };
                result
                    .conflicts
                    .push(conflict(path, kind, local, upstream)?);
            }
            FileAction::Hold(kind) => result
                .conflicts
                .push(conflict(path, kind, local, upstream)?),
        }
    }
    result.clean = result.conflicts.is_empty();
    Ok(result)
}

fn conflict(
    path: &str,
    kind: ConflictKind,
    local: &Path,
    upstream: &Path,
) -> Result<MergeConflict> {
    Ok(MergeConflict {
        path: path.to_string(),
        kind,
        local_sha256: sha_of(local, path)?,
        upstream_sha256: sha_of(upstream, path)?,
    })
}

/// Raw SHA-256 of `dir/path`, `None` when the file is absent.
pub(crate) fn sha_of(dir: &Path, path: &str) -> Result<Option<String>> {
    let file = dir.join(path);
    match std::fs::read(&file) {
        Ok(bytes) => Ok(Some(raw_sha256(&bytes))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read {}", file.display())),
    }
}

/// Three-way merge of one text file. CRLF is folded to LF on all three
/// inputs; the result uses LF. `None` means the edits clash.
///
/// git2 0.19 has no safe single-file merge, so each input becomes a
/// one-entry tree in an in-memory object database and the trees are merged
/// with rename detection off (§11.3, decision 5). Nothing reaches the disk.
pub fn merge_text(base: &[u8], local: &[u8], upstream: &[u8]) -> Result<Option<Vec<u8>>> {
    const NAME: &str = "file";
    let odb = Odb::new()?;
    odb.add_new_mempack_backend(1)?;
    let repo = Repository::from_odb(odb)?;
    let tree = |content: &[u8]| -> Result<Oid> {
        let blob = repo.blob(&content_hash::fold_crlf(content))?;
        let mut builder = repo.treebuilder(None)?;
        builder.insert(NAME, blob, 0o100644)?;
        Ok(builder.write()?)
    };
    let (base, local, upstream) = (tree(base)?, tree(local)?, tree(upstream)?);
    let mut opts = MergeOptions::new();
    opts.find_renames(false);
    let index = repo.merge_trees(
        &repo.find_tree(base)?,
        &repo.find_tree(local)?,
        &repo.find_tree(upstream)?,
        Some(&opts),
    )?;
    if index.has_conflicts() {
        return Ok(None);
    }
    let entry = index
        .get_path(Path::new(NAME), 0)
        .context("the merged file is missing from the merge result")?;
    let merged = repo.find_blob(entry.id)?.content().to_vec();
    Ok(Some(merged))
}

/// Every line ends in CRLF (and there is at least one line ending).
pub(crate) fn uses_crlf(content: &[u8]) -> bool {
    let lf = content.iter().filter(|&&b| b == b'\n').count();
    lf > 0 && content.windows(2).filter(|w| *w == b"\r\n").count() == lf
}

/// LF to CRLF. The input comes from folded text, so it has no CRLF of its own.
pub(crate) fn to_crlf(content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len() + content.len() / 32);
    for &byte in content {
        if byte == b'\n' {
            out.push(b'\r');
        }
        out.push(byte);
    }
    out
}

fn read(dir: &Path, path: &str) -> Result<Vec<u8>> {
    let file = dir.join(path);
    std::fs::read(&file).with_context(|| format!("failed to read {}", file.display()))
}

pub(crate) fn write_file(dest: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, content).with_context(|| format!("failed to write {}", dest.display()))
}

/// Copy keeping the permissions (the executable bit on Unix).
pub(crate) fn copy_file(src: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dest)
        .with_context(|| format!("failed to copy {} to {}", src.display(), dest.display()))?;
    Ok(())
}

/// Remove `root/path` and any directories that become empty, up to `root`.
pub(crate) fn remove_file(root: &Path, path: &str) -> Result<()> {
    let file = root.join(path);
    match std::fs::remove_file(&file) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to remove {}", file.display()))
        }
    }
    let mut dir = file.parent();
    while let Some(current) = dir {
        if current == root || !current.starts_with(root) {
            break;
        }
        if std::fs::remove_dir(current).is_err() {
            break; // not empty
        }
        dir = current.parent();
    }
    Ok(())
}

/// Copy a staged tree. It comes from the installer, which already dropped
/// `.git` and every symlink; symlinks are skipped here all the same.
pub(crate) fn copy_tree(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in
        std::fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))?
    {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = dest.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            copy_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}
