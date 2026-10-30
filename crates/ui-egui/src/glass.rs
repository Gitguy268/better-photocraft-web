//! Liquid Glass in the shell: where the GPU backdrop and the floating panes (`gpu_canvas/glass.rs`)
//! go into egui's paint lists. Active only while the theme's tokens carry a `theme::Glass`
//! (the glass themes, with transparency not reduced and the GPU canvas running); every other
//! theme pays one token lookup per pass.
//!
//! - Before the app paints, the window-sized backdrop is added to the background layer, so the
//!   translucent panel fills on top of it read as frosted glass.
//! - After the app painted, every surface floating above the window (menus, popups, tooltips,
//!   dialogs, floating panels, full-window editors) and every card on the dock gets a pane: it is recognised by its fill,
//!   one of the theme's translucent surface colours, and its rectangle is wrapped in place with
//!   the pane's body below it and the lit rim above it. So the pane has the surface's exact
//!   shape in the same frame, and no widget knows about glass.

use egui::{Color32, Rect, Shape};

use crate::gpu_canvas::glass as gpu;
use crate::theme::Tokens;

pub(crate) struct Plugin;

impl egui::Plugin for Plugin {
    fn debug_name(&self) -> &'static str {
        "photocraft-liquid-glass"
    }

    fn on_begin_pass(&mut self, ui: &mut egui::Ui) {
        // The backdrop shows the main window's canvas: other OS windows keep the plain tints.
        if let Some(glass) = Tokens::get(ui.ctx()).glass
            && ui.ctx().viewport_id() == egui::ViewportId::ROOT
        {
            ui.painter().add(gpu::backdrop(ui.ctx().viewport_rect(), glass));
        }
    }

    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx();
        let t = Tokens::get(ctx);
        if t.glass.is_none() || ctx.viewport_id() != egui::ViewportId::ROOT {
            return;
        }
        let surfaces = [t.card, t.chrome, t.dock, t.canvas];
        let mut layers: Vec<_> = ctx.memory(|m| m.areas().visible_layer_ids()).into_iter().filter(|l| l.order != egui::Order::Background).collect();
        // Floating surfaces first: they must never go without a pane (it hides what is under them).
        layers.push(egui::LayerId::background());
        let mut slot = 0;
        ctx.graphics_mut(|graphics| {
            for layer in layers {
                let Some(list) = graphics.get_mut(layer) else { continue };
                // The window's bars and dock already lie on the backdrop; the cards on the dock
                // are panes of their own, so they are as clear as a floating one.
                let surfaces = if layer.order == egui::Order::Background { &surfaces[..1] } else { &surfaces[..] };
                let mut glazed = Vec::new();
                for i in 0..list.next_idx().0 {
                    list.mutate_shape(egui::layers::ShapeIdx(i), |clipped| glaze(&mut clipped.shape, surfaces, &mut glazed, &mut slot, 0));
                }
            }
        });
    }
}

/// How visible `fill` is if it is the translucent surface colour `surface`, possibly faded (egui
/// fades windows and menus in by scaling their colours): `None` when it is another colour.
fn tint_opacity(fill: Color32, surface: Color32) -> Option<f32> {
    if surface.a() == 255 || surface.a() == 0 || fill.a() < 8 || fill.a() > surface.a() {
        return None;
    }
    let k = f32::from(fill.a()) / f32::from(surface.a());
    let same = |f: u8, s: u8| (f32::from(f) - f32::from(s) * k).abs() <= 2.0;
    (same(fill.r(), surface.r()) && same(fill.g(), surface.g()) && same(fill.b(), surface.b())).then_some(k)
}

/// Put a glass pane under (and its rim over) every surface-coloured rectangle in `shape` that
/// isn't inside one already glazed: a card inside a dialog stays a tint on the dialog's glass.
fn glaze(shape: &mut Shape, surfaces: &[Color32], glazed: &mut Vec<Rect>, slot: &mut usize, depth: u8) {
    match shape {
        // A frame with a shadow is a list of the shadow and the frame.
        Shape::Vec(shapes) if depth < 2 => shapes.iter_mut().for_each(|s| glaze(s, surfaces, glazed, slot, depth + 1)),
        Shape::Rect(r) => {
            let (rect, radius) = (r.rect, r.corner_radius);
            if *slot >= gpu::MAX_PANES || r.blur_width > 0.0 || r.brush.is_some() || rect.width() < 24.0 || rect.height() < 16.0 {
                return;
            }
            let Some(opacity) = surfaces.iter().find_map(|s| tint_opacity(r.fill, *s)) else { return };
            if glazed.iter().any(|g| g.contains_rect(rect)) {
                return;
            }
            glazed.push(rect);
            let frame = std::mem::replace(shape, Shape::Noop);
            *shape = Shape::Vec(vec![gpu::pane(*slot, rect, radius, opacity, false), frame, gpu::pane(*slot, rect, radius, opacity, true)]);
            *slot += 1;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeKind;

    #[test]
    fn surface_tints_are_recognised_through_a_fade() {
        let card = Tokens::for_kind(ThemeKind::LiquidGlass).card;
        assert_eq!(tint_opacity(card, card), Some(1.0));
        let half = tint_opacity(card.gamma_multiply(0.5), card).expect("a fading window");
        assert!((half - 0.5).abs() < 0.02);
        assert_eq!(tint_opacity(Color32::from_black_alpha(110), card), None, "a scrim");
        assert_eq!(tint_opacity(Color32::from_rgb(22, 24, 31), card), None, "opaque");
        assert_eq!(tint_opacity(card, Tokens::for_kind(ThemeKind::Studio).card), None, "opaque themes have no panes");
    }

    fn panes(shape: &Shape) -> usize {
        match shape {
            Shape::Vec(v) => v.iter().map(panes).sum(),
            Shape::Callback(_) => 1,
            _ => 0,
        }
    }

    #[test]
    fn only_outer_surface_rectangles_get_a_pane() {
        let t = Tokens::for_kind(ThemeKind::LiquidGlassLight);
        let surfaces = [t.card, t.chrome, t.dock, t.canvas];
        let rect = |r: Rect, fill| Shape::rect_filled(r, 8.0, fill);
        let outer = Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(300.0, 200.0));
        let mut shapes = vec![
            rect(Rect::EVERYTHING, t.scrim),
            Shape::Vec(vec![Shape::from(egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: t.shadow }.as_shape(outer, 8)), rect(outer, t.card)]),
            rect(outer.shrink(20.0), t.card),
            rect(outer.translate(egui::vec2(400.0, 0.0)), t.dock),
            rect(Rect::from_min_size(outer.min, egui::vec2(10.0, 10.0)), t.card),
            rect(outer.translate(egui::vec2(0.0, 300.0)), t.accent),
        ];
        let (mut glazed, mut slot) = (Vec::new(), 0);
        for s in &mut shapes {
            glaze(s, &surfaces, &mut glazed, &mut slot, 0);
        }
        // A body and a rim each for the dialog and the dock-coloured region beside it.
        assert_eq!(shapes.iter().map(panes).collect::<Vec<_>>(), [0, 2, 0, 2, 0, 0]);
        assert_eq!(slot, 2);
    }
}
