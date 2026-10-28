use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Session;

/// A session with a small document whose path is `<dir>/art.pcraft` (the file need not exist).
fn session(dir: &Path) -> (Session, PathBuf) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "background": "#336699"})).unwrap();
    let path = dir.join("art.pcraft");
    s.active_mut().unwrap().path = Some(path.display().to_string());
    (s, path)
}

/// The document's content as deterministic bundle bytes, to compare states.
fn snapshot(s: &Session) -> Vec<u8> {
    photocraft_format::save_to_bytes(&s.active().unwrap().doc, &photocraft_format::SaveOptions::default()).unwrap()
}

fn versions(s: &mut Session) -> Vec<Value> {
    s.execute("file.versions.list", json!({})).unwrap()["versions"].as_array().unwrap().clone()
}

fn messages(s: &mut Session) -> Vec<String> {
    versions(s).iter().map(|v| v["message"].as_str().unwrap().to_owned()).collect()
}

#[test]
fn needs_a_saved_document() {
    let mut s = Session::new();
    assert!(s.execute("file.versions.save", json!({})).is_err(), "no document");
    s.execute("file.new", json!({})).unwrap();
    let e = s.execute("file.versions.save", json!({})).unwrap_err().to_string();
    assert!(e.contains("save the document once first"), "{e}");
    assert!(s.execute("file.versions.list", json!({})).is_err());
}

#[test]
fn save_and_list_versions() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path());
    let v1 = s.execute("file.versions.save", json!({"message": "blue"})).unwrap();
    assert_eq!(v1["created"], true);
    assert_eq!(v1["id"].as_str().unwrap().len(), 40);
    let again = s.execute("file.versions.save", json!({"message": "no change"})).unwrap();
    assert_eq!(again["created"], false);
    assert_eq!(again["id"], v1["id"]);
    s.execute("image.adjustments.invert", json!({})).unwrap();
    let v2 = s.execute("file.versions.save", json!({})).unwrap();
    assert_eq!(v2["created"], true);
    assert_eq!(messages(&mut s), vec!["Version of art.pcraft", "blue"]);
    let listed = s.execute("file.versions.list", json!({"limit": 1})).unwrap();
    assert_eq!(listed["versions"].as_array().unwrap().len(), 1);
    assert!(listed["history"].as_str().unwrap().ends_with(".photocraft-versions"));
    assert!(listed["versions"][0]["date"].as_str().unwrap().ends_with(" UTC"));
    assert!(dir.path().join(".photocraft-versions").join("history.git").is_dir());
}

#[test]
fn restore_is_one_undoable_step_and_a_new_version() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path());
    let blue = snapshot(&s);
    let v1 = s.execute("file.versions.save", json!({"message": "blue"})).unwrap()["id"].as_str().unwrap().to_owned();
    s.execute("image.adjustments.invert", json!({})).unwrap();
    let inverted = snapshot(&s);
    s.execute("file.versions.save", json!({"message": "inverted"})).unwrap();
    let r = s.execute("file.versions.restore", json!({"id": v1})).unwrap();
    assert_eq!(r["restored"]["id"], v1.as_str());
    assert_eq!(snapshot(&s), blue);
    // "Before restoring" matched the newest version, so it made none.
    assert_eq!(messages(&mut s), vec![format!("Restored {}", &v1[..10]), "inverted".into(), "blue".into()]);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(snapshot(&s), inverted, "undo brings back the state before the restore");
}

#[test]
fn restore_saves_unsaved_edits_first() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path());
    let v1 = s.execute("file.versions.save", json!({})).unwrap()["id"].as_str().unwrap().to_owned();
    s.execute("image.adjustments.invert", json!({})).unwrap();
    let unsaved = snapshot(&s);
    s.execute("file.versions.restore", json!({"id": v1})).unwrap();
    let list = versions(&mut s);
    let before = list.iter().find(|v| v["message"].as_str().unwrap().starts_with("Before restoring")).expect("safety version");
    s.execute("file.versions.restore", json!({"id": before["id"]})).unwrap();
    assert_eq!(snapshot(&s), unsaved);
}

#[test]
fn the_file_on_disk_is_never_touched() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, path) = session(dir.path());
    std::fs::write(&path, b"the person's own file").unwrap();
    let v1 = s.execute("file.versions.save", json!({})).unwrap()["id"].as_str().unwrap().to_owned();
    s.execute("image.adjustments.invert", json!({})).unwrap();
    s.execute("file.versions.save", json!({})).unwrap();
    s.execute("file.versions.restore", json!({"id": v1})).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"the person's own file");
    assert!(s.active().unwrap().is_dirty(), "a restore is an unsaved change");
}

