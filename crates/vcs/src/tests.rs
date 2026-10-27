#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::imp::test_hooks::{Step, classify_io, save_with};
use crate::{Error, FOLDER, History};

/// Write a minimal folder bundle: a manifest plus the given tiles, replacing what was there.
fn write_doc(path: &Path, manifest: &str, tiles: &[(&str, &[u8])]) {
    if path.exists() {
        std::fs::remove_dir_all(path).unwrap();
    }
    std::fs::create_dir_all(path.join("tiles")).unwrap();
    std::fs::write(path.join("manifest.json"), manifest).unwrap();
    for (name, bytes) in tiles {
        std::fs::write(path.join("tiles").join(name), bytes).unwrap();
    }
}

/// Every file of a bundle (relative path to bytes), minus what versions leave out.
fn contents(path: &Path) -> BTreeMap<String, Vec<u8>> {
    fn go(dir: &Path, rel: &str, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().into_string().unwrap();
            let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            if e.file_type().unwrap().is_dir() {
                if child != "composite" {
                    go(&e.path(), &child, out);
                }
            } else if child != "save.lock" {
                out.insert(child, std::fs::read(e.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    go(path, "", &mut out);
    out
}

const NAME: &str = "art.pcraft";

struct Fixture {
    dir: tempfile::TempDir,
    h: History,
    v1: BTreeMap<String, Vec<u8>>,
    v2: BTreeMap<String, Vec<u8>>,
    id1: String,
    id2: String,
}

impl Fixture {
    fn stage(&self) -> PathBuf {
        self.h.stage_dir(NAME).unwrap()
    }

    fn ids(&self) -> Vec<String> {
        self.h.list(NAME, 100).unwrap().into_iter().map(|v| v.id).collect()
    }

    fn out(&self, n: &str) -> PathBuf {
        self.dir.path().join(n)
    }
}

/// A document with two versions: v1, then v2 with one tile changed and one added.
fn two_versions() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let h = History::at(dir.path().join(FOLDER));
    let stage = h.stage_dir(NAME).unwrap();
    write_doc(&stage, r#"{"v":1}"#, &[("a.zst", b"tile-a"), ("b.zst", b"tile-b")]);
    let v1 = contents(&stage);
    let id1 = h.save(NAME, "first").unwrap().version.id;
    write_doc(&stage, r#"{"v":2}"#, &[("a.zst", b"tile-a"), ("c.zst", b"tile-c")]);
    let v2 = contents(&stage);
    let id2 = h.save(NAME, "second").unwrap().version.id;
    Fixture { dir, h, v1, v2, id1, id2 }
}

fn crash(at: Step) -> impl FnMut(Step) -> crate::Result<()> {
    move |s| if s == at { Err(Error::Io(format!("simulated crash at {s:?}"))) } else { Ok(()) }
}

#[test]
fn save_creates_history_and_skips_unchanged() {
    let f = two_versions();
    assert_ne!(f.id1, f.id2);
    let again = f.h.save(NAME, "nothing new").unwrap();
    assert!(!again.created);
    assert_eq!(again.version.id, f.id2);
    let list = f.h.list(NAME, 10).unwrap();
    assert_eq!(list.iter().map(|v| v.id.clone()).collect::<Vec<_>>(), vec![f.id2.clone(), f.id1.clone()]);
    assert_eq!(list[0].message, "second");
    assert!(list[0].time > 0);
    assert!(f.h.dir().join("history.git").join("photocraft-versions").is_file());
}

#[test]
fn checkout_writes_the_version_and_changes_no_history() {
    let f = two_versions();
    let dest = f.out("v1.pcraft");
    let v = f.h.checkout(NAME, &f.id1, &dest).unwrap();
    assert_eq!(v.id, f.id1);
    assert_eq!(v.message, "first");
    assert_eq!(contents(&dest), f.v1);
    let dest2 = f.out("v2.pcraft");
    f.h.checkout(NAME, &f.id2, &dest2).unwrap();
    assert_eq!(contents(&dest2), f.v2);
    assert_eq!(f.ids(), vec![f.id2.clone(), f.id1.clone()]);
    assert_eq!(contents(&f.stage()), f.v2, "the staging folder is untouched");
}

#[test]
fn checkout_refuses_a_folder_with_files() {
    let f = two_versions();
    let dest = f.out("busy");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("keep.txt"), b"mine").unwrap();
    assert!(f.h.checkout(NAME, &f.id1, &dest).is_err());
    assert_eq!(std::fs::read(dest.join("keep.txt")).unwrap(), b"mine");
    let empty = f.out("empty");
    std::fs::create_dir_all(&empty).unwrap();
    f.h.checkout(NAME, &f.id1, &empty).unwrap();
}

#[test]
fn list_respects_limit_and_empty_history() {
    let f = two_versions();
    assert_eq!(f.h.list(NAME, 1).unwrap().len(), 1);
    assert!(f.h.list(NAME, 0).unwrap().is_empty());
    assert!(f.h.list("other.pcraft", 10).unwrap().is_empty());
    let dir = tempfile::tempdir().unwrap();
    assert!(History::at(dir.path().join(FOLDER)).list(NAME, 10).unwrap().is_empty());
    assert!(!dir.path().join(FOLDER).exists(), "listing creates nothing");
}

#[test]
fn documents_in_one_history_keep_separate_versions() {
    let dir = tempfile::tempdir().unwrap();
    let h = History::at(dir.path().join(FOLDER));
    write_doc(&h.stage_dir("a.pcraft").unwrap(), r#"{"a":1}"#, &[("x.zst", b"a1")]);
    write_doc(&h.stage_dir("b.pcraft").unwrap(), r#"{"b":1}"#, &[("x.zst", b"b1")]);
    let a1 = h.save("a.pcraft", "a1").unwrap().version.id;
    let b1 = h.save("b.pcraft", "b1").unwrap().version.id;
    write_doc(&h.stage_dir("a.pcraft").unwrap(), r#"{"a":2}"#, &[("x.zst", b"a2")]);
    let a2 = h.save("a.pcraft", "a2").unwrap().version.id;
    let ids = |n: &str| h.list(n, 10).unwrap().into_iter().map(|v| v.id).collect::<Vec<_>>();
    assert_eq!(ids("a.pcraft"), vec![a2.clone(), a1.clone()]);
    assert_eq!(ids("b.pcraft"), vec![b1.clone()]);
    // A later version of a still holds b as it was.
    let dest = dir.path().join("b-at-a2");
    h.checkout("b.pcraft", &a2, &dest).unwrap();
    assert_eq!(contents(&dest), contents(&h.stage_dir("b.pcraft").unwrap()));
    // a's first version predates b.
    assert!(matches!(h.checkout("b.pcraft", &a1, &dir.path().join("none")), Err(Error::NoSuchVersion(_))));
}

#[test]
fn foreign_repository_is_never_written() {
    let dir = tempfile::tempdir().unwrap();
    let h = History::at(dir.path().join(FOLDER));
    std::fs::create_dir_all(h.dir()).unwrap();
    gix::init_bare(h.dir().join("history.git")).unwrap();
    write_doc(&h.stage_dir(NAME).unwrap(), "{}", &[]);
    let before = contents(&h.dir().join("history.git"));
    assert!(matches!(h.save(NAME, "x"), Err(Error::ForeignRepository(_))));
    assert!(matches!(h.list(NAME, 10), Err(Error::ForeignRepository(_))));
    assert!(matches!(h.checkout(NAME, &"0".repeat(40), &dir.path().join("o")), Err(Error::ForeignRepository(_))));
    assert_eq!(contents(&h.dir().join("history.git")), before);
}

#[test]
fn nothing_staged_is_refused_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let h = History::at(dir.path().join(FOLDER));
    assert!(matches!(h.save(NAME, "x"), Err(Error::NothingStaged(_))));
    std::fs::create_dir_all(h.stage_dir(NAME).unwrap()).unwrap();
    assert!(matches!(h.save(NAME, "x"), Err(Error::NothingStaged(_))));
    assert!(!h.dir().join("history.git").exists());
}

#[test]
fn bad_names_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let h = History::at(dir.path().join(FOLDER));
    let long = "a".repeat(256);
    for name in ["", ".", "..", "a/b", "a\\b", "a\0b", long.as_str()] {
        assert!(matches!(h.stage_dir(name), Err(Error::BadName(_))), "{name:?}");
        assert!(matches!(h.save(name, "x"), Err(Error::BadName(_))), "{name:?}");
        assert!(matches!(h.list(name, 1), Err(Error::BadName(_))), "{name:?}");
        assert!(matches!(h.checkout(name, "0", &dir.path().join("o")), Err(Error::BadName(_))), "{name:?}");
    }
}

#[test]
fn beside_puts_history_next_to_the_document() {
    let (h, name) = History::beside(Path::new("/art/poster.pcraft")).unwrap();
    assert_eq!(h.dir(), Path::new("/art/.photocraft-versions"));
    assert_eq!(name, "poster.pcraft");
    let (h, name) = History::beside(Path::new("/art/poster.pcraft/")).unwrap();
    assert_eq!((h.dir(), name.as_str()), (Path::new("/art/.photocraft-versions"), "poster.pcraft"));
    let (h, name) = History::beside(Path::new("photo.psd")).unwrap();
    assert_eq!((h.dir(), name.as_str()), (Path::new("./.photocraft-versions"), "photo.psd"));
    for p in ["", "/", "..", "/art/.."] {
        assert!(History::beside(Path::new(p)).is_err(), "{p:?}");
    }
}

#[test]
fn bad_version_ids_are_errors_not_panics() {
    let f = two_versions();
    let blob = gix::open(f.h.dir().join("history.git")).unwrap().write_blob(b"tile-a").unwrap().to_string();
    for (i, id) in ["", "zz", "0123", &"0".repeat(40), &"g".repeat(40), &"a".repeat(200), "é", &blob].into_iter().enumerate() {
        let dest = f.out(&format!("bad{i}"));
        assert!(matches!(f.h.checkout(NAME, id, &dest), Err(Error::NoSuchVersion(_))), "{id:?}");
        assert!(!dest.exists(), "{id:?}: nothing written");
    }
}

#[test]
fn messages_are_cleaned() {
    let dir = tempfile::tempdir().unwrap();
    let h = History::at(dir.path().join(FOLDER));
    let stage = h.stage_dir(NAME).unwrap();
    write_doc(&stage, "{}", &[("a.zst", b"1")]);
    assert_eq!(h.save(NAME, "  ").unwrap().version.message, "Version of art.pcraft");
    write_doc(&stage, "{}", &[("a.zst", b"2")]);
    assert_eq!(h.save(NAME, "a\0b").unwrap().version.message, "ab");
    write_doc(&stage, "{}", &[("a.zst", b"3")]);
    let kept = h.save(NAME, &"é".repeat(5000)).unwrap().version.message;
    assert!(kept.len() <= 4096 && kept.chars().all(|c| c == 'é'));
}

#[test]
fn preview_and_lock_are_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let h = History::at(dir.path().join(FOLDER));
    let stage = h.stage_dir(NAME).unwrap();
    write_doc(&stage, "{}", &[("a.zst", b"1")]);
    std::fs::create_dir_all(stage.join("composite")).unwrap();
    std::fs::write(stage.join("composite").join("preview.png"), b"p1").unwrap();
    let first = h.save(NAME, "x").unwrap().version.id;
    std::fs::write(stage.join("composite").join("preview.png"), b"p2").unwrap();
    std::fs::write(stage.join("save.lock"), b"").unwrap();
    let again = h.save(NAME, "x").unwrap();
    assert!(!again.created, "a redrawn preview is not a new version");
    assert_eq!(again.version.id, first);
}

#[test]
fn save_gives_up_on_a_held_save_lock() {
    let dir = tempfile::tempdir().unwrap();
    let h = History::at(dir.path().join(FOLDER));
    let stage = h.stage_dir(NAME).unwrap();
    write_doc(&stage, "{}", &[("a.zst", b"1")]);
    let held = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(stage.join("save.lock")).unwrap();
    held.lock().unwrap();
    assert!(matches!(h.save(NAME, "x"), Err(Error::Busy(_))));
    drop(held);
    assert!(h.save(NAME, "x").unwrap().created);
}

#[test]
fn crash_during_save_keeps_history_and_retry_works() {
    for at in [Step::Blobs, Step::Tree, Step::CommitObject] {
        let f = two_versions();
        write_doc(&f.stage(), r#"{"v":3}"#, &[("z.zst", b"tile-z")]);
        let v3 = contents(&f.stage());
        assert!(save_with(&f.h, NAME, "third", &mut crash(at)).is_err());
        assert_eq!(f.ids(), vec![f.id2.clone(), f.id1.clone()], "{at:?}: history unchanged");
        let saved = f.h.save(NAME, "third").unwrap();
        assert!(saved.created, "{at:?}");
        let dest = f.out("v3");
        f.h.checkout(NAME, &saved.version.id, &dest).unwrap();
        assert_eq!(contents(&dest), v3, "{at:?}");
    }
}

#[test]
fn interrupted_init_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let h = History::at(dir.path().join(FOLDER));
    std::fs::create_dir_all(h.dir().join("history.git.init").join("objects")).unwrap();
    write_doc(&h.stage_dir(NAME).unwrap(), "{}", &[("a.zst", b"1")]);
    assert!(h.save(NAME, "x").unwrap().created);
    assert!(!h.dir().join("history.git.init").exists());
}

#[test]
fn full_disk_is_named() {
    let e = classify_io(std::io::Error::from(std::io::ErrorKind::StorageFull));
    assert!(matches!(e, Error::DiskFull(_)), "{e:?}");
    assert!(e.to_string().contains("disk is full"));
    assert!(matches!(classify_io(std::io::Error::from(std::io::ErrorKind::PermissionDenied)), Error::Io(_)));
}

#[test]
fn history_opens_with_plain_git_tools() {
    let f = two_versions();
    let repo = gix::open(f.h.dir().join("history.git")).unwrap();
    assert!(repo.is_bare());
    assert_eq!(repo.head_id().unwrap().detach().to_string(), f.id2);
    let attrs = std::fs::read_to_string(repo.git_dir().join("info").join("attributes")).unwrap();
    assert_eq!(attrs, "* -text -diff\n");
}
