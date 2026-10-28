//! The Versions panel (a tab of the History card; a PhotoCraft extra) and its status-bar chip.
//!
//! Save the open document as a version with a note, see its versions newest first, restore one.
//! Every action is a `file.versions.*` engine command. The list is read from disk only when the
//! document's path changes or after an action here, and kept in egui's temp memory meanwhile, so
//! drawing the panel or the chip never touches the disk.
//!
//! Checkpoints ([`tick`], [`saved`]): Preferences › File Handling saves one of each changed
//! document every few minutes and on File › Save, in the background, once the document has a
//! version. They are listed with the rest, dimmed.

use std::collections::HashMap;

use egui::{Align2, RichText, Sense, Stroke, pos2, vec2};
use photocraft_doc::DocId;
use photocraft_engine::jobs::{JobEvent, JobId, JobOutcome, Started};
use photocraft_engine::vcs_cmds;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;
use crate::widgets;

/// The panel's tab in the History card (after History, Actions and Layer Comps).
pub(crate) const TAB: usize = 3;

#[derive(Clone, Debug, PartialEq)]
struct Row {
    id: String,
    message: String,
    time: i64,
    date: String,
    checkpoint: bool,
}

/// Checkpoint bookkeeping (not serialised).
#[derive(Debug, Default)]
pub struct Auto {
    /// When the timer fires next (`gpu_canvas::now_ms`); 0 = not armed.
    next_ms: f64,
    /// Each open document's revision at its last checkpoint, or when it was first seen.
    seen: HashMap<DocId, u64>,
    /// Checkpoints running: job, document, the revision it started from.
    pending: Vec<(JobId, DocId, u64)>,
}

/// The versions of the document at `path`, as last read.
#[derive(Clone, Debug)]
struct Listing {
    path: String,
    rows: Vec<Row>,
    error: Option<String>,
}

fn listing_id() -> egui::Id {
    egui::Id::unique("versions-panel-listing")
}

fn note_id() -> egui::Id {
    egui::Id::unique("versions-panel-note")
}

fn rows(v: &Value) -> Vec<Row> {
    let s = |r: &Value, k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    v.get("versions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|r| Row {
            id: s(r, "id"),
            message: s(r, "message"),
            time: r.get("time").and_then(Value::as_i64).unwrap_or(0),
            date: s(r, "date"),
            checkpoint: r.get("checkpoint").and_then(Value::as_bool).unwrap_or(false),
        })
        .filter(|r| !r.id.is_empty())
        .collect()
}

/// The active document's listing: cached, read again when its path changed or after `forget`.
fn listing(app: &mut PhotocraftApp, ctx: &egui::Context, path: &str) -> Listing {
    if let Some(cached) = ctx.data(|d| d.get_temp::<Listing>(listing_id())).filter(|l| l.path == path) {
        return cached;
    }
    // A read-only query drawn with the status bar: straight to the session, so it leaves the
    // status line, the views and the last-command record alone (`app.run` updates all three).
    let fresh = match app.session.execute("file.versions.list", json!({})) {
        Ok(v) => Listing { path: path.to_owned(), rows: rows(&v), error: None },
        Err(e) => Listing { path: path.to_owned(), rows: Vec::new(), error: Some(e.to_string()) },
    };
    ctx.data_mut(|d| d.insert_temp(listing_id(), fresh.clone()));
    fresh
}

fn forget(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<Listing>(listing_id()));
}

fn now() -> Option<i64> {
    #[cfg(not(target_arch = "wasm32"))]
    return std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().and_then(|d| i64::try_from(d.as_secs()).ok());
    #[cfg(target_arch = "wasm32")]
    None
}

