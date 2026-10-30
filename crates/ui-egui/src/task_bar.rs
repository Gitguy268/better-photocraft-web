//! The Contextual Task Bar: a small floating bar under the canvas with the active layer's quick
//! actions, one click away (Photoshop has a larger one; this is the minimal version): for a pixel
//! layer, the local AI commands (Remove Background, Select Subject, AI Upscale, AI Denoise).
//! Window › Contextual Task Bar shows and hides it.
//!
//! It shows with every tool, as Photoshop's does, so the AI is never more than a click away. It
//! sits over the bottom of the canvas, so it steps aside while a drag that began on the canvas
//! (a brush stroke, a marquee) is under way.
//!
//! The actions are the same list the Properties panel shows ([`crate::props_layout::quick_actions`])
//! and run through the same commands.

use serde_json::json;

use crate::PhotocraftApp;

/// How many actions fit in the bar; the Properties panel lists the rest.
const MAX_ACTIONS: usize = 4;

/// The actions the bar offers now: none while there is no document, a session that owns the
/// canvas (transform, crop frame, a dialog) or nothing to act on.
pub fn actions(app: &PhotocraftApp) -> Vec<(&'static str, &'static str)> {
    if !app.ui.panels.task_bar || app.jobs.focus.is_some() || app.ui.transform.is_some() || !app.ui.dialogs.is_empty() || crate::crop_ui::pending(app) {
        return Vec::new();
    }
    let Some(st) = app.session.active() else { return Vec::new() };
    let Some(layer) = st.active_layer.and_then(|id| st.doc.layer(id)) else { return Vec::new() };
    // Pixel layers only, for now: their quick actions are the ones worth a place on the canvas.
    if !matches!(layer.content, photocraft_doc::LayerContent::Raster(_)) {
        return Vec::new();
    }
    crate::props_layout::quick_actions(&layer.content).iter().copied().filter(|(_, id)| crate::menus::is_enabled(app, id)).take(MAX_ACTIONS).collect()
}

/// Draw the bar at the bottom of the canvas.
pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let actions = actions(app);
    let id = egui::Id::unique("contextual-task-bar");
    // A drag that began elsewhere (on the canvas) keeps the bar out of its way until release.
    let bar = ctx.memory(|m| m.area_rect(id));
    let dragging_elsewhere = ctx.input(|i| i.pointer.any_down() && i.pointer.press_origin().is_some_and(|p| bar.is_none_or(|r| !r.contains(p))));
    if actions.is_empty() || dragging_elsewhere {
        return;
    }
    let canvas = app.last_canvas_rect;
    let mut clicked: Option<&'static str> = None;
    egui::Area::new(id)
        .order(egui::Order::Middle)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .fixed_pos(egui::pos2(canvas.center().x, canvas.bottom() - 28.0))
        .constrain_to(canvas)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (label, id) in &actions {
                        if crate::widgets::secondary_button(ui, tl!(label), 0.0).clicked() {
                            clicked = Some(id);
                        }
                    }
                    if actions.iter().any(|a| a.1 == "layer.removeBackground")
                        && crate::icons::button(ui, "settings", 24.0, false, tl!("Remove Background Options…")).clicked()
                    {
                        clicked = Some("layer.removeBackgroundOptions");
                    }
                    if crate::icons::button(ui, "x", 24.0, false, tl!("Hide")).clicked() {
                        clicked = Some("window.toggle.taskBar");
                    }
                });
            });
        });
    if let Some(id) = clicked
        && let Err(e) = crate::menus::invoke(app, ctx, id, json!({}))
    {
        crate::notices::error(app, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.session.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
        // A normal pixel layer (the locked Background offers nothing to remove a background from).
        app.session.execute("layer.new.layer", json!({})).unwrap();
        app.ui.tool = crate::state::Tool::Move;
        app
    }

    #[test]
    fn a_pixel_layer_gets_remove_background_and_select_subject() {
        let app = app();
        let ids: Vec<_> = actions(&app).iter().map(|a| a.1).collect();
        assert_eq!(ids[..3], ["layer.removeBackground", "select.subject", "filter.ai.upscale"]);
        assert!(actions(&PhotocraftApp::new(photocraft_engine::Session::new(), Default::default())).is_empty(), "no document, no bar");
        // Every tool shows it, the default Brush included.
        let mut painting = self::app();
        for tool in [crate::state::Tool::Brush, crate::state::Tool::Crop, crate::state::Tool::Eraser, crate::state::Tool::Lasso] {
            painting.ui.tool = tool;
            assert_eq!(actions(&painting).len(), ids.len(), "{tool:?}");
        }
    }

    #[test]
    fn a_drag_from_the_canvas_hides_it_until_release() {
        let mut app = app();
        let ctx = egui::Context::default();
        crate::PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::ALL[0]);
        app.last_canvas_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let shown = |app: &mut PhotocraftApp, events: Vec<egui::Event>| {
            ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| show(app, ui.ctx())).textures_delta.clear();
            ctx.memory(|m| m.areas().visible_layer_ids()).iter().any(|l| l.id == egui::Id::unique("contextual-task-bar"))
        };
        assert!(shown(&mut app, vec![]));
        let at = egui::pos2(100.0, 100.0);
        let press = egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() };
        assert!(!shown(&mut app, vec![egui::Event::PointerMoved(at), press]), "a stroke on the canvas");
        let release = egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() };
        shown(&mut app, vec![release]);
        assert!(shown(&mut app, vec![]), "back after the release");
    }

    #[test]
    fn the_window_menu_hides_and_shows_it_and_the_choice_is_saved() {
        let mut app = app();
        let ctx = egui::Context::default();
        assert_eq!(crate::menus::checked(&app, "window.toggle.taskBar"), Some(true));
        crate::menus::invoke(&mut app, &ctx, "window.toggle.taskBar", json!({})).unwrap();
        assert!(actions(&app).is_empty() && crate::menus::checked(&app, "window.toggle.taskBar") == Some(false));
        let saved = serde_json::to_value(&app.ui.panels).unwrap();
        assert_eq!(saved["task_bar"], false);
        // Settings saved before the bar existed show it.
        let mut old = saved.clone();
        old.as_object_mut().unwrap().remove("task_bar");
        assert!(serde_json::from_value::<crate::state::Panels>(old).unwrap().task_bar);
    }

    #[test]
    fn it_stays_out_of_the_way_of_dialogs_and_draws_without_a_gpu() {
        let mut app = app();
        let ctx = egui::Context::default();
        crate::PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::ALL[0]);
        ctx.run_ui(Default::default(), |ui| show(&mut app, ui.ctx())).textures_delta.clear();
        crate::menus::invoke(&mut app, &ctx, "image.canvasSize", json!({})).unwrap();
        assert!(!app.ui.dialogs.is_empty() && actions(&app).is_empty());
    }
}
