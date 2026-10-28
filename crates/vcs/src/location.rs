//! Where a history lives: the link file, moving a history (copy, verify, switch, then delete) and
//! pointing a folder at an existing one. See [`crate::relocate`] for the order each step keeps.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::imp::{MARKER, MAX_DEPTH, REPO, REPO_LOCK, lock, open, sync_dir, sync_file};
use crate::{Error, FOLDER, History, LINK, Moved, Result};

/// Longest link or owner file read, in bytes.
const MAX_LINK: u64 = 4096;
/// File in a moved or linked repository naming the folder whose documents it serves.
const OWNER: &str = "photocraft-project";
/// Where a copy is built at the destination before it is renamed into place.
const COPYING: &str = "history.git.copying";

/// Points a test can stop at, right after each named step, to act out a crash there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Copied,
    Switched,
}

pub(crate) type Hook<'a> = &'a mut dyn FnMut(Step) -> Result<()>;

/// A small text file's first line, or `None` when it doesn't exist. Only regular files are read:
/// a FIFO in its place would block.
fn read_line(path: &Path) -> Result<Option<String>> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Err(Error::Damaged(format!("{} is not a regular file", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_LINK + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LINK {
        return Err(Error::Damaged(format!("{} is longer than {MAX_LINK} bytes", path.display())));
    }
    let text = String::from_utf8(bytes).map_err(|_| Error::Damaged(format!("{} is not UTF-8 text", path.display())))?;
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return Err(Error::Damaged(format!("{} is empty", path.display())));
    }
    Ok(Some(line.to_owned()))
}

/// Network paths (`\\server\share`, `//server/share`) are refused: looking one up sends the
/// person's credentials to that server on Windows, and a link file can come in a downloaded
/// folder. A share mapped to a drive letter works.
fn check_local(path: &str) -> Result<()> {
    if path.starts_with("\\\\") || path.starts_with("//") {
        return Err(Error::Io(format!("{path} is a network path; map the share to a drive letter and use that instead")));
    }
    Ok(())
}

/// The history `folder`'s link file names (relative paths are read from `folder`).
pub(crate) fn read_link(folder: &Path) -> Result<Option<PathBuf>> {
    let Some(line) = read_line(&folder.join(LINK))? else { return Ok(None) };
    check_local(&line)?;
    Ok(Some(folder.join(line)))
}

/// What a linked history must be before it is read or written: reachable, PhotoCraft's, and used
/// by `folder` alone.
pub(crate) fn check_linked(folder: &Path, dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        return Err(Error::Offline(dir.to_path_buf()));
    }
    if !dir.join(REPO).join(MARKER).is_file() {
        return Err(Error::Damaged(format!("{} holds no PhotoCraft version history; find the history folder or move it again", dir.display())));
    }
    check_not_shared(folder, dir)
}

/// A path to compare by: resolved through links where it exists, else its parent resolved.
fn real(p: &Path) -> PathBuf {
    if let Ok(r) = std::fs::canonicalize(p) {
        return r;
    }
    match (p.parent().and_then(|d| std::fs::canonicalize(d).ok()), p.file_name()) {
        (Some(d), Some(n)) => d.join(n),
        _ => std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()),
    }
}

/// Write a file atomically: a temp file beside it, flushed, then renamed over it.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = path.with_file_name(format!("{name}.tmp"));
    std::fs::write(&tmp, text)?;
    sync_file(&tmp)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Make `folder`'s documents use the history in `dir`: the default folder needs no link.