/// "just now", "5 minutes ago" … up to a week; older ones (and a clock set back) show the date.
fn when(row: &Row, now: Option<i64>) -> String {
    let lang = crate::i18n::current();
    let Some(ago) = now.map(|n| n.saturating_sub(row.time)).filter(|a| *a >= 0) else { return row.date.clone() };
    let n = |unit: i64| u64::try_from(ago / unit).unwrap_or(0);
    match ago {
        0..60 => tl!("just now").to_owned(),
        60..3_600 => crate::i18n::trn(lang, n(60), "{n} minute ago", "{n} minutes ago"),
        3_600..86_400 => crate::i18n::trn(lang, n(3_600), "{n} hour ago", "{n} hours ago"),
        86_400..604_800 => crate::i18n::trn(lang, n(86_400), "{n} day ago", "{n} days ago"),
        _ => row.date.clone(),
    }
}

fn report(app: &mut PhotocraftApp, result: Result<String, String>) {
    match result {
        Ok(msg) => {
            app.ui.status = msg;
            app.ui.status_error = false;
        }
        Err(e) => {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }
}

fn saved_message(v: &Value) -> String {
    if v["created"] == true { tl!("Version saved").to_owned() } else { tl!("No changes since the last version").to_owned() }
}

/// Save a version; true when one was made or is being saved in the background.
fn save(app: &mut PhotocraftApp, ctx: &egui::Context, note: &str) -> bool {
    let r = app.run(vcs_cmds::SAVE, json!({"message": note}));
    forget(ctx);
    // In the background the result comes to `on_job_event`.
    if r.as_ref().is_ok_and(|v| v["pending"] == true) {
        report(app, Ok(tl!("Saving a version…").to_owned()));
        return true;
    }
    let saved = r.as_ref().is_ok_and(|v| v["created"] == true);
    report(app, r.map(|v| saved_message(&v)));
    saved
}

/// Message of a checkpoint saved by the timer.
const AUTO_MESSAGE: &str = "Auto-saved";
/// Message of a checkpoint saved with the file.
const SAVED_MESSAGE: &str = "Saved to file";

fn start(app: &mut PhotocraftApp, doc: DocId, revision: u64, message: &str) {
    match vcs_cmds::start_checkpoint(&mut app.session, doc, message) {
        Ok(Started::Job(job)) => app.versions.pending.push((job, doc, revision)),
        Ok(Started::Done(_)) => {
            app.versions.seen.insert(doc, revision);
        }
        Err(e) => report(app, Err(crate::i18n::fmt(tl!("Checkpoint failed: {error}"), &[("error", &e.to_string())]))),
    }
}

/// Per frame: save a checkpoint of every changed document when the timer is due and the app is
/// idle (no button held, no dialog, no text being typed).
pub(crate) fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if cfg!(target_arch = "wasm32") {
        return;
    }
    let live: HashMap<DocId, u64> = app.session.documents().iter().map(|d| (d.doc.id, d.revision)).collect();
    app.versions.seen.retain(|id, _| live.contains_key(id));
    for (id, rev) in &live {
        app.versions.seen.entry(*id).or_insert(*rev);
    }
    let minutes = app.session.prefs().file_handling.version_minutes.min(60);
    if minutes == 0 {
        app.versions.next_ms = 0.0;
        return;
    }
    let now = crate::gpu_canvas::now_ms();
    let interval = f64::from(minutes) * 60_000.0;
    // Armed on first use, and again when the interval was shortened.
    if app.versions.next_ms == 0.0 || app.versions.next_ms > now + interval {
        app.versions.next_ms = now + interval;
    }
    if now < app.versions.next_ms {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((app.versions.next_ms - now) / 1000.0));
        return;
    }
    if ctx.input(|i| i.pointer.any_down()) || crate::panel_docking::modal_open(app, ctx) || ctx.text_edit_focused() {
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
        return;
    }
    app.versions.next_ms = now + interval;
    let due: Vec<(DocId, u64)> = app
        .session
        .documents()
        .iter()
        .filter(|d| d.path.is_some() && app.versions.seen.get(&d.doc.id) != Some(&d.revision))
        .filter(|d| !app.versions.pending.iter().any(|(_, doc, _)| *doc == d.doc.id) && app.session.job_on(d.doc.id).is_none())
        .map(|d| (d.doc.id, d.revision))
        .collect();
    for (doc, rev) in due {
        start(app, doc, rev, AUTO_MESSAGE);
    }
}

