//! Versions (a PhotoCraft extra; Photoshop keeps version history only for cloud documents): save
//! the open document as a version in git history beside its file, list them, and restore one.
//!
//! Storage is `photocraft-vcs`, in `.photocraft-versions` next to the document. Saving writes the
//! document as it is in memory (unsaved edits too) into the history's staging folder as a folder
//! `.pcraft`, then commits it, so the person's own file is never touched and any format works.
//! Restoring loads a version into the open document as one undoable step, like File › Revert; the
//! file on disk changes only when the person saves. The current state is saved as a version first
//! and the restore lands as a new version on top, so nothing leaves the history.
//!
//! Checkpoints are versions the app saves on its own ([`start_checkpoint`]: the shell's timer and
//! File › Save, Preferences › File Handling). They only add to a history the person started, so a
//! document gets no hidden folder until someone saves a version of it.

use std::path::Path;
use std::sync::Arc;

use photocraft_doc::{DocId, Document};
use photocraft_vcs::History;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::jobs::Started;
use crate::presets::{bad, req_str};
use crate::{EngineError, Result, Session};

/// The id a checkpoint job reports under ([`crate::jobs::JobEvent::command`]).
pub const SAVE: &str = "file.versions.save";

/// Versions `file.versions.list` returns when no `limit` is given, and at most.
const DEFAULT_LIMIT: u64 = 50;
const MAX_LIMIT: u64 = 10_000;

fn vcs_err(e: photocraft_vcs::Error) -> EngineError {
    EngineError::Other(e.to_string())
}

fn need_path() -> EngineError {
    EngineError::Other("save the document once first: its versions are kept in a folder beside the file".into())
}

/// The active document's history and its name there; it needs a path to sit beside.
fn history(s: &Session) -> Result<(History, String)> {
    crate::file_cmds::native(s).map_err(EngineError::Other)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    History::beside(Path::new(d.path.as_deref().ok_or_else(need_path)?)).map_err(vcs_err)
}

fn can_version(s: &Session) -> std::result::Result<(), String> {
    crate::file_cmds::native_doc(s)?;
    let d = s.active().ok_or("no document open")?;
    d.path.as_ref().map(|_| ()).ok_or_else(|| "save the document once first".into())
}

/// Write the active document into the history's staging folder, as a folder `.pcraft`.
fn stage(s: &Session, h: &History, name: &str) -> Result<()> {
    stage_doc(&s.active().ok_or(EngineError::NoDocument)?.doc, h, name)
}

fn stage_doc(doc: &Document, h: &History, name: &str) -> Result<()> {
    let dir = h.stage_dir(name).map_err(vcs_err)?;
    std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("could not create {}: {e}", dir.display())))?;
    photocraft_format::PcraftWriter::new()
        .save_dir(doc, &dir, &photocraft_format::SaveOptions::default())
        .map_err(|e| EngineError::Other(format!("could not stage the document for a version: {e}")))?;
    Ok(())
}

/// Stage `doc` and commit it: the work of a save, on whichever thread runs it. A checkpoint of a
/// document with no versions yet is skipped (`{"created": false, "skipped": true}`).
fn commit(doc: &Document, h: &History, name: &str, message: &str, checkpoint: bool) -> Result<Value> {
    if checkpoint && h.list(name, 1).map_err(vcs_err)?.is_empty() {
        return Ok(json!({"created": false, "skipped": true}));
    }
    stage_doc(doc, h, name)?;
    let saved = if checkpoint { h.checkpoint(name, message) } else { h.save(name, message) }.map_err(vcs_err)?;
    let mut out = version_json(&saved.version);
    out["created"] = json!(saved.created);
    Ok(out)
}

/// Save a checkpoint of document `doc` in the background, for the shell: not journaled, so it
/// never lands in a recorded action or the History Log. The job reports as [`SAVE`] with
/// `{"checkpoint": true}`; its result is [`file.versions.save`](SAVE)'s.
pub fn start_checkpoint(s: &mut Session, doc: DocId, message: &str) -> Result<Started> {
    crate::file_cmds::native(s).map_err(EngineError::Other)?;
    let d = s.documents().iter().find(|d| d.doc.id == doc).ok_or(EngineError::NoDocument)?;
    let (h, name) = History::beside(Path::new(d.path.as_deref().ok_or_else(need_path)?)).map_err(vcs_err)?;
    let snapshot: Arc<Document> = d.doc.clone();
    let message = message.to_owned();
    s.start_job(
        SAVE,
        json!({"checkpoint": true, "docId": doc.0}),
        "Saving a checkpoint",
        false,
        move |ctx| {
            ctx.progress(0.0, "Saving a checkpoint");
            commit(&snapshot, &h, &name, &message, true)
        },
        |_, out| Ok(out),
    )
}

fn version_json(v: &photocraft_vcs::Version) -> Value {
    // UTC: the engine has no time-zone database; the panel shows recent versions as "5 minutes ago".
    // The web build has no chrono, and no versions either.
    #[cfg(not(target_arch = "wasm32"))]
    let date = chrono::DateTime::from_timestamp(v.time, 0).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string());
    #[cfg(target_arch = "wasm32")]
    let date: Option<String> = None;
    json!({"id": v.id, "message": v.message, "time": v.time, "date": date, "checkpoint": v.checkpoint})
}

