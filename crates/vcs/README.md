# photocraft-vcs

Saved versions of documents, kept as git history in a hidden folder. It is
written with [gitoxide](https://github.com/GitoxideLabs/gitoxide) (`gix`, pure
Rust, no C) and never runs the `git` program. Native only: on the web every
call returns `Error::Unsupported`.

A `History` is a folder (by default `.photocraft-versions` beside the
document) holding a bare repository and one staging folder per document. The
app writes the open document into its staging folder as a folder `.pcraft`,
then commits it. The user's own file is never read or written here, so it can
be a zip `.pcraft`, a PSD or unsaved.

```rust
use std::path::Path;
use photocraft_vcs::History;

let (history, name) = History::beside(Path::new("/art/poster.pcraft"))?;
let stage = history.stage_dir(&name)?;   // the app saves the document here (folder bundle)
let saved = history.save(&name, "sky reworked")?;
let versions = history.list(&name, 50)?; // newest first
history.checkout(&name, &versions[1].id, Path::new("/tmp/v.pcraft"))?; // then load it
```

| Call | Does |
|---|---|
| `History::beside(path)` | The history for a document (its folder's link, else `.photocraft-versions`) and the name it is stored under |
| `stage_dir(name)` | Where the app writes the document before `save` |
| `save(name, message)` | Commits the staging folder. Returns `created: false` when nothing changed |
| `checkpoint(name, message)` | `save` for a version the app made on its own; listed with `checkpoint: true` |
| `list(name, limit)` | Versions that changed this document, newest first |
| `checkout(name, id, dest)` | Writes a version into an empty folder for the app to load |
| `relocate(folder, dest, progress)` | Moves a folder's history into an empty folder elsewhere, or back |
| `relink(folder, dir)` | Points a folder at an existing history after either one moved |

## Keeping the history elsewhere

A `.photocraft-versions.link` file beside the documents holds one line: the
path of the history folder. It wins over `.photocraft-versions`. `relocate`
copies the repository into `dest`, compares every byte, renames it into place,
then switches the link (a temp file renamed over it; removed when `dest` is the
default folder), and only then deletes the old copy. A crash before the switch
leaves the old history in use (a retry clears the unused copy); after it, the
new one, with the old copy left on disk. The staging folders are not copied:
the app writes them again on the next save.

A linked history that can't be reached is `Error::Offline`, and a linked folder
without a repository is `Error::Damaged`: a fresh, empty history is never
started in its place. Each moved or linked repository records the folder it
serves (`photocraft-project`); `relink` refuses a history another folder still
uses (`Error::Shared`).

## What it guarantees

- **No lost versions.** History is only added to; there is no reset or delete.
  Restoring is the app's job: load a checked-out version, then save that as a
  new version on top.
- **No torn versions.** The staging folder's `save.lock` (the lock
  `photocraft-format` holds while saving) is held while files are read.
- **Survives crashes and power loss.** gix does not fsync, so this crate
  flushes the new objects, then moves the branch (compare-and-swap), then
  flushes the branch. A crash at any step leaves the previous version as the
  newest one.
- **Leaves other repositories alone.** A repository without PhotoCraft's
  marker file is refused, and the repository is opened isolated from the
  user's git config. `info/attributes` holds `* -text -diff`, so no line-ending
  conversion touches tile bytes.

`composite/` (the redrawn preview) and `save.lock` are left out of versions.
Several documents share one history; each version replaces only its own
document's subtree, and `list` shows only the versions that changed it.

Known gaps: the staging folder doubles the disk use of each document; moving
back to the default folder is refused while an old copy that could not be
deleted is still there; macOS
flushes with `fsync`, not `F_FULLFSYNC`; Windows can't flush folders.