#[test]
fn bad_params_are_errors_and_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path());
    for p in [json!({"message": 5}), json!({"message": ["a"]}), json!({"message": {}}), json!({"checkpoint": "yes"}), json!({"checkpoint": 1})] {
        assert!(s.execute("file.versions.save", p.clone()).is_err(), "{p}");
    }
    for p in [json!({"limit": -1}), json!({"limit": 1.5}), json!({"limit": "9"}), json!({"limit": 10_001}), json!({"limit": true})] {
        assert!(s.execute("file.versions.list", p.clone()).is_err(), "{p}");
    }
    assert!(!dir.path().join(".photocraft-versions").exists(), "refused calls create nothing");
    s.execute("file.versions.save", json!({})).unwrap();
    let before = versions(&mut s).len();
    let long = "a".repeat(41);
    let zeros = "0".repeat(40);
    for p in [json!({}), json!({"id": 7}), json!({"id": ""}), json!({"id": "zz"}), json!({"id": long}), json!({"id": zeros}), json!({"id": "é".repeat(20)})] {
        assert!(s.execute("file.versions.restore", p.clone()).is_err(), "{p}");
    }
    s.execute("image.adjustments.invert", json!({})).unwrap();
    assert!(s.execute("file.versions.restore", json!({"id": "0".repeat(40)})).is_err());
    assert_eq!(versions(&mut s).len(), before, "an unknown id saves no 'Before restoring' version");
}

#[test]
fn a_checkpoint_waits_for_the_first_version() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path());
    let skipped = s.execute("file.versions.save", json!({"checkpoint": true})).unwrap();
    assert_eq!((skipped["created"].clone(), skipped["skipped"].clone()), (json!(false), json!(true)));
    assert!(!dir.path().join(".photocraft-versions").join("history.git").exists(), "no history until a person saves a version");
    s.execute("file.versions.save", json!({"message": "named"})).unwrap();
    s.execute("image.adjustments.invert", json!({})).unwrap();
    let cp = s.execute("file.versions.save", json!({"checkpoint": true, "message": "Auto-saved"})).unwrap();
    assert_eq!((cp["created"].clone(), cp["checkpoint"].clone()), (json!(true), json!(true)));
    let flags: Vec<Value> = versions(&mut s).iter().map(|v| v["checkpoint"].clone()).collect();
    assert_eq!(flags, vec![json!(true), json!(false)]);
    assert_eq!(messages(&mut s), vec!["Auto-saved", "named"]);
}

#[test]
fn a_shell_checkpoint_runs_in_the_background_and_is_not_journaled() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path());
    s.execute("file.versions.save", json!({"message": "named"})).unwrap();
    s.execute("image.adjustments.invert", json!({})).unwrap();
    let doc = s.active().unwrap().doc.id;
    // Another document is active when the checkpoint starts.
    s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
    let journal = s.journal.len();
    let crate::jobs::Started::Job(job) = super::start_checkpoint(&mut s, doc, "Auto-saved").unwrap() else { panic!("a background job") };
    let done = s.wait_job(job).unwrap();
    assert_eq!((done["created"].clone(), done["checkpoint"].clone()), (json!(true), json!(true)));
    assert_eq!(s.journal.len(), journal, "not journaled");
    assert!(s.set_active(0));
    assert_eq!(messages(&mut s), vec!["Auto-saved", "named"]);
    // A document with no path, or one that is gone, is an error, not a panic.
    assert!(s.set_active(1));
    let unsaved = s.active().unwrap().doc.id;
    assert!(super::start_checkpoint(&mut s, unsaved, "x").is_err());
    s.execute("file.close", json!({})).unwrap();
    assert!(super::start_checkpoint(&mut s, unsaved, "x").is_err());
}

#[test]
fn a_started_save_runs_as_a_job() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path());
    let crate::jobs::Started::Job(job) = s.start("file.versions.save", json!({"message": "bg"})).unwrap() else { panic!("a background job") };
    assert_eq!(s.wait_job(job).unwrap()["created"], true);
    assert_eq!(messages(&mut s), vec!["bg"]);
}

#[test]
fn every_version_command_is_registered_with_docs() {
    for id in ["file.versions.save", "file.versions.list", "file.versions.restore"] {
        let spec = crate::commands::command_specs().iter().find(|c| c.id == id).unwrap_or_else(|| panic!("{id} registered"));
        assert!(spec.menu.is_empty(), "{id}: no Photoshop menu entry");
        assert!(spec.params.contains('→'), "{id}: params say what comes back");
    }
}