/// The document's file was saved (File › Save, not a copy): checkpoint it too, when Preferences ›
/// File Handling asks for that.
pub(crate) fn saved(app: &mut PhotocraftApp, doc: DocId) {
    if cfg!(target_arch = "wasm32") || !app.session.prefs().file_handling.version_on_save || app.versions.pending.iter().any(|(_, d, _)| *d == doc) {
        return;
    }
    let Some(rev) = app.session.documents().iter().find(|d| d.doc.id == doc).map(|d| d.revision) else { return };
    start(app, doc, rev, SAVED_MESSAGE);
}

/// A checkpoint the shell started; Beep When Done stays quiet for these.
pub(crate) fn is_checkpoint(app: &PhotocraftApp, job: JobId) -> bool {
    app.versions.pending.iter().any(|(j, _, _)| *j == job)
}

/// A version save that ran in the background ended. True when it was one (the event is handled).
pub(crate) fn on_job_event(app: &mut PhotocraftApp, ctx: &egui::Context, e: &JobEvent) -> bool {
    if e.command != vcs_cmds::SAVE {
        return false;
    }
    forget(ctx);
    if let Some(i) = app.versions.pending.iter().position(|(j, _, _)| *j == e.id) {
        let (_, doc, rev) = app.versions.pending.remove(i);
        match &e.outcome {
            // Quiet: the chip's count is the only sign.
            JobOutcome::Done(_) => {
                app.versions.seen.insert(doc, rev);
            }
            JobOutcome::Failed(err) => report(app, Err(crate::i18n::fmt(tl!("Checkpoint failed: {error}"), &[("error", err)]))),
            JobOutcome::Cancelled => {}
        }
        return true;
    }
    match &e.outcome {
        JobOutcome::Done(v) => report(app, Ok(saved_message(v))),
        JobOutcome::Failed(err) => report(app, Err(err.clone())),
        JobOutcome::Cancelled => report(app, Ok(crate::i18n::fmt(tl!("Cancelled {label}"), &[("label", &e.label)]))),
    }
    true
}

fn restore(app: &mut PhotocraftApp, ctx: &egui::Context, row: &Row) {
    let r = app.run("file.versions.restore", json!({"id": row.id}));
    forget(ctx);
    let msg = r.map(|_| crate::i18n::fmt(tl!("Restored “{name}” (Undo brings back what you had)"), &[("name", &row.message)]));
    report(app, msg);
}

/// Show the Versions tab, in the docking layout or the classic dock.
pub(crate) fn open(app: &mut PhotocraftApp) {
    if app.ui.docking.is_some() {
        if let Err(e) = app.run("window.panel.activate", json!({"panel": "versions"})) {
            report(app, Err(e));
        }
        return;
    }
    app.ui.dock_tabs.history = TAB;
    crate::dock::reveal(app, crate::dock::Group::History);
}

fn faint(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(t.text_faint).size(11.5)).wrap());
}

