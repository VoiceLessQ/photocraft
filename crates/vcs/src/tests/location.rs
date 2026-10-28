use std::path::{Path, PathBuf};

use super::{NAME, write_doc};
use crate::location::test_hooks::{Step, relocate_with};
use crate::{Error, FOLDER, History, LINK, relink, relocate};

/// A project folder with a history beside it holding two versions of `art.pcraft`.
fn project(root: &Path, name: &str) -> (PathBuf, Vec<String>) {
    let folder = root.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    let (h, doc) = History::beside(&folder.join(NAME)).unwrap();
    let stage = h.stage_dir(&doc).unwrap();
    write_doc(&stage, r#"{"v":1}"#, &[("a.zst", b"tile-a")]);
    h.save(&doc, "first").unwrap();
    write_doc(&stage, r#"{"v":2}"#, &[("a.zst", b"tile-b")]);
    h.save(&doc, "second").unwrap();
    let ids = ids(&folder);
    (folder, ids)
}

fn ids(folder: &Path) -> Vec<String> {
    History::beside(&folder.join(NAME)).unwrap().0.list(NAME, 100).unwrap().into_iter().map(|v| v.id).collect()
}

fn go(_: u64, _: u64) -> bool {
    true
}

#[test]
fn relocate_moves_every_version_and_back_home() {
    let root = tempfile::tempdir().unwrap();
    let (folder, before) = project(root.path(), "p");
    let away = root.path().join("nas").join("hist");
    let moved = relocate(&folder, &away, &mut go).unwrap();
    assert!(moved.history.linked() && moved.leftover.is_none());
    assert!(!folder.join(FOLDER).exists(), "the old copy is deleted");
    let link = std::fs::read_to_string(folder.join(LINK)).unwrap();
    assert_eq!(Path::new(link.trim()), away);
    assert_eq!(ids(&folder), before);
    // New versions go to the new place.
    let (h, doc) = History::beside(&folder.join(NAME)).unwrap();
    assert!(h.stage_dir(&doc).unwrap().starts_with(&away));
    write_doc(&h.stage_dir(&doc).unwrap(), r#"{"v":3}"#, &[("a.zst", b"tile-c")]);
    assert!(h.save(&doc, "third").unwrap().created);
    let after = ids(&folder);
    assert_eq!(after.len(), 3);
    // Back to the default folder: the link goes, the versions stay.
    let home = relocate(&folder, &folder.join(FOLDER), &mut go).unwrap();
    assert!(!home.history.linked());
    assert!(!folder.join(LINK).exists());
    assert!(!away.exists(), "the copy on the NAS is deleted");
    assert_eq!(ids(&folder), after);
}

#[test]
fn an_offline_history_is_never_recreated() {
    let root = tempfile::tempdir().unwrap();
    let (folder, before) = project(root.path(), "p");
    let away = root.path().join("usb");
    relocate(&folder, &away, &mut go).unwrap();
    let unplugged = root.path().join("usb-unplugged");
    std::fs::rename(&away, &unplugged).unwrap();
    let (h, doc) = History::beside(&folder.join(NAME)).unwrap();
    assert!(h.offline());
    assert!(matches!(h.stage_dir(&doc), Err(Error::Offline(_))));
    assert!(matches!(h.save(&doc, "x"), Err(Error::Offline(_))));
    assert!(matches!(h.checkpoint(&doc, "x"), Err(Error::Offline(_))));
    assert!(matches!(h.list(&doc, 10), Err(Error::Offline(_))));
    assert!(matches!(relocate(&folder, &root.path().join("else"), &mut go), Err(Error::Offline(_))));
    assert!(!away.exists(), "nothing is created in its place");
    std::fs::rename(&unplugged, &away).unwrap();
    assert_eq!(ids(&folder), before);
    // A linked folder that was emptied is damaged, not a place to start over.
    std::fs::remove_dir_all(away.join("history.git")).unwrap();
    let (h, doc) = History::beside(&folder.join(NAME)).unwrap();
    assert!(matches!(h.stage_dir(&doc), Err(Error::Damaged(_))), "nothing is staged there either");
    assert!(matches!(h.save(&doc, "x"), Err(Error::Damaged(_))));
    assert!(matches!(h.list(&doc, 10), Err(Error::Damaged(_))));
    assert_eq!(std::fs::read_dir(&away).unwrap().count(), 0);
}

#[test]
fn a_crash_before_the_switch_keeps_the_old_history_and_a_retry_works() {
    let root = tempfile::tempdir().unwrap();
    let (folder, before) = project(root.path(), "p");
    let away = root.path().join("away");
    let crash = |at| move |s| if s == at { Err(Error::Io("simulated crash".into())) } else { Ok(()) };
    assert!(relocate_with(&folder, &away, &mut go, &mut crash(Step::Copied)).is_err());
    assert!(!folder.join(LINK).exists());
    assert!(!History::beside(&folder.join(NAME)).unwrap().0.linked());
    assert_eq!(ids(&folder), before);
    let moved = relocate(&folder, &away, &mut go).unwrap();
    assert!(moved.history.linked());
    assert_eq!(ids(&folder), before);
}

#[test]
fn a_crash_after_the_switch_uses_the_new_history() {
    let root = tempfile::tempdir().unwrap();
    let (folder, before) = project(root.path(), "p");
    let away = root.path().join("away");
    let mut crash = |s| if s == Step::Switched { Err(Error::Io("simulated crash".into())) } else { Ok(()) };
    assert!(relocate_with(&folder, &away, &mut go, &mut crash).is_err());
    assert!(History::beside(&folder.join(NAME)).unwrap().0.linked());
    assert_eq!(ids(&folder), before);
    assert!(folder.join(FOLDER).join("history.git").exists(), "the old copy is left, not lost");
}

#[test]
fn a_cancelled_move_changes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let (folder, before) = project(root.path(), "p");
    let away = root.path().join("away");
    assert!(matches!(relocate(&folder, &away, &mut |_, _| false), Err(Error::Cancelled)));
    assert!(!folder.join(LINK).exists());
    assert_eq!(std::fs::read_dir(&away).unwrap().count(), 0, "the partial copy is removed");
    assert_eq!(ids(&folder), before);
}

#[test]
fn bad_destinations_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let (folder, before) = project(root.path(), "p");
    let busy = root.path().join("busy");
    std::fs::create_dir_all(&busy).unwrap();
    std::fs::write(busy.join("photo.jpg"), b"x").unwrap();
    let file = root.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    let current = folder.join(FOLDER);
    for dest in [busy.clone(), file, current.clone(), current.join("inner"), folder.clone(), root.path().to_path_buf()] {
        assert!(relocate(&folder, &dest, &mut go).is_err(), "{}", dest.display());
    }
    assert!(!folder.join(LINK).exists());
    assert_eq!(ids(&folder), before);
    assert!(busy.join("photo.jpg").exists());
}

#[test]
fn choosing_a_place_before_the_first_version_starts_the_history_there() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("p");
    std::fs::create_dir_all(&folder).unwrap();
    let away = root.path().join("away");
    relocate(&folder, &away, &mut go).unwrap();
    let (h, doc) = History::beside(&folder.join(NAME)).unwrap();
    assert!(h.linked() && h.list(&doc, 10).unwrap().is_empty());
    write_doc(&h.stage_dir(&doc).unwrap(), "{}", &[("a.zst", b"1")]);
    assert!(h.save(&doc, "first").unwrap().created);
    assert!(away.join("history.git").is_dir());
    assert!(!folder.join(FOLDER).exists());
}

