//! Native implementation on gitoxide. See the crate docs for the rules each step keeps.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use gix::ObjectId;
use gix::objs::tree::EntryKind;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

use crate::{Error, History, Result, Saved, Version, check_name};

/// The bare repository inside the history folder.
const REPO: &str = "history.git";
/// File in the repository that marks it as PhotoCraft's (others are never written to).
const MARKER: &str = "photocraft-versions";
/// Held in the repository for a whole save, so two windows can't interleave.
const REPO_LOCK: &str = "photocraft-versions.lock";
/// The bundle's own lock, the one `photocraft-format` holds while it saves.
const SAVE_LOCK: &str = "save.lock";
const LOCK_WAIT: Duration = if cfg!(test) { Duration::from_millis(200) } else { Duration::from_secs(10) };
const MAX_DEPTH: usize = 32;
/// Commits walked at most when listing or checking a version id.
const MAX_WALK: usize = 100_000;
/// Longest version message kept, in bytes.
const MAX_MESSAGE: usize = 4096;

/// Points a test can stop at, right after each named step, to act out a crash there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Blobs,
    Tree,
    CommitObject,
}

pub(crate) type Hook<'a> = &'a mut dyn FnMut(Step) -> Result<()>;

/// Turn any error into ours, naming a full disk as such (gitoxide's own message hides it).
fn classify(e: &(dyn std::error::Error + 'static)) -> Error {
    let mut cur = Some(e);
    while let Some(c) = cur {
        if let Some(io) = c.downcast_ref::<std::io::Error>()
            && is_disk_full(io)
        {
            return Error::DiskFull(io.to_string());
        }
        cur = c.source();
    }
    Error::Io(e.to_string())
}

fn is_disk_full(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::StorageFull || e.kind() == std::io::ErrorKind::QuotaExceeded
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        if is_disk_full(&e) { Error::DiskFull(e.to_string()) } else { Error::Io(e.to_string()) }
    }
}

trait GitResult<T> {
    fn git(self) -> Result<T>;
}

impl<T, E: std::error::Error + 'static> GitResult<T> for std::result::Result<T, E> {
    fn git(self) -> Result<T> {
        self.map_err(|e| classify(&e))
    }
}

/// Take an OS file lock, waiting up to [`LOCK_WAIT`]. `None` where the file system has no locks.
fn lock(path: &Path, what: &str) -> Result<Option<File>> {
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(TryLockError::WouldBlock) if start.elapsed() < LOCK_WAIT => std::thread::sleep(Duration::from_millis(20)),
            Err(TryLockError::WouldBlock) => return Err(Error::Busy(what.to_owned())),
            Err(TryLockError::Error(_)) => return Ok(None),
        }
    }
}

fn sync_file(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

/// Flush a folder's entries. Windows can't open folders for this and journals them itself.
fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    sync_file(path)?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn open_isolated(path: &Path) -> Result<gix::Repository> {
    gix::open_opts(path, gix::open::Options::isolated()).git()
}

/// Open the history's repository; create it (atomically, marked as ours) when `create`.
fn open(h: &History, create: bool) -> Result<Option<gix::Repository>> {
    let repo_dir = h.dir.join(REPO);
    if repo_dir.exists() {
        if !repo_dir.join(MARKER).is_file() {
            return Err(Error::ForeignRepository(repo_dir));
        }
        return open_isolated(&repo_dir).map(Some);
    }
    if !create {
        return Ok(None);
    }
    // Build it under another name and rename it in once marked, so a crash never leaves an
    // unmarked repository that later saves would refuse.
    std::fs::create_dir_all(&h.dir)?;
    let staging = h.dir.join(format!("{REPO}.init"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    gix::ThreadSafeRepository::init_opts(&staging, gix::create::Kind::Bare, gix::create::Options::default(), gix::open::Options::isolated()).git()?;
    std::fs::write(staging.join(MARKER), b"Versions kept by PhotoCraft. Other tools may read this history; PhotoCraft only writes to it.\n")?;
    // A git with core.autocrlf must never rewrite tile or manifest bytes on checkout.
    std::fs::create_dir_all(staging.join("info"))?;
    std::fs::write(staging.join("info").join("attributes"), b"* -text -diff\n")?;
    sync_file(&staging.join(MARKER))?;
    sync_file(&staging.join("info").join("attributes"))?;
    std::fs::rename(&staging, &repo_dir)?;
    sync_dir(&h.dir)?;
    open_isolated(&repo_dir).map(Some)
}

fn branch(repo: &gix::Repository) -> Result<FullName> {
    repo.head_name().git()?.ok_or_else(|| Error::Damaged("the version history is not on a branch (detached HEAD)".into()))
}

fn head(repo: &gix::Repository, branch: &FullName) -> Result<Option<ObjectId>> {
    Ok(repo.try_find_reference(branch.as_ref()).git()?.map(|r| r.id().detach()))
}

fn commit_tree(repo: &gix::Repository, commit: ObjectId) -> Result<ObjectId> {
    Ok(repo.find_commit(commit).git()?.tree_id().git()?.detach())
}

/// The document's subtree inside a root tree, if the version has it.
fn doc_tree(repo: &gix::Repository, root_tree: ObjectId, name: &str) -> Result<Option<ObjectId>> {
    let tree = repo.find_tree(root_tree).git()?;
    let decoded = tree.decode().git()?;
    Ok(decoded.entries.iter().find(|e| e.mode.is_tree() && e.filename == name).map(|e| e.oid.to_owned()))
}

fn version(repo: &gix::Repository, id: ObjectId) -> Result<Version> {
    let commit = repo.find_commit(id).git()?;
    let message = String::from_utf8_lossy(commit.message_raw_sloppy()).trim_end().to_owned();
    let time = commit.time().git()?.seconds;
    Ok(Version { id: id.to_string(), message, time })
}

/// Files that belong in a version: everything but the save lock and the redrawable preview.
fn walk(dir: &Path, rel: &str, depth: usize, out: &mut BTreeMap<String, PathBuf>) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Damaged(format!("{} is nested deeper than {MAX_DEPTH} folders", dir.display())));
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|n| Error::Io(format!("file name {n:?} is not UTF-8")))?;
        let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        let ty = entry.file_type()?;
        if ty.is_dir() {
            if child != "composite" {
                walk(&entry.path(), &child, depth + 1, out)?;
            }
        } else if ty.is_file() && child != SAVE_LOCK {
            out.insert(child, entry.path());
        }
    }
    Ok(())
}