fn switch(folder: &Path, dir: &Path) -> Result<()> {
    if real(dir) == real(&folder.join(FOLDER)) {
        match std::fs::remove_file(folder.join(LINK)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    } else {
        let abs = std::path::absolute(dir)?;
        let text = abs.to_str().ok_or_else(|| Error::Io(format!("{} is not a UTF-8 path", abs.display())))?;
        check_local(text)?;
        write_atomic(&folder.join(LINK), &format!("{text}\n"))?;
    }
    sync_dir(folder)
}

fn write_owner(repo: &Path, folder: &Path) -> Result<()> {
    let abs = real(folder);
    let text = abs.to_str().ok_or_else(|| Error::Io(format!("{} is not a UTF-8 path", abs.display())))?;
    write_atomic(&repo.join(OWNER), &format!("{text}\n"))
}

/// The folder whose documents `dir`'s history serves, when known.
fn owner_of(dir: &Path) -> Result<Option<PathBuf>> {
    if let Some(line) = read_line(&dir.join(REPO).join(OWNER))? {
        return Ok(Some(PathBuf::from(line)));
    }
    let default = dir.file_name().is_some_and(|n| n == FOLDER);
    Ok(dir.parent().filter(|_| default).map(Path::to_path_buf))
}

/// Refuse when another folder still uses the history in `dir`.
fn check_not_shared(folder: &Path, dir: &Path) -> Result<()> {
    let Some(owner) = owner_of(dir)? else { return Ok(()) };
    if real(&owner) == real(folder) || !owner.is_dir() {
        return Ok(());
    }
    match History::of_folder(&owner) {
        Ok(h) if real(&h.dir) == real(dir) => Err(Error::Shared(owner)),
        _ => Ok(()),
    }
}

/// Every file under `dir` (relative path to path), minus the repository lock. Links are refused:
/// a history holds none, and following one could copy or delete outside it.
fn files(dir: &Path, rel: &str, depth: usize, out: &mut BTreeMap<String, PathBuf>) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Damaged(format!("{} is nested deeper than {MAX_DEPTH} folders", dir.display())));
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|n| Error::Io(format!("file name {n:?} is not UTF-8")))?;
        let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        let ty = entry.file_type()?;
        if ty.is_dir() {
            files(&entry.path(), &child, depth + 1, out)?;
        } else if ty.is_file() {
            if child != REPO_LOCK {
                out.insert(child, entry.path());
            }
        } else {
            return Err(Error::Damaged(format!("{} is a link or a special file; a version history holds none", entry.path().display())));
        }
    }
    Ok(())
}

fn same_bytes(a: &Path, b: &Path) -> Result<bool> {
    let (mut fa, mut fb) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let n = fa.read(&mut ba)?;
        if n == 0 {
            return Ok(true);
        }
        let got = ba.get(..n).unwrap_or_default();
        let other = bb.get_mut(..n).ok_or_else(|| Error::Io("read past the buffer".into()))?;
        fb.read_exact(other)?;
        if got != other {
            return Ok(false);
        }
    }
}

