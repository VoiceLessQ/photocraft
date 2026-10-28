use egui_kittest::{Harness, kittest::Queryable};
use serde_json::json;

use super::{Row, TAB, rows, when};
use crate::PhotocraftApp;

fn row(time: i64) -> Row {
    Row { id: "a".repeat(40), message: "m".into(), time, date: "2026-01-02 03:04 UTC".into(), checkpoint: false }
}

#[test]
fn times_read_as_ago_then_as_a_date() {
    let now = Some(1_000_000);
    assert_eq!(when(&row(1_000_000), now), "just now");
    assert_eq!(when(&row(1_000_000 - 59), now), "just now");
    assert_eq!(when(&row(1_000_000 - 60), now), "1 minute ago");
    assert_eq!(when(&row(1_000_000 - 3_599), now), "59 minutes ago");
    assert_eq!(when(&row(1_000_000 - 7_200), now), "2 hours ago");
    assert_eq!(when(&row(1_000_000 - 86_400), now), "1 day ago");
    assert_eq!(when(&row(1_000_000 - 604_800), now), "2026-01-02 03:04 UTC");
    // A clock set back, or no clock at all, shows the date instead of a negative age.
    assert_eq!(when(&row(1_000_100), now), "2026-01-02 03:04 UTC");
    assert_eq!(when(&row(0), None), "2026-01-02 03:04 UTC");
    assert_eq!(when(&row(i64::MIN), now), "2026-01-02 03:04 UTC");
}

#[test]
fn rows_skip_entries_without_an_id() {
    let v = json!({"versions": [{"id": "b", "message": "x", "time": 5, "date": "d", "checkpoint": true}, {"message": "no id"}, 7, null]});
    assert_eq!(rows(&v), vec![Row { id: "b".into(), message: "x".into(), time: 5, date: "d".into(), checkpoint: true }]);
    assert!(rows(&json!({})).is_empty());
    assert!(rows(&json!({"versions": "nope"})).is_empty());
}

fn harness(app: PhotocraftApp, panel: bool) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(420.0, 480.0)).build_ui_state(
        move |ui, app: &mut PhotocraftApp| {
            if ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                if panel {
                    super::panel(app, ui);
                } else {
                    crate::panels::status_bar(app, ui);
                }
            }
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::default());
    h.run_steps(3);
    h
}

/// An app with one small document whose path is `<dir>/art.pcraft`.
fn app_with_doc(dir: &std::path::Path) -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 32, "height": 32, "background": "white"})).unwrap();
    app.session.active_mut().unwrap().path = Some(dir.join("art.pcraft").display().to_string());
    app.sync_views();
    app
}

#[test]
fn panel_saves_lists_and_restores() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app_with_doc(dir.path()), true);
    assert!(h.query_by_label_contains("No versions yet").is_some());
    h.get_by_label("Save Version").click();
    h.run_steps(3);
    assert_eq!(h.state().ui.status, "Version saved");
    assert_eq!(h.get_all_by_label("Restore this version (Undo brings back what you had)").count(), 1);
    h.state_mut().run("image.adjustments.invert", json!({})).unwrap();
    h.get_by_label("Save Version").click();
    h.run_steps(3);
    assert_eq!(h.get_all_by_label("Restore this version (Undo brings back what you had)").count(), 2);
    // Saving again with nothing new says so and adds no row.
    h.get_by_label("Save Version").click();
    h.run_steps(3);
    assert_eq!(h.state().ui.status, "No changes since the last version");
    assert_eq!(h.get_all_by_label("Restore this version (Undo brings back what you had)").count(), 2);
    // The older row is the white original.
    h.get_all_by_label("Restore this version (Undo brings back what you had)").nth(1).unwrap().click();
    h.run_steps(3);
    assert!(h.state().ui.status.starts_with("Restored"), "{}", h.state().ui.status);
    assert!(!h.state().ui.status_error);
    assert_eq!(h.get_all_by_label("Restore this version (Undo brings back what you had)").count(), 3, "the restore is a new version on top");
}

#[test]
fn panel_explains_what_it_needs() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let h = harness(PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default()), true);
    assert!(h.query_by_label("No document").is_some());
    app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
    app.sync_views();
    let h = harness(app, true);
    assert!(h.query_by_label_contains("Save the document once").is_some());
    assert!(h.query_by_label("Save Version").is_none());
}

#[test]
fn status_chip_counts_versions_and_opens_the_panel() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with_doc(dir.path());
    app.run("file.versions.save", json!({})).unwrap();
    app.run("image.adjustments.invert", json!({})).unwrap();
    app.run("file.versions.save", json!({})).unwrap();
    app.ui.panels.history = false;
    let mut h = harness(app, false);
    h.get_by_label("2 versions").click();
    h.run_steps(2);
    assert!(h.state().ui.panels.history);
    assert_eq!(h.state().ui.dock_tabs.history, TAB);
}

#[test]
fn chip_is_hidden_for_unsaved_documents() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
    app.sync_views();
    let h = harness(app, false);
    assert!(h.query_by_label("Versions").is_none());
    assert!(h.query_by_label_contains("version").is_none());
}

#[test]
fn rows_sit_right_under_the_note_field() {
    // A right-to-left note row without a fixed height took the whole panel and pushed the list
    // out of sight, so a click on Restore landed nowhere.
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app_with_doc(dir.path()), true);
    h.state_mut().run("file.versions.save", json!({})).unwrap();
    h.state_mut().run("image.adjustments.invert", json!({})).unwrap();
    h.state_mut().run("file.versions.save", json!({})).unwrap();
    super::forget(&h.ctx);
    h.run_steps(3);
    let field = h.get_by_label("Version note").rect();
    let first = h.get_all_by_label("Restore this version (Undo brings back what you had)").map(|n| n.rect()).fold(f32::MAX, |top, r| top.min(r.top()));
    assert!(first - field.bottom() < 30.0, "note field ends at {}, first row button at {first}", field.bottom());
    h.get_all_by_label("Restore this version (Undo brings back what you had)").next().unwrap().click();
    h.run_steps(3);
    assert!(h.state().ui.status.starts_with("Restored"), "{}", h.state().ui.status);
}