/// Every id in a tree (subtrees and blobs), reading only tree objects.
fn tree_ids(repo: &gix::Repository, tree: ObjectId) -> Result<HashSet<ObjectId>> {
    let (mut stack, mut ids) = (vec![tree], HashSet::new());
    while let Some(t) = stack.pop() {
        if !ids.insert(t) {
            continue;
        }
        for e in repo.find_tree(t).git()?.decode().git()?.entries.iter() {
            if e.mode.is_tree() {
                stack.push(e.oid.to_owned());
            } else {
                ids.insert(e.oid.to_owned());
            }
        }
    }
    Ok(ids)
}

/// Flush loose objects and their folders before any branch points at them.
fn sync_objects(repo: &gix::Repository, ids: impl IntoIterator<Item = ObjectId>) -> Result<()> {
    let objects = repo.git_dir().join("objects");
    let mut dirs = BTreeSet::new();
    for id in ids {
        let hex = id.to_string();
        let (Some(fan), Some(rest)) = (hex.get(..2), hex.get(2..)) else { continue };
        let file = objects.join(fan).join(rest);
        if file.is_file() {
            sync_file(&file)?;
            dirs.insert(objects.join(fan));
        }
    }
    for dir in &dirs {
        sync_dir(dir)?;
    }
    sync_dir(&objects)
}

/// Flush the branch file, its reflog and their folders after the branch moved.
fn sync_ref(repo: &gix::Repository, branch: &FullName) -> Result<()> {
    let git_dir = repo.git_dir();
    let rel = branch.as_bstr().to_string();
    for path in [git_dir.join(&rel), git_dir.join("logs").join(&rel)] {
        if path.is_file() {
            sync_file(&path)?;
            if let Some(parent) = path.parent() {
                sync_dir(parent)?;
            }
        }
    }
    sync_dir(git_dir)
}

fn signature() -> gix::actor::Signature {
    gix::actor::Signature { name: "PhotoCraft".into(), email: "versions@photocraft.invalid".into(), time: gix::date::Time::now_local_or_utc() }
}

/// Move `branch` from `expected` to `new`; refused if another writer moved it meanwhile.
fn move_branch(repo: &gix::Repository, branch: &FullName, expected: Option<ObjectId>, new: ObjectId) -> Result<()> {
    let prev = match expected {
        Some(old) => PreviousValue::MustExistAndMatch(Target::Object(old)),
        None => PreviousValue::MustNotExist,
    };
    let edit = RefEdit {
        change: Change::Update {
            log: LogChange { mode: RefLog::AndReference, force_create_reflog: true, message: "photocraft: version".into() },
            expected: prev,
            new: Target::Object(new),
        },
        name: branch.clone(),
        deref: false,
    };
    // The repository is opened isolated, so no user.name exists: name the reflog's committer here.
    let sig = signature();
    let mut time = gix::date::parse::TimeBuf::default();
    match repo.edit_references_as([edit], Some(sig.to_ref(&mut time))) {
        Ok(_) => Ok(()),
        Err(_) if head(repo, branch)? != expected => Err(Error::Moved),
        Err(e) => Err(classify(&e)),
    }
}

/// No NUL (git refuses it), at most [`MAX_MESSAGE`] bytes, never empty.
fn clean_message(message: &str, name: &str) -> String {
    let mut m: String = message.trim().chars().filter(|c| *c != '\0').collect();
    if m.len() > MAX_MESSAGE {
        let mut cut = MAX_MESSAGE;
        while !m.is_char_boundary(cut) {
            cut -= 1;
        }
        m.truncate(cut);
    }
    if m.is_empty() { format!("Version of {name}") } else { m }
}

