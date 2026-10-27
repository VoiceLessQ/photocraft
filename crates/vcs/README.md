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
| `History::beside(path)` | The default history for a document and the name it is stored under |
| `stage_dir(name)` | Where the app writes the document before `save` |
| `save(name, message)` | Commits the staging folder. Returns `created: false` when nothing changed |
| `list(name, limit)` | Versions that changed this document, newest first |
| `checkout(name, id, dest)` | Writes a version into an empty folder for the app to load |

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

Known gaps: the staging folder doubles the disk use of each document; macOS
flushes with `fsync`, not `F_FULLFSYNC`; Windows can't flush folders.
