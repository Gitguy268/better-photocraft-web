//! Remove Background, Photoshop's Quick Action for a pixel layer (Properties › Quick Actions):
//! the subject is found, its edge refined, and the background hidden, in one history step.
//!
//! The subject comes from a local AI model when one is installed (`"model"`, see `ai_cmds`), and
//! from the classical Select Subject otherwise. By default the result is non-destructive, as in
//! Photoshop: a layer mask (the pixels stay), with the Background turned into a normal layer
//! first. `"output"` can instead make a new layer, or cut the layer's own pixels out.

use photocraft_algo::matting::{self, RefineParams};
use photocraft_doc::{Document, Layer, LayerContent, LayerMask};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::ai_cmds::{self, Subject};
use crate::commands::{CommandSpec, layer_param};
use crate::smartselect_cmds::with_doc_sampler;
use crate::{EngineError, Result, Session};

const CMD: &str = "layer.removeBackground";

/// Edge refinement after the classical Select Subject: a narrow smart radius and a little
/// smoothing, so soft edges and hair get partial coverage instead of a hard cut.
const REFINE: RefineParams = RefineParams { radius: 2.0, smart_radius: true, smooth: 10.0, feather: 0.5, contrast: 10.0, shift_edge: 0.0 };

/// Where the result goes.
#[derive(Clone, Copy, PartialEq)]
enum Output {
    /// A layer mask on the layer (default; non-destructive).
    Mask,
    /// A new layer holding only the subject; the source layer is hidden.
    NewLayer,
    /// A new layer with all the pixels and a mask; the source layer is hidden.
    NewLayerWithMask,
    /// The layer's own background pixels become transparent.
    Cutout,
}

/// Remove Background needs an unlocked pixel layer.
fn check(doc: &Document, l: &Layer) -> std::result::Result<(), String> {
    if !matches!(l.content, LayerContent::Raster(_)) {
        return Err(format!("the layer is {} {} layer, not a pixel layer", l.content.article(), l.content.kind_name()));
    }
    let locks = doc.effective_locks(l.id);
    if locks.pixels || locks.all {
        return Err(format!("Could not complete your request because the layer \"{}\" is locked", l.name));
    }
    Ok(())
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    check(&d.doc, crate::active_layer_of(s)?)
}

fn bad(msg: &str) -> EngineError {
    EngineError::BadParams { cmd: CMD.into(), msg: msg.into() }
}

