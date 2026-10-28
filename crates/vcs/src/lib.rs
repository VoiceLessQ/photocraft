//! `photocraft-vcs`: saved versions of documents, kept as git history in a hidden folder.
//!
//! A [`History`] is a folder (by default `.photocraft-versions` beside the document) holding a
//! bare git repository and one staging folder per document. The app writes the open document
//! into its staging folder as a folder `.pcraft`, then [`History::save`] commits that folder as
//! the document's subtree. The user's own file is never read or written here, so a version can
//! be saved before the document is, and restoring one ([`History::checkout`] into a temp folder,
//! then loading it) changes nothing on disk until the person saves.
//!
//! Written with gitoxide (pure Rust, no C), never with the `git` program, opened isolated from
//! the user's git config. The rules: history is only added to; the staging folder's `save.lock`
//! is held while it is read, so a version never mixes two saves; and the branch moves only after
//! the objects it names are flushed to disk, then the branch is flushed. A repository PhotoCraft
//! did not create is never written to.
//!
//! The history can live elsewhere (another drive, a NAS): a one-line [`LINK`] file beside the
//! documents names it, and [`relocate`] moves it there (copy, verify, switch the link, then delete
//! the old copy). A linked history that can't be reached is [`Error::Offline`]; a fresh one is
//! never started in its place.
//!
//! Native only: on the web every call returns [`Error::Unsupported`].

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::{Path, PathBuf};

#[cfg(not(target_arch = "wasm32"))]
mod imp;
#[cfg(not(target_arch = "wasm32"))]
mod location;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

/// Name of the history folder [`History::beside`] puts next to a document.
pub const FOLDER: &str = ".photocraft-versions";

/// One-line file beside the documents naming a history kept elsewhere; it wins over [`FOLDER`].
pub const LINK: &str = ".photocraft-versions.link";

/// Why a version operation failed. Every message says what to do next.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// This build has no versions (the web build).
    #[error("versions are not available in this build")]
    Unsupported,
    /// The document name can't be stored (empty, a path, too long).
    #[error("{0:?} can't be used as a document name for versions")]
    BadName(String),
    /// Nothing has been staged for this document (no `manifest.json` in its staging folder).
    #[error("nothing staged to save for {0}")]
    NothingStaged(String),
    /// The history folder holds a repository PhotoCraft did not create.
    #[error("{} holds a git repository that PhotoCraft did not create; versions stay off there", .0.display())]
    ForeignRepository(PathBuf),
    /// A save or another version operation kept its lock too long.
    #[error("{0} is busy; try again in a moment")]
    Busy(String),
    /// The id names no version of this document.
    #[error("no version {0} of this document")]
    NoSuchVersion(String),
    /// The history moved while this operation ran (another window saved a version).
    #[error("the version history changed while saving; try again")]
    Moved,
    /// The disk is full; the history is unchanged.
    #[error("the disk is full ({0}); free some space and try again")]
    DiskFull(String),
    /// The history the link file names can't be reached (its drive is disconnected).
    #[error("the version history at {} can't be reached (is its drive connected?); versions wait until it is back", .0.display())]
    Offline(PathBuf),
    /// The history already serves the documents in another folder.
    #[error("that version history belongs to the documents in {}; one history serves one folder", .0.display())]
    Shared(PathBuf),
    /// A move was cancelled before anything changed.
    #[error("cancelled; the version history stays where it was")]
    Cancelled,
    /// The history is in a state that needs a person to look at it.
    #[error("{0}")]
    Damaged(String),
    /// Any other file-system or git error.
    #[error("{0}")]
    Io(String),
}

/// Result of a version operation.
pub type Result<T> = std::result::Result<T, Error>;

/// One saved version of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// Full commit id (40 hex digits).
    pub id: String,
    /// What the person (or the app) wrote when saving it.
    pub message: String,
    /// When it was saved, in seconds since the Unix epoch.
    pub time: i64,
    /// Saved by the app on its own ([`History::checkpoint`]), not by the person.
    pub checkpoint: bool,
}

/// What [`History::save`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// The document's newest version after the call.
    pub version: Version,
    /// False when the staged files equal the newest version, so none was made.
    pub created: bool,
}

/// What [`relocate`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    /// The history at its new place.
    pub history: History,
    /// The old copy, when it could not be deleted (the move itself is complete).
    pub leftover: Option<PathBuf>,
}

/// A history folder: a bare repository plus a staging folder per document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    dir: PathBuf,
    /// The folder whose [`LINK`] file named this history: it is never created where it is
    /// missing, and is refused when another folder uses it.
    link_of: Option<PathBuf>,
}

impl History {
    /// The history in `dir` (created on the first save).
    pub fn at(dir: impl Into<PathBuf>) -> History {
        History { dir: dir.into(), link_of: None }
    }