#[test]
fn drawing_the_chip_keeps_the_status_line() {
    // The chip reads the list while the status bar draws; through `app.run` that cleared an error
    // status (a failed save's message vanished on the next frame).
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with_doc(dir.path());
    app.ui.status = "Couldn't save art.pcraft: disk full".into();
    app.ui.status_error = true;
    let h = harness(app, false);
    assert_eq!(h.state().ui.status, "Couldn't save art.pcraft: disk full");
    assert!(h.state().ui.status_error);
    assert!(h.query_by_label("Versions").is_some());
}

/// Finish every checkpoint the shell started and hand the results to the UI.
fn drain(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let jobs: Vec<_> = app.versions.pending.iter().map(|(j, _, _)| *j).collect();
    for job in jobs {
        let _ = app.session.wait_job(job);
    }
    crate::jobs_ui::tick(app, ctx);
    assert!(app.versions.pending.is_empty());
}

fn fire(app: &mut PhotocraftApp, ctx: &egui::Context) {
    app.versions.next_ms = f64::MIN_POSITIVE;
    super::tick(app, ctx);
    drain(app, ctx);
}

fn listed(app: &mut PhotocraftApp) -> Vec<(String, bool)> {
    let v = app.session.execute("file.versions.list", json!({})).unwrap();
    rows(&v).into_iter().map(|r| (r.message, r.checkpoint)).collect()
}

#[test]
fn the_timer_checkpoints_changed_documents_once_they_have_a_version() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = app_with_doc(dir.path());
    app.run("prefs.set", json!({"values": {"fileHandling.versionMinutes": 1}})).unwrap();
    super::tick(&mut app, &ctx);
    assert!(app.versions.next_ms > 0.0, "armed");
    super::tick(&mut app, &ctx);
    assert!(app.versions.pending.is_empty(), "not due yet");
    // Changed, but no version yet: no history appears beside the file.
    app.run("image.adjustments.invert", json!({})).unwrap();
    fire(&mut app, &ctx);
    assert!(!dir.path().join(".photocraft-versions").join("history.git").exists());
    app.run("file.versions.save", json!({"message": "named"})).unwrap();
    app.run("image.adjustments.invert", json!({})).unwrap();
    fire(&mut app, &ctx);
    assert_eq!(listed(&mut app), vec![("Auto-saved".into(), true), ("named".into(), false)]);
    // Nothing changed since: the next firing starts no job.
    app.versions.next_ms = f64::MIN_POSITIVE;
    super::tick(&mut app, &ctx);
    assert!(app.versions.pending.is_empty());
    // Off: never fires.
    app.run("prefs.set", json!({"values": {"fileHandling.versionMinutes": 0}})).unwrap();
    app.run("image.adjustments.invert", json!({})).unwrap();
    fire(&mut app, &ctx);
    assert_eq!(listed(&mut app).len(), 2);
    assert_eq!(app.versions.next_ms, 0.0);
}

#[test]
fn the_timer_waits_while_a_button_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = app_with_doc(dir.path());
    app.run("file.versions.save", json!({})).unwrap();
    super::tick(&mut app, &ctx);
    app.run("image.adjustments.invert", json!({})).unwrap();
    let press = |down| egui::RawInput {
        events: vec![egui::Event::PointerButton {
            pos: egui::pos2(5.0, 5.0),
            button: egui::PointerButton::Primary,
            pressed: down,
            modifiers: Default::default(),
        }],
        ..Default::default()
    };
    // The fork asserts that a frame's texture changes are taken, not dropped.
    let frame = |input| ctx.run_ui(input, |_| {}).textures_delta.clear();
    frame(press(true));
    app.versions.next_ms = f64::MIN_POSITIVE;
    super::tick(&mut app, &ctx);
    assert!(app.versions.pending.is_empty(), "mid-stroke: waits");
    frame(press(false));
    super::tick(&mut app, &ctx);
    drain(&mut app, &ctx);
    assert_eq!(listed(&mut app).len(), 2);
}

#[test]
fn saving_the_file_saves_a_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = app_with_doc(dir.path());
    let doc = app.session.active().unwrap().doc.id;
    let path = app.session.active().unwrap().path.clone().unwrap();
    // A document with no versions gets none from saving it.
    app.saved(doc, 1, &path, &[], false);
    drain(&mut app, &ctx);
    assert!(listed(&mut app).is_empty());
    app.run("file.versions.save", json!({"message": "named"})).unwrap();
    app.run("image.adjustments.invert", json!({})).unwrap();
    app.saved(doc, 2, &path, &[], true);
    drain(&mut app, &ctx);
    assert_eq!(listed(&mut app).len(), 1, "a copy is not the document's file");
    app.saved(doc, 2, &path, &[], false);
    drain(&mut app, &ctx);
    assert_eq!(listed(&mut app), vec![("Saved to file".into(), true), ("named".into(), false)]);
    app.run("prefs.set", json!({"values": {"fileHandling.versionOnSave": false}})).unwrap();
    app.run("image.adjustments.invert", json!({})).unwrap();
    app.saved(doc, 3, &path, &[], false);
    drain(&mut app, &ctx);
    assert_eq!(listed(&mut app).len(), 2, "off: saving adds none");
}