/// Where the subject is looked for: the layer's own pixels within the canvas. A photo that
/// doesn't fill the canvas (placed, or the canvas grown) would otherwise stand out from the
/// empty canvas around it as the subject, and the mask would reveal its whole rectangle, which a
/// Stroke then outlines (#8235). The whole canvas when sampling all layers.
pub(crate) fn subject_area(l: &Layer, canvas: Rect, sample_all: bool) -> Rect {
    let Some(s) = l.surface().filter(|_| !sample_all) else { return canvas };
    let empty_around = s.format().alpha && s.default_pixel().last().is_some_and(|a| *a <= 0.0);
    let b = s.content_bounds().intersect(&canvas);
    if empty_around && !b.is_empty() { b } else { canvas }
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let id = layer_param(s, p)?;
    let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
    check(doc, doc.layer(id).ok_or(EngineError::NoLayer(id))?).map_err(EngineError::Other)?;
    let flag = |k: &str, default: bool| p.get(k).and_then(Value::as_bool).unwrap_or(default);
    let num = |k: &str, default: f32, max: f32| p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).map_or(default, |v| (v as f32).clamp(0.0, max));
    let (refine, decontaminate) = (flag("refine", true), flag("decontaminate", false));
    let (feather, amount) = (num("feather", 0.0, 250.0), num("amount", 100.0, 100.0));
    let mut output = match p.get("output").map(|v| v.as_str()) {
        None | Some(Some("mask")) => Output::Mask,
        Some(Some("newLayer")) => Output::NewLayer,
        Some(Some("newLayerWithMask")) => Output::NewLayerWithMask,
        Some(Some("cutout")) => Output::Cutout,
        _ => return Err(bad("output must be mask|newLayer|newLayerWithMask|cutout")),
    };
    // Decontaminated colours need their own pixels, so a mask alone becomes a new layer with a
    // mask (as Select and Mask does).
    if decontaminate && output == Output::Mask {
        output = Output::NewLayerWithMask;
    }
    let (ask, plan) = ai_cmds::subject_plan(s, CMD, p)?;
    let model = match &plan {
        Subject::Classical => "classical",
        Subject::Output(m, _) => m.id,
        #[cfg(feature = "ml-runtime")]
        Subject::Local(m, ..) => m.id,
    };
    // A model mask is made at the model's resolution: its edge is this many pixels wide here.
    let edge = ai_cmds::mask_edge_px(&plan, ask.area);
    crate::jobs::edit_job(
        s,
        "Remove Background",
        move |doc, active, ctx| {
            ctx.progress(0.1, "Finding subject");
            let mut region = ai_cmds::find_subject(doc, &ask, false, plan, ctx)?.ok_or_else(|| EngineError::Other("no subject found".into()))?;
            ctx.check()?;
            let params = RefineParams {
                radius: if refine { REFINE.radius.max(edge * 2.0).min(64.0) } else { 0.0 },
                smart_radius: refine,
                smooth: if refine { REFINE.smooth } else { 0.0 },
                feather: if refine { REFINE.feather } else { 0.0 } + feather,
                contrast: if refine { REFINE.contrast } else { 0.0 },
                shift_edge: 0.0,
            };
            if refine || feather > 0.0 {
                ctx.progress(0.6, "Refining edge");
                let refined = with_doc_sampler(doc, Some(id), ask.sample_all, |smp, _| {
                    matting::refine_mask(smp, &matting::region_reader(&region), region.bbox, ask.area, &params)
                });
                region = refined.unwrap_or(region);
                ctx.check()?;
            }
            let b = region.bbox;
            let bounds = [b.x0, b.y0, b.width() as i32, b.height() as i32];
            if output == Output::Mask {
                crate::extra_cmds::background_to_layer_for_mask(doc, id);
                let mask = crate::commands::mask_at_depth(LayerMask { surface: matting::region_surface(&region), ..LayerMask::reveal_all() }, doc.depth)?;
                doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.mask = Some(mask);
                doc.selection = None;
                return Ok((id, bounds));
            }
            let src_layer = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
            let src = src_layer.surface().ok_or_else(|| EngineError::Other("the layer has no pixels".into()))?;
            ctx.progress(0.85, "Cutting out");
            let pixels = if decontaminate { matting::decontaminate(src, &region, params.radius.max(2.0), amount) } else { src.clone() };
            let target = if output == Output::Cutout {
                crate::extra_cmds::background_to_layer_for_mask(doc, id);
                let layer = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
                layer.content = LayerContent::Raster(matting::masked_copy(&pixels, &region));
                id
            } else {
                let mut layer = Layer::new(doc.copy_name(&src_layer.name), LayerContent::Raster(pixels));
                if output == Output::NewLayerWithMask {
                    layer.mask =
                        Some(crate::commands::mask_at_depth(LayerMask { surface: matting::region_surface(&region), ..LayerMask::reveal_all() }, doc.depth)?);
                } else if let LayerContent::Raster(px) = &mut layer.content {
                    *px = matting::masked_copy(px, &region);
                }
                let new = doc.insert_above(Some(id), layer);
                if let Some(l) = doc.layer_mut(id) {
                    l.visible = false;
                }
                *active = Some(new);
                new
            };
            doc.selection = None;
            Ok((target, bounds))
        },
        move |(layer, bounds)| json!({ "layer": layer.0, "bounds": bounds, "model": model }),
    )
}