/// Copy the repository `src` into `copy`, flushing every file and folder, then check every byte.
fn copy_verified(src: &Path, copy: &Path, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<()> {
    let mut list = BTreeMap::new();
    files(src, "", 0, &mut list)?;
    let mut total = 0u64;
    for p in list.values() {
        total = total.saturating_add(std::fs::metadata(p)?.len());
    }
    std::fs::create_dir_all(copy)?;
    let mut done = 0u64;
    let mut dirs = vec![copy.to_path_buf()];
    for (rel, from) in &list {
        let to = copy.join(rel);
        if let Some(parent) = to.parent()
            && !parent.is_dir()
        {
            std::fs::create_dir_all(parent)?;
            dirs.push(parent.to_path_buf());
        }
        done = done.saturating_add(std::fs::copy(from, &to)?);
        sync_file(&to)?;
        if !progress(done, total) {
            return Err(Error::Cancelled);
        }
    }
    for d in dirs.iter().rev() {
        sync_dir(d)?;
    }
    let mut copied = BTreeMap::new();
    files(copy, "", 0, &mut copied)?;
    if copied.keys().ne(list.keys()) {
        return Err(Error::Io(format!("the copy in {} does not match the history; nothing was moved", copy.display())));
    }
    for (rel, from) in &list {
        if !same_bytes(from, &copy.join(rel))? {
            return Err(Error::Io(format!("{rel} differs after copying to {}; nothing was moved", copy.display())));
        }
    }
    Ok(())
}

/// `dest` exists as a folder holding nothing but an interrupted copy (cleared), or is created. A
/// complete copy this folder owns but never switched to (a crash before the link moved) is an
/// unused duplicate of the history in use, so it is cleared too.
fn prepare_dest(dest: &Path, folder: &Path) -> Result<()> {
    if !dest.exists() {
        std::fs::create_dir_all(dest)?;
        return Ok(());
    }
    if !dest.is_dir() {
        return Err(Error::Io(format!("{} is a file; choose an empty folder", dest.display())));
    }
    let unused_copy = owner_of(dest).ok().flatten().is_some_and(|o| real(&o) == real(folder));
    for entry in std::fs::read_dir(dest)? {
        let name = entry?.file_name();
        if name != COPYING && !(name == REPO && unused_copy) {
            return Err(Error::Io(format!("{} is not empty; choose an empty folder for the version history", dest.display())));
        }
    }
    for part in [COPYING, REPO] {
        let leftover = dest.join(part);
        if leftover.exists() {
            std::fs::remove_dir_all(&leftover)?;
        }
    }
    Ok(())
}

/// Delete what PhotoCraft keeps in an old history folder; the folder itself when that empties it.
fn delete_old(dir: &Path) -> std::io::Result<()> {
    for part in [REPO, "stage"] {
        let p = dir.join(part);
        if p.exists() {
            std::fs::remove_dir_all(p)?;
        }
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        // Scratch copies of restored versions, left by a crash mid-restore.
        if entry.file_name().to_str().is_some_and(|n| n.starts_with("restore-")) {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    std::fs::remove_dir(dir)
}

pub(crate) fn relocate(folder: &Path, dest: &Path, progress: &mut dyn FnMut(u64, u64) -> bool, hook: Hook) -> Result<Moved> {
    let from = History::of_folder(folder)?;
    let dest = std::path::absolute(dest)?;
    let (from_real, dest_real) = (real(&from.dir), real(&dest));
    if dest_real.starts_with(&from_real) || from_real.starts_with(&dest_real) {
        return Err(Error::Io(format!("{} is the version history or holds it; choose a folder outside it", dest.display())));
    }
    let linked = real(&dest) != real(&folder.join(FOLDER));
    let to = History { dir: dest.clone(), link_of: linked.then(|| folder.to_path_buf()) };
    if let Some(text) = dest.to_str()
        && linked
    {
        check_local(text)?;
    }
    let Some(_) = open(&from, false)? else {
        // Nothing to move yet: start the history at its new place.
        prepare_dest(&dest, folder)?;
        open(&History::at(&dest), true)?;
        write_owner(&dest.join(REPO), folder)?;
        switch(folder, &dest)?;
        return Ok(Moved { history: to, leftover: None });
    };
    let src = from.dir.join(REPO);
    prepare_dest(&dest, folder)?;
    {
        // Held through the switch, so no version lands in the old copy after it was copied.
        let _repo_lock = lock(&src.join(REPO_LOCK), "the version history")?;
        let copy = dest.join(COPYING);
        if let Err(e) = copy_verified(&src, &copy, progress) {
            let _ = std::fs::remove_dir_all(&copy);
            return Err(e);
        }
        write_owner(&copy, folder)?;
        std::fs::rename(&copy, dest.join(REPO))?;
        sync_dir(&dest)?;
        hook(Step::Copied)?;
        switch(folder, &dest)?;
    }
    hook(Step::Switched)?;
    let leftover = delete_old(&from.dir).err().map(|_| from.dir.clone());
    Ok(Moved { history: to, leftover })
}

pub(crate) fn relink(folder: &Path, dir: &Path) -> Result<History> {
    let dir = std::path::absolute(dir)?;
    let repo = dir.join(REPO);
    if !repo.join(MARKER).is_file() {
        return Err(Error::Io(format!("{} holds no PhotoCraft version history", dir.display())));
    }
    check_not_shared(folder, &dir)?;
    write_owner(&repo, folder)?;
    switch(folder, &dir)?;
    History::of_folder(folder)
}

#[cfg(test)]
pub(crate) mod test_hooks {
    pub(crate) use super::{Step, relocate as relocate_with};
}
