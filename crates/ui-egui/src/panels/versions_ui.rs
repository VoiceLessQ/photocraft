//! The Versions panel (a tab of the History card; a PhotoCraft extra) and its status-bar chip.
//!
//! Save the open document as a version with a note, see its versions newest first, restore one.
//! Every action is a `file.versions.*` engine command. The list is read from disk only when the
//! document's path changes or after an action here, and kept in egui's temp memory meanwhile, so
//! drawing the panel or the chip never touches the disk.

use egui::{Align2, RichText, Sense, Stroke, pos2, vec2};
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
        .map(|r| Row { id: s(r, "id"), message: s(r, "message"), time: r.get("time").and_then(Value::as_i64).unwrap_or(0), date: s(r, "date") })
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

fn save(app: &mut PhotocraftApp, ctx: &egui::Context, note: &str) -> bool {
    let r = app.run("file.versions.save", json!({"message": note}));
    forget(ctx);
    let saved = r.as_ref().is_ok_and(|v| v["created"] == true);
    let msg = r.map(|v| if v["created"] == true { tl!("Version saved").to_owned() } else { tl!("No changes since the last version").to_owned() });
    report(app, msg);
    saved
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
            crate::icons::paint(ui, icon, "clock", 11.0, if i == 0 { t.icon } else { t.text_faint });
            // Text stops short of the Restore button; a long note is clipped, its tooltip shows it all.
            let text = egui::Rect::from_min_max(pos2(rect.left() + 28.0, rect.top()), pos2(rect.right() - 34.0, rect.bottom()));
            let painter = ui.painter().with_clip_rect(text);
            painter.text(pos2(text.left(), rect.top() + 5.0), Align2::LEFT_TOP, &row.message, egui::FontId::proportional(12.0), t.text);
            let sub = format!("{}  ·  {}", when(row, now), row.id.get(..7).unwrap_or(&row.id));
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