fn save(s: &mut Session, p: &Value) -> Result<Value> {
    let message = match p.get("message") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(m)) => m.clone(),
        Some(_) => return Err(bad(SAVE, "`message` must be a string")),
    };
    let checkpoint = match p.get("checkpoint") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad(SAVE, "`checkpoint` must be true or false")),
    };
    let (h, name) = history(s)?;
    let doc = s.active().ok_or(EngineError::NoDocument)?.doc.clone();
    // A background job when started with `Session::start`: staging a large document takes a
    // second or more, and the snapshot leaves the document free to edit meanwhile.
    crate::jobs::run(
        s,
        "Save Version",
        false,
        move |ctx| {
            ctx.progress(0.0, "Saving a version");
            commit(&doc, &h, &name, &message, checkpoint)
        },
        |_, out| Ok(out),
    )
}

fn list(s: &mut Session, p: &Value) -> Result<Value> {
    let limit = match p.get("limit") {
        None | Some(Value::Null) => DEFAULT_LIMIT,
        Some(v) => {
            v.as_u64().filter(|n| *n <= MAX_LIMIT).ok_or_else(|| bad("file.versions.list", format!("`limit` must be a whole number from 0 to {MAX_LIMIT}")))?
        }
    };
    let (h, name) = history(s)?;
    let versions = h.list(&name, usize::try_from(limit).unwrap_or(usize::MAX)).map_err(vcs_err)?;
    Ok(json!({"versions": versions.iter().map(version_json).collect::<Vec<_>>(), "history": h.dir().display().to_string()}))
}

fn short(id: &str) -> &str {
    id.get(..10).unwrap_or(id)
}

fn restore(s: &mut Session, p: &Value) -> Result<Value> {
    let id = req_str(p, "id", "file.versions.restore")?;
    if id.len() != 40 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad("file.versions.restore", "`id` must be a 40-digit version id from file.versions.list"));
    }
    let (h, name) = history(s)?;
    // Check the id before anything is saved, so a typo leaves no "Before restoring" version.
    if !h.list(&name, usize::MAX).map_err(vcs_err)?.iter().any(|v| v.id.eq_ignore_ascii_case(id)) {
        return Err(vcs_err(photocraft_vcs::Error::NoSuchVersion(id.to_owned())));
    }
    stage(s, &h, &name)?;
    h.save(&name, &format!("Before restoring {}", short(id))).map_err(vcs_err)?;
    let dest = h.dir().join(format!("restore-{}", std::process::id()));
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| EngineError::Other(format!("could not clear {}: {e}", dest.display())))?;
    }
    let loaded = h.checkout(&name, id, &dest).map_err(vcs_err).and_then(|v| {
        crate::allocation::checkpoint("restoring a version")?;
        let doc = photocraft_format::load_path(&dest).map_err(|e| EngineError::Other(format!("version {}: {e}", short(id))))?;
        Ok((v, doc))
    });
    // The checkout is a scratch copy; a leftover is harmless, so a failed removal is not an error.
    let _ = std::fs::remove_dir_all(&dest);
    let (version, fresh) = loaded?;
    s.edit("Restore Version", |doc, active| {
        let (doc_id, doc_name) = (doc.id, doc.name.clone());
        *doc = fresh;
        // Keep the identity (colour state, views and caches are keyed by it) and the tab name.
        doc.id = doc_id;
        doc.name = doc_name;
        *active = doc.top_layer();
        Ok(())
    })?;
    stage(s, &h, &name)?;
    let saved = h.save(&name, &format!("Restored {}", short(id))).map_err(vcs_err)?;
    let layers = s.active().map_or(0, |d| d.doc.layers.len());
    Ok(json!({"restored": version_json(&version), "version": version_json(&saved.version), "layers": layers}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "file.versions.save",
            label: "Save Version",
            menu: &[],
            shortcut: None,
            params: r#"{"message":str="", "checkpoint":bool=false} → {id, message, time, checkpoint, created}: save the open document (unsaved edits too) as a version in .photocraft-versions beside its file; created=false when nothing changed. checkpoint=true marks it as saved by the app and does nothing ({created:false, skipped:true}) until the document has a version"#,
            enabled: can_version,
            journal: true,
            run: save,
        },
        CommandSpec {
            id: "file.versions.list",
            label: "Versions",
            menu: &[],
            shortcut: None,
            params: r#"{"limit":int=50} → {versions:[{id, message, time, date, checkpoint}], history}: the open document's versions, newest first (time in Unix seconds, date in UTC; checkpoint: saved by the app, not named)"#,
            enabled: can_version,
            journal: false,
            run: list,
        },
        CommandSpec {
            id: "file.versions.restore",
            label: "Restore Version",
            menu: &[],
            shortcut: None,
            params: r#"{"id":str} → {restored, version, layers}: load version `id` into the open document as one undoable step; the current state is saved as a version first and the restore as a new one on top. The file on disk changes only when you save"#,
            enabled: can_version,
            journal: true,
            run: restore,
        },
    ]
}

#[cfg(test)]
mod tests;