/// Remove Background command spec. Photoshop has it only as a Properties Quick Action; here it is
/// also in the Layer menu, so it can be found and given a shortcut.
pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "Remove Background",
        menu: &["Layer"],
        shortcut: None,
        params: r##"{"layer":id?,"model":"auto|classical|<id from ai.models>"="auto","output":"mask|newLayer|newLayerWithMask|cutout"="mask","refine":bool=true,"feather":px=0,"decontaminate":bool=false,"amount":0..100=100,"sampleAllLayers":bool=false} → {layer,bounds,model} (hides the background behind the subject: a layer mask by default, the Background becoming a normal layer; "auto" uses an installed local AI model, else the classical Select Subject)"##,
        enabled,
        run,
        journal: true,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_algo::segment::Rng;
    use photocraft_doc::LayerId;

    fn session_with(w: u32, h: u32, depth: u32, px: impl Fn(u32, u32) -> [f32; 3]) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": w, "height": h, "depth": depth})).unwrap();
        s.edit("paint", |doc, _| {
            let bg = doc.layers[0].surface_mut().unwrap();
            let n = bg.channels();
            let mut data = Vec::with_capacity((w * h) as usize * n);
            for y in 0..h {
                for x in 0..w {
                    let c = px(x, y);
                    data.extend_from_slice(&c[..n.min(3)]);
                    if n == 4 {
                        data.push(1.0);
                    }
                }
            }
            bg.write_region(Rect::new(0, 0, w as i32, h as i32), &data);
            Ok(())
        })
        .unwrap();
        s
    }

    /// Textured disc (centre (100, 80), radius 45) on a noisy background.
    fn disc(depth: u32) -> (Session, impl Fn(i32, i32) -> bool) {
        let mut rng = Rng::new(2);
        let nz: Vec<f32> = (0..200 * 160).map(|_| rng.normal()).collect();
        let inside = |x: i32, y: i32| {
            let (dx, dy) = (x as f32 + 0.5 - 100.0, y as f32 + 0.5 - 80.0);
            dx * dx + dy * dy <= 45.0 * 45.0
        };
        let s = session_with(200, 160, depth, move |x, y| {
            let n = 0.05 * nz[(y * 200 + x) as usize];
            if inside(x as i32, y as i32) {
                let t = ((x + y) as f32 * 0.35).sin() * 0.5 + 0.5;
                [0.85 + 0.05 * t + n, 0.2 + 0.3 * t + n, 0.15 + n]
            } else {
                [0.25 + n, 0.45 + n, 0.7 + n]
            }
            .map(|v| v.clamp(0.0, 1.0))
        });
        (s, inside)
    }

    fn active_id(s: &Session) -> LayerId {
        s.active().unwrap().active_layer.unwrap()
    }

    fn layer(s: &Session, id: LayerId) -> &Layer {
        s.active().unwrap().doc.layer(id).unwrap()
    }

    fn mask_cov(s: &Session, x: i32, y: i32) -> f32 {
        layer(s, active_id(s)).mask.as_ref().map_or(0.0, |m| m.value(x, y))
    }

    fn mask_iou(s: &Session, w: i32, h: i32, truth: impl Fn(i32, i32) -> bool) -> f32 {
        let (mut i, mut u) = (0, 0);
        for y in 0..h {
            for x in 0..w {
                let (a, b) = (mask_cov(s, x, y) >= 0.5, truth(x, y));
                i += (a && b) as i32;
                u += (a || b) as i32;
            }
        }
        i as f32 / u.max(1) as f32
    }

    #[test]
    fn remove_background_masks_the_subject_at_every_depth_and_undoes() {
        for depth in [8, 16, 32] {
            let (mut s, inside) = disc(depth);
            assert!(s.is_enabled(CMD));
            let id = active_id(&s);
            let pixels = layer(&s, id).surface().unwrap().read_region(Rect::new(0, 0, 200, 160));
            let steps = s.active().unwrap().history.past_len();
            let r = s.execute(CMD, json!({})).unwrap();
            assert_eq!(r["layer"], id.0);
            assert_eq!(s.active().unwrap().history.past_len(), steps + 1, "depth {depth}: one history step");
            assert!(layer(&s, id).mask.is_some(), "depth {depth}: expected a layer mask");
            let v = mask_iou(&s, 200, 160, &inside);
            assert!(v >= 0.75, "depth {depth}: IoU {v}");
            assert!(mask_cov(&s, 0, 0) < 0.2, "depth {depth}: background corner still revealed");
            assert!(mask_cov(&s, 100, 80) >= 0.8, "depth {depth}: disc centre hidden");
            // Non-destructive: the pixels are untouched, only masked.
            assert_eq!(layer(&s, id).surface().unwrap().read_region(Rect::new(0, 0, 200, 160)), pixels, "depth {depth}");
            let corner = photocraft_compose::render(&s.active().unwrap().doc, Rect::new(0, 0, 1, 1)).px[0][3];
            assert!(corner < 0.2, "depth {depth}: composite corner alpha {corner}");
            assert!(s.active().unwrap().doc.selection.is_none());
            s.execute("edit.undo", json!({})).unwrap();
            assert!(layer(&s, id).mask.is_none(), "depth {depth}: mask remained after undo");
            s.execute("edit.redo", json!({})).unwrap();
            assert!(layer(&s, id).mask.is_some(), "depth {depth}: mask missing after redo");
        }
    }

    #[test]
    fn remove_background_turns_the_background_into_a_layer() {
        let (mut s, _) = disc(8);
        let id = active_id(&s);
        assert_eq!(layer(&s, id).name, "Background");
        s.execute(CMD, json!({})).unwrap();
        let l = layer(&s, id);
        assert_eq!(l.name, "Layer 0");
        assert!(!l.locks.transparency && !l.locks.position);
        assert!(l.mask.is_some());
    }

    #[test]
    fn remove_background_cancelled_while_finding_the_subject_changes_nothing() {
        let (mut s, _) = disc(8);
        let id = active_id(&s);
        let before = s.active().unwrap().revision;
        // The job's cancel reaches Select Subject, which stops at its first check.
        let ctx = crate::jobs::JobCtx::new();
        ctx.cancel();
        s.set_inline_job_ctx(Some(ctx.clone()));
        assert!(matches!(s.execute(CMD, json!({})), Err(EngineError::Cancelled)));
        assert_eq!(ctx.message(), "Finding subject");
        assert_eq!(s.active().unwrap().revision, before);
        assert!(layer(&s, id).mask.is_none());
        s.set_inline_job_ctx(None);
        s.execute(CMD, json!({})).unwrap();
        assert!(layer(&s, id).mask.is_some());
    }

    #[test]
    fn remove_background_without_a_subject_leaves_the_document() {
        let mut s = session_with(32, 32, 8, |_, _| [0.5, 0.5, 0.5]);
        let before = s.active().unwrap().revision;
        let id = active_id(&s);
        assert!(s.execute(CMD, json!({})).is_err());
        assert_eq!(s.active().unwrap().revision, before);
        assert!(layer(&s, id).mask.is_none());
    }

    #[test]
    fn remove_background_disabled_states_and_bad_params() {
        assert!(!Session::new().is_enabled(CMD));
        assert!(Session::new().execute(CMD, json!({})).is_err());

        let (mut s, _) = disc(8);
        s.execute("type.create", json!({"text": "Hi", "size": 24, "x": 10, "y": 20})).unwrap();
        assert!(!s.is_enabled(CMD));
        assert!(s.execute(CMD, json!({})).is_err());

        let (mut s, _) = disc(8);
        s.execute("shape.create", json!({"kind": "rect", "rect": [10, 10, 40, 20], "fill": "#ff0000"})).unwrap();
        assert!(!s.is_enabled(CMD));
        assert!(s.execute(CMD, json!({})).is_err());

        let (mut s, _) = disc(8);
        s.execute("layer.setProps", json!({"locks": {"pixels": true}})).unwrap();
        assert!(!s.is_enabled(CMD));
        assert!(s.execute(CMD, json!({})).is_err());

        let (mut s, _) = disc(8);
        let before = s.active().unwrap().revision;
        assert!(s.execute(CMD, json!({"layer": u64::MAX})).is_err());
        // A `layer` string names a layer; one that isn't there is an error, not the active layer.
        assert!(s.execute(CMD, json!({"layer": "x"})).is_err());
        assert_eq!(s.active().unwrap().revision, before);
        // Wrong types for the options fall back to the defaults (layer-only sampling, refined).
        assert!(s.execute(CMD, json!({"sampleAllLayers": 3, "refine": "no"})).is_ok());
        assert!(s.active().unwrap().revision > before);
    }

    #[test]
    fn remove_background_looks_at_the_layer_alone_by_default() {
        let (mut s, _) = disc(8);
        let bottom = active_id(&s);
        s.execute("layer.new.layer", json!({"name": "Empty"})).unwrap();
        let top = active_id(&s);
        let before = s.active().unwrap().revision;
        // The empty layer has no subject of its own, though the photo shows through it.
        assert!(s.execute(CMD, json!({})).is_err());
        assert_eq!(s.active().unwrap().revision, before);
        assert!(layer(&s, top).mask.is_none() && layer(&s, bottom).mask.is_none());
        // Sampling all layers finds the subject in the composite.
        s.execute(CMD, json!({"sampleAllLayers": true})).unwrap();
        assert!(layer(&s, top).mask.is_some() && layer(&s, bottom).mask.is_none());
    }

    #[test]
    fn remove_background_twice_replaces_the_mask() {
        let (mut s, _) = disc(8);
        s.execute(CMD, json!({})).unwrap();
        s.execute(CMD, json!({"refine": false})).unwrap();
        let masks = s.active().unwrap().doc.walk().into_iter().filter(|(_, _, l)| l.mask.is_some()).count();
        assert_eq!(masks, 1);
    }

    #[test]
    fn remove_background_on_a_layer_smaller_than_the_canvas_masks_the_subject_not_the_layer() {
        // The photo doesn't fill the canvas (a placed photo, or the canvas grown): the empty
        // canvas around it is not the background, and a Stroke must not outline the photo's
        // rectangle (#8235).
        for depth in [8, 16, 32] {
            let (mut s, inside) = disc(depth);
            s.execute("layer.new.layerFromBackground", json!({})).unwrap();
            s.execute("image.canvasSize", json!({"width": 260, "height": 220, "anchor": "center"})).unwrap();
            let id = active_id(&s);
            s.execute(CMD, json!({})).unwrap();
            // The photo now sits at (30, 30).
            let v = mask_iou(&s, 260, 220, |x, y| inside(x - 30, y - 30));
            assert!(v >= 0.75, "depth {depth}: IoU {v}");
            for (x, y) in [(31, 31), (228, 31), (31, 188), (228, 188), (40, 110)] {
                assert!(mask_cov(&s, x, y) < 0.2, "depth {depth}: photo background at ({x}, {y}) revealed");
            }
            s.edit("stroke", |doc, _| {
                let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
                l.effects.items = vec![photocraft_doc::Effect::Stroke(photocraft_doc::StrokeFx {
                    common: photocraft_doc::FxCommon::new(photocraft_color::BlendMode::Normal, 1.0),
                    size: 3.0,
                    position: photocraft_doc::StrokePosition::Outside,
                    paint: photocraft_doc::FxPaint::Color(photocraft_color::Color::rgb(0.0, 0.0, 1.0)),
                    overprint: false,
                })];
                Ok(())
            })
            .unwrap();
            let img = photocraft_compose::render(&s.active().unwrap().doc, Rect::new(0, 0, 260, 220));
            // Just outside the photo's left edge, and just inside it: no stroke.
            for x in [27, 28, 29, 30, 31] {
                let a = img.px[110 * 260 + x][3];
                assert!(a < 0.05, "depth {depth}: stroke along the layer's edge at x = {x} (alpha {a})");
            }
        }
    }
}