#[test]
fn relink_finds_a_history_and_refuses_one_in_use() {
    let root = tempfile::tempdir().unwrap();
    let (a, before) = project(root.path(), "a");
    let hist = root.path().join("hist");
    relocate(&a, &hist, &mut go).unwrap();
    let b = root.path().join("b");
    std::fs::create_dir_all(&b).unwrap();
    assert!(matches!(relink(&b, &hist), Err(Error::Shared(owner)) if owner == a.canonicalize().unwrap()));
    assert!(!b.join(LINK).exists());
    // The project moved: its old folder is gone, so the new one may take the history over.
    std::fs::remove_dir_all(&a).unwrap();
    assert!(relink(&b, &hist).unwrap().linked());
    assert_eq!(ids(&b), before);
    // A default history beside another folder's documents is in use by that folder.
    let (c, _) = project(root.path(), "c");
    assert!(matches!(relink(&b, &c.join(FOLDER)), Err(Error::Shared(_))));
    // Not a history at all.
    let empty = root.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(relink(&b, &empty).is_err());
    assert_eq!(ids(&b), before);
}

#[test]
fn hostile_link_files_are_errors_not_panics() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("p");
    std::fs::create_dir_all(&folder).unwrap();
    let doc = folder.join(NAME);
    for bytes in [b"".to_vec(), b"   \n".to_vec(), vec![0xff, 0xfe, 0x00], vec![b'a'; 5000]] {
        std::fs::write(folder.join(LINK), &bytes).unwrap();
        assert!(matches!(History::beside(&doc), Err(Error::Damaged(_))), "{:?}", bytes.get(..4));
    }
    // A relative link is read from the folder; the history it names is offline until it exists.
    std::fs::write(folder.join(LINK), "../elsewhere\n").unwrap();
    let (h, _) = History::beside(&doc).unwrap();
    assert_eq!(h.dir(), folder.join("../elsewhere"));
    assert!(h.offline());
}