    /// The history for the document at `path` (the one its folder's [`LINK`] names, else
    /// `<its folder>/.photocraft-versions`) and the name the document is stored under (its file
    /// name).
    pub fn beside(path: &Path) -> Result<(History, String)> {
        let path: PathBuf = path.components().collect();
        let bad = || Error::BadName(path.display().to_string());
        let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(bad)?.to_owned();
        check_name(&name)?;
        Ok((History::of_folder(&folder_of(&path))?, name))
    }

    /// The history of the documents in `folder`.
    pub fn of_folder(folder: &Path) -> Result<History> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(dir) = location::read_link(folder)? {
            return Ok(History { dir, link_of: Some(folder.to_path_buf()) });
        }
        Ok(History::at(folder.join(FOLDER)))
    }

    /// The history folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether a [`LINK`] file named this history (it lives away from the documents).
    pub fn linked(&self) -> bool {
        self.link_of.is_some()
    }

    /// A linked history whose folder can't be reached.
    pub fn offline(&self) -> bool {
        self.linked() && !self.dir.is_dir()
    }

    /// A linked history must be reachable, PhotoCraft's, and this folder's alone before anything
    /// is written there: a link file can name any folder.
    fn check_link(&self) -> Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(folder) = &self.link_of {
            location::check_linked(folder, &self.dir)?;
        }
        Ok(())
    }

    /// Where the app writes `name`'s folder `.pcraft` before calling [`History::save`].
    pub fn stage_dir(&self, name: &str) -> Result<PathBuf> {
        check_name(name)?;
        self.check_link()?;
        Ok(self.dir.join("stage").join(name))
    }

    /// Commit `name`'s staging folder as its newest version, creating the history on first use.
    /// Returns the existing newest version with `created: false` when nothing changed.
    pub fn save(&self, name: &str, message: &str) -> Result<Saved> {
        #[cfg(not(target_arch = "wasm32"))]
        return imp::save(self, name, message, false, &mut |_| Ok(()));
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (name, message);
            Err(Error::Unsupported)
        }
    }

    /// [`History::save`] for a version the app saves on its own (on a timer, or with the file):
    /// listed with [`Version::checkpoint`] set, so it shows apart from the ones people name.
    pub fn checkpoint(&self, name: &str, message: &str) -> Result<Saved> {
        #[cfg(not(target_arch = "wasm32"))]
        return imp::save(self, name, message, true, &mut |_| Ok(()));
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (name, message);
            Err(Error::Unsupported)
        }
    }

    /// `name`'s versions, newest first, at most `limit`. Empty when it has none yet.
    pub fn list(&self, name: &str, limit: usize) -> Result<Vec<Version>> {
        #[cfg(not(target_arch = "wasm32"))]
        return imp::list(self, name, limit);
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (name, limit);
            Err(Error::Unsupported)
        }
    }

    /// Write version `id` of `name` into `dest` (missing or empty) as a folder `.pcraft`, for the
    /// app to load. The history is unchanged.
    pub fn checkout(&self, name: &str, id: &str, dest: &Path) -> Result<Version> {
        #[cfg(not(target_arch = "wasm32"))]
        return imp::checkout(self, name, id, dest);
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (name, id, dest);
            Err(Error::Unsupported)
        }
    }
}

/// The folder a document's history belongs to: the one it sits in (`.` for a bare file name).
pub fn folder_of(doc: &Path) -> PathBuf {
    match doc.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Move the history of the documents in `folder` into `dest` (missing or empty): copy it, verify
/// every byte, point the [`LINK`] at it (or remove the link when `dest` is the default
/// [`FOLDER`]), then delete the old copy. A crash at any step leaves one complete history in
/// use. `progress(done, total)` gets bytes copied; returning false cancels before the switch.
/// With no history yet, an empty one is made at `dest`, so versions start there.
pub fn relocate(folder: &Path, dest: &Path, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<Moved> {
    #[cfg(not(target_arch = "wasm32"))]
    return location::relocate(folder, dest, progress, &mut |_| Ok(()));
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (folder, dest, progress);
        Err(Error::Unsupported)
    }
}

/// Point the documents in `folder` at the existing history in `dir` ("Find history folder…"
/// after the folder or the history moved). Refused when another folder still uses it.
pub fn relink(folder: &Path, dir: &Path) -> Result<History> {
    #[cfg(not(target_arch = "wasm32"))]
    return location::relink(folder, dir);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (folder, dir);
        Err(Error::Unsupported)
    }
}

/// A document name is one path component: not empty, no separators, not `.`/`..`, at most 255
/// bytes, no NUL.
fn check_name(name: &str) -> Result<()> {
    let bad = name.is_empty() || name == "." || name == ".." || name.len() > 255 || name.contains(['/', '\\', '\0']);
    if bad { Err(Error::BadName(name.to_owned())) } else { Ok(()) }
}