pub fn panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    if cfg!(target_arch = "wasm32") {
        faint(ui, &t, tl!("Versions need the desktop app: the browser has no folder to keep them in."));
        return;
    }
    let Some(st) = app.session.active() else {
        faint(ui, &t, tl!("No document"));
        return;
    };
    let Some(path) = st.path.clone() else {
        faint(ui, &t, tl!("Save the document once to keep versions of it. They go in a hidden folder beside the file."));
        return;
    };
    let ctx = ui.ctx().clone();

    let mut note = ui.data(|d| d.get_temp::<String>(note_id())).unwrap_or_default();
    let mut do_save = false;
    // Button first, right to left, and the note field takes what is left: the row never widens
    // the panel (an overflowing row would push every list row past the panel's edge).
    // A fixed height: a bare right-to-left layout would take the whole panel height.
    let row = vec2(ui.available_width(), widgets::button_height(ui.ctx()) + 2.0);
    ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
        do_save |= widgets::primary_button(ui, tl!("Save Version"), 0.0).clicked();
        let w = ui.available_width().max(40.0);
        let field = widgets::text_field(ui, note_id().with("field"), &mut note, tl!("What changed?"), w, false).accessible_name(tl!("Version note"));
        do_save |= field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    });
    if do_save && save(app, &ctx, &note) {
        note.clear();
    }
    ui.data_mut(|d| d.insert_temp(note_id(), note));
    ui.add_space(4.0);

    let listing = listing(app, &ctx, &path);
    if let Some(e) = &listing.error {
        ui.add(egui::Label::new(RichText::new(e).color(t.warning).size(11.5)).wrap());
        return;
    }
    if listing.rows.is_empty() {
        faint(ui, &t, tl!("No versions yet. Save one to start the history; your file stays as it is."));
        return;
    }
    let now = now();
    let row_h = 38.0;
    let mut chosen: Option<Row> = None;
    let max_h = (ui.available_height() - 4.0).clamp(80.0, 420.0);
    egui::ScrollArea::vertical().id_salt("versions-rows").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
        for (i, row) in listing.rows.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
            if i == 0 {
                ui.painter().rect_filled(rect, 0.0, t.row_selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 0.0, t.hover.gamma_multiply(0.5));
            }
            let icon = egui::Rect::from_center_size(pos2(rect.left() + 14.0, rect.top() + 13.0), vec2(12.0, 12.0));
            crate::icons::paint(ui, icon, "clock", 11.0, if i == 0 && !row.checkpoint { t.icon } else { t.text_faint });
            // Text stops short of the Restore button; a long note is clipped, its tooltip shows it all.
            let text = egui::Rect::from_min_max(pos2(rect.left() + 28.0, rect.top()), pos2(rect.right() - 34.0, rect.bottom()));
            let painter = ui.painter().with_clip_rect(text);
            // Checkpoints are dimmed and say so: the versions people named stand out.
            let (message, color) = if row.checkpoint { (tl!(row.message.as_str()).to_owned(), t.text_faint) } else { (row.message.clone(), t.text) };
            painter.text(pos2(text.left(), rect.top() + 5.0), Align2::LEFT_TOP, &message, egui::FontId::proportional(12.0), color);
            let kind = if row.checkpoint { format!("{}  ·  ", tl!("Checkpoint")) } else { String::new() };
            let sub = format!("{kind}{}  ·  {}", when(row, now), row.id.get(..7).unwrap_or(&row.id));
            painter.text(pos2(text.left(), rect.top() + 21.0), Align2::LEFT_TOP, sub, egui::FontId::proportional(10.5), t.text_faint);
            resp.on_hover_text(format!("{}\n{}", row.message, row.date));
            // Always shown, so the list works from the keyboard too.
            let button = egui::Rect::from_min_size(pos2(rect.right() - 30.0, rect.center().y - 12.0), vec2(24.0, 24.0));
            // Every row's button has the same label, so each needs its own id.
            let mut child = ui.new_child(egui::UiBuilder::new().id_salt(("versions-restore", &row.id)).max_rect(button));
            if crate::icons::button(&mut child, "undo-2", 24.0, false, "Restore this version (Undo brings back what you had)").clicked() {
                chosen = Some(row.clone());
            }
            ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, t.separator));
        }
    });
    if let Some(row) = chosen {
        restore(app, &ctx, &row);
    }
}

/// The status-bar chip: how many versions the document has; a click opens the Versions panel.
pub(crate) fn status_chip(app: &mut PhotocraftApp, ui: &mut egui::Ui, height: f32) {
    if cfg!(target_arch = "wasm32") {
        return;
    }
    let Some(path) = app.session.active().and_then(|d| d.path.clone()) else { return };
    let ctx = ui.ctx().clone();
    let n = listing(app, &ctx, &path).rows.len();
    let text = if n == 0 { tl!("Versions").to_owned() } else { crate::i18n::trn(crate::i18n::current(), n as u64, "{n} version", "{n} versions") };
    if widgets::compact_button(ui, &text, height).on_hover_text(tl!("Open the Versions panel")).clicked() {
        open(app);
    }
}

#[cfg(test)]
mod tests;