#[test]
fn a_link_to_a_folder_that_is_no_history_writes_nothing_there() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("p");
    let target = root.path().join("my-work");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("notes.txt"), b"keep").unwrap();
    std::fs::write(folder.join(LINK), format!("{}\n", target.display())).unwrap();
    let (h, doc) = History::beside(&folder.join(NAME)).unwrap();
    assert!(matches!(h.stage_dir(&doc), Err(Error::Damaged(_))));
    assert!(matches!(h.save(&doc, "x"), Err(Error::Damaged(_))));
    assert!(matches!(relocate(&folder, &root.path().join("dest"), &mut go), Err(Error::Damaged(_))));
    let names: Vec<_> = std::fs::read_dir(&target).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("notes.txt")], "nothing created or deleted there");
}

#[test]
fn a_hand_written_link_to_another_folders_history_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let (a, before) = project(root.path(), "a");
    let b = root.path().join("b");
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(b.join(LINK), format!("{}\n", a.join(FOLDER).display())).unwrap();
    let (h, doc) = History::beside(&b.join(NAME)).unwrap();
    assert!(matches!(h.stage_dir(&doc), Err(Error::Shared(_))));
    assert!(matches!(h.list(&doc, 10), Err(Error::Shared(_))));
    assert!(matches!(relocate(&b, &root.path().join("dest"), &mut go), Err(Error::Shared(_))));
    assert!(!root.path().join("dest").exists());
    assert_eq!(ids(&a), before, "a's history is untouched");
}

#[test]
fn network_paths_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let (folder, before) = project(root.path(), "p");
    for line in ["\\\\server\\share\\versions", "//server/share/versions"] {
        std::fs::write(folder.join(LINK), format!("{line}\n")).unwrap();
        assert!(History::beside(&folder.join(NAME)).is_err(), "{line}");
    }
    std::fs::remove_file(folder.join(LINK)).unwrap();
    assert_eq!(ids(&folder), before);
}

#[cfg(unix)]
#[test]
fn a_fifo_as_the_link_file_does_not_block() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("p");
    std::fs::create_dir_all(&folder).unwrap();
    let made = std::process::Command::new("mkfifo").arg(folder.join(LINK)).status();
    if !made.is_ok_and(|s| s.success()) {
        return; // no mkfifo here
    }
    assert!(matches!(History::beside(&folder.join(NAME)), Err(Error::Damaged(_))));
}