pub(crate) fn save(h: &History, name: &str, message: &str, hook: Hook) -> Result<Saved> {
    let stage = h.stage_dir(name)?;
    if !stage.join("manifest.json").is_file() {
        return Err(Error::NothingStaged(name.to_owned()));
    }
    let repo = open(h, true)?.ok_or_else(|| Error::Damaged("the version history could not be created".into()))?;
    let _repo_lock = lock(&repo.git_dir().join(REPO_LOCK), "the version history")?;
    let branch = branch(&repo)?;
    let parent = head(&repo, &branch)?;
    let parent_tree = parent.map(|p| commit_tree(&repo, p)).transpose()?;
    let mut editor = repo.edit_tree(parent_tree.unwrap_or(repo.empty_tree().id)).git()?;
    editor.remove(name).git()?;
    {
        let _save_lock = lock(&stage.join(SAVE_LOCK), "a save of the document")?;
        let mut files = BTreeMap::new();
        walk(&stage, "", 0, &mut files)?;
        if !files.contains_key("manifest.json") {
            return Err(Error::NothingStaged(name.to_owned()));
        }
        for (rel, file) in files {
            let blob = repo.write_blob(std::fs::read(&file)?).git()?;
            editor.upsert(format!("{name}/{rel}").as_str(), EntryKind::Blob, blob).git()?;
        }
    }
    hook(Step::Blobs)?;
    let tree = editor.write().git()?.detach();
    hook(Step::Tree)?;
    if let (Some(p), Some(pt)) = (parent, parent_tree)
        && pt == tree
    {
        return Ok(Saved { version: version(&repo, p)?, created: false });
    }
    let old = match parent_tree {
        Some(t) => tree_ids(&repo, t)?,
        None => HashSet::new(),
    };
    let new_objects: Vec<ObjectId> = tree_ids(&repo, tree)?.into_iter().filter(|id| !old.contains(id)).collect();
    let sig = signature();
    let id = repo
        .write_object(gix::objs::Commit {
            tree,
            parents: parent.into_iter().collect(),
            author: sig.clone(),
            committer: sig,
            encoding: None,
            message: clean_message(message, name).into(),
            extra_headers: Vec::new(),
        })
        .git()?
        .detach();
    hook(Step::CommitObject)?;
    sync_objects(&repo, new_objects.into_iter().chain([id]))?;
    move_branch(&repo, &branch, parent, id)?;
    sync_ref(&repo, &branch)?;
    Ok(Saved { version: version(&repo, id)?, created: true })
}

pub(crate) fn list(h: &History, name: &str, limit: usize) -> Result<Vec<Version>> {
    check_name(name)?;
    let Some(repo) = open(h, false)? else { return Ok(Vec::new()) };
    let branch = branch(&repo)?;
    let Some(tip) = head(&repo, &branch)? else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for (walked, info) in repo.rev_walk([tip]).all().git()?.enumerate() {
        if walked >= MAX_WALK || out.len() >= limit {
            break;
        }
        let info = info.git()?;
        let commit = repo.find_commit(info.id).git()?;
        let here = doc_tree(&repo, commit.tree_id().git()?.detach(), name)?;
        let before = match commit.parent_ids().next() {
            Some(p) => doc_tree(&repo, commit_tree(&repo, p.detach())?, name)?,
            None => None,
        };
        if here.is_some() && here != before {
            out.push(version(&repo, info.id)?);
        }
    }
    Ok(out)
}

fn in_history(repo: &gix::Repository, tip: ObjectId, target: ObjectId) -> Result<bool> {
    for (walked, info) in repo.rev_walk([tip]).all().git()?.enumerate() {
        if walked >= MAX_WALK {
            break;
        }
        if info.git()?.id == target {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn checkout(h: &History, name: &str, id: &str, dest: &Path) -> Result<Version> {
    check_name(name)?;
    let no_such = || Error::NoSuchVersion(id.to_owned());
    let repo = open(h, false)?.ok_or_else(no_such)?;
    let target = ObjectId::from_hex(id.as_bytes()).map_err(|_| no_such())?;
    let tip = head(&repo, &branch(&repo)?)?.ok_or_else(no_such)?;
    if !in_history(&repo, tip, target)? {
        return Err(no_such());
    }
    let subtree = doc_tree(&repo, commit_tree(&repo, target)?, name)?.ok_or_else(no_such)?;
    if dest.exists() && std::fs::read_dir(dest)?.next().is_some() {
        return Err(Error::Io(format!("{} is not empty; versions are only written into an empty folder", dest.display())));
    }
    std::fs::create_dir_all(dest)?;
    let mut index = repo.index_from_tree(&subtree).git()?;
    let mut opts = repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping).git()?;
    opts.destination_is_initially_empty = true;
    let objects = repo.objects.clone().into_arc().git()?;
    gix::worktree::state::checkout(&mut index, dest, objects, &gix::progress::Discard, &gix::progress::Discard, &AtomicBool::new(false), opts).git()?;
    version(&repo, target)
}

#[cfg(test)]
pub(crate) mod test_hooks {
    pub(crate) use super::{Step, save as save_with};

    pub(crate) fn classify_io(e: std::io::Error) -> crate::Error {
        super::classify(&e)
    }
}
