//! The Remove Tool as a command: paint over something and it is replaced by its surroundings.
//!
//! The stroke's footprint is the hole. A stroke that closes around an object also removes what it
//! encloses (`closeLoops`), and the hole grows by [`GROW`] pixels so the object's soft edge and
//! halo go with it. The hole is completed from a window around the stroke by patch-based
//! completion with texture features (`photocraft_algo::remove`, `photocraft_algo::nonlocal`). The
//! selection, when there is one, limits what changes. Large holes take seconds, so the command
//! runs as a background job (#210): with progress, cancellable, and the document unchanged until
//! it applies, in one undo step.
//!
//! Several strokes can go in one removal (`strokes`): Photoshop's *Remove after each stroke*
//! off, where the strokes collect and are applied together, so the fill sees every hole at once
//! instead of sampling from an area the next stroke is about to remove. With `newLayer` the fill
//! lands on a new layer above the target, only where the hole is, and the target stays as it was
//! (Photoshop's *Create a new layer*; with Sample All Layers it fills from the composite).

use super::content_aware_move::{Target, replace};
use super::patch::coverage;
use super::*;
use photocraft_doc::Layer;
use photocraft_ml::{Model, Purpose, Tensor};

const CMD: &str = "paint.remove";

/// Largest stroke, in pixels of its bounding box (as the Patch Tool's and Content-Aware Move's).
/// Checked before the stroke's coverage is rendered, which allocates its whole bounding box, also
/// where it leaves the canvas.
const MAX_PIXELS: u64 = 16 << 20;

/// Cap on the sampling margin around the stroke (the window grows with the stroke).
const MAX_MARGIN: i32 = 512;

/// How far the hole grows beyond the stroke, in pixels.
const GROW: usize = 2;

/// Seed of the completion, so a removal is reproducible.
const SEED: u64 = 0x0052_e30e;

/// The sampling margin around the stroke: three quarters of its larger side, at least 32 pixels
/// (in u64, so a huge extent can't overflow; a margin beyond i32 saturates).
fn sampling_margin(area: Rect) -> i32 {
    let ext = u64::from(area.width().max(area.height()));
    i32::try_from(ext * 3 / 4).unwrap_or(i32::MAX).max(32)
}

/// What a removal does, checked before any work starts.
struct Plan {
    id: Option<LayerId>,
    /// One stroke (`points`), or several applied together (`strokes`).
    strokes: Vec<Stroke>,
    /// Where the fill samples from: the strokes plus a margin, on the canvas.
    window: Rect,
    close_loops: bool,
    all_layers: bool,
    /// The fill goes to a new layer above the target instead of into it.
    new_layer: bool,
}

/// The stroke's bounding box (points ± the brush radius), before any coverage is rendered.
fn stroke_bounds(stroke: &Stroke) -> Rect {
    let r = f64::from(stroke.brush.size) / 2.0 + 2.0;
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for q in &stroke.points {
        (x0, y0, x1, y1) = (x0.min(q.x), y0.min(q.y), x1.max(q.x), y1.max(q.y));
    }
    // `check_coords` keeps points within ±MAX_COORD and sizes are capped, so these fit an i32.
    Rect::new((x0 - r).floor() as i32, (y0 - r).floor() as i32, (x1 + r).ceil() as i32, (y1 + r).ceil() as i32)
}

/// The strokes of the request: `strokes` (several, each a list of points, sharing the brush
/// and the target) or the one in `points`.
fn parse_strokes(s: &Session, p: &Value) -> Result<(Vec<Stroke>, Option<LayerId>)> {
    let Some(list) = p.get("strokes") else {
        let (stroke, id) = parse_brush(s, p, CMD)?;
        reject_huge_stroke(CMD, &stroke)?;
        return Ok((vec![stroke], id));
    };
    let list = list.as_array().ok_or_else(|| bad(CMD, "`strokes` is a list of point lists"))?;
    if list.is_empty() {
        return Err(bad(CMD, "`strokes` is empty"));
    }
    let mut strokes = Vec::with_capacity(list.len());
    let mut id = None;
    for points in list {
        let mut one = p.clone();
        if let Some(o) = one.as_object_mut() {
            o.remove("strokes");
            o.insert("points".into(), points.clone());
        }
        let (stroke, layer) = parse_brush(s, &one, CMD)?;
        reject_huge_stroke(CMD, &stroke)?;
        strokes.push(stroke);
        id = layer;
    }
    Ok((strokes, id))
}

fn plan(s: &Session, p: &Value) -> Result<Plan> {
    let (strokes, id) = parse_strokes(s, p)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let canvas = d.doc.bounds();
    let bounds = strokes.iter().map(stroke_bounds).reduce(|a, b| a.union(&b)).ok_or_else(|| bad(CMD, "`strokes` is empty"))?;
    let pixels = u64::from(bounds.width()) * u64::from(bounds.height());
    if pixels > MAX_PIXELS {
        return Err(bad(CMD, format!("the stroke is too large to remove ({pixels} pixels, at most {MAX_PIXELS})")));
    }
    let area = bounds.intersect(&canvas);
    if area.is_empty() {
        return Err(bad(CMD, "the stroke is outside the canvas"));
    }
    let window = area.inflate(sampling_margin(area).min(MAX_MARGIN)).intersect(&canvas);
    let close_loops = flag(p, "closeLoops", true);
    let all_layers = flag(p, "sampleAllLayers", false) && targets_pixels(p);
    // A mask or channel has no "above": the fill goes where the stroke went.
    let new_layer = flag(p, "newLayer", false) && targets_pixels(p);
    Ok(Plan { id, strokes, window, close_loops, all_layers, new_layer })
}

/// The hole over the plan's window: the strokes' footprints, their enclosed areas and a margin,
/// limited to the selection. Empty when nothing would change.
fn hole(plan: &Plan, sel: Option<&Surface>) -> Result<Vec<bool>> {
    let win = plan.window;
    let (w, h) = (win.width() as usize, win.height() as usize);
    let mut m = vec![false; w * h];
    for stroke in &plan.strokes {
        let (bounds, cov) = super::coverage(CMD, stroke)?;
        let bw = bounds.width() as usize;
        for (i, c) in cov.iter().enumerate() {
            let (x, y) = (bounds.x0 + (i % bw.max(1)) as i32, bounds.y0 + (i / bw.max(1)) as i32);
            if *c > 0.0 && win.contains(x, y) {
                let j = (y - win.y0) as usize * w + (x - win.x0) as usize;
                if let Some(v) = m.get_mut(j) {
                    *v = true;
                }
            }
        }
    }
    if plan.close_loops {
        m = photocraft_algo::remove::close_loops(w, h, &m);
    }
    m = photocraft_algo::remove::dilate(w, h, &m, GROW);
    if let Some(sel) = sel {
        for (v, c) in m.iter_mut().zip(coverage(sel, win)) {
            *v &= c > 0.0;
        }
    }
    Ok(m)
}

/// How a removal's hole gets filled.
pub(super) enum Fill {
    /// Patch completion from the surroundings (no model chosen or installed).
    Classical,
    /// An inpainting model's output for this removal, computed by the shell's worker.
    Output(&'static Model, Vec<f32>),
    /// Run the inpainting model here.
    #[cfg(feature = "ml-runtime")]
    Local(&'static Model, crate::ai_cmds::Local),
}

/// The plan's window as the removal reads it (the layer, or every layer merged) and its format.
fn read_window(doc: &mut Document, plan: &Plan, p: &Value) -> Result<(Region, PixelFormat)> {
    let (surf, _) = crate::channel_cmds::target_surface(doc, plan.id, p)?;
    let fmt = surf.format();
    let img = if plan.all_layers { composite_region(doc, None, SampleLayers::All, plan.window, fmt) } else { Region::read(surf, plan.window) };
    Ok((img, fmt))
}

/// The inpainting model this removal uses, if any: `"model"` is `auto` (an installed one, the
/// default), `classical`, or a model id. Models paint opaque RGB: other colour modes, and a
/// window with transparency in it (where the classical fill completes alpha too), use the
/// classical fill.
fn model_for(s: &Session, plan: &Plan, p: &Value) -> Result<Option<&'static Model>> {
    use photocraft_cms::ColorSpace;
    let Some(model) = crate::ai_cmds::chosen_model(s, CMD, p, Purpose::Inpaint)? else { return Ok(None) };
    let d = s.active().ok_or(EngineError::NoDocument)?;
    if !matches!(crate::color_cmds::mode_space(d.doc.mode), ColorSpace::Rgb | ColorSpace::Gray) {
        return Ok(None);
    }
    // A copy to read from (documents are copy-on-write, so this is cheap).
    let (img, fmt) = read_window(&mut (*d.doc).clone(), plan, p)?;
    let opaque = alpha_index(&fmt).is_none_or(|a| img.data.chunks_exact(img.ch).all(|px| px.get(a).is_some_and(|v| *v >= 1.0)));
    Ok(opaque.then_some(model))
}

/// What a removal works on: the hole over the plan's window, the window's pixels and their format.
struct Prepared {
    hole: Vec<bool>,
    img: Region,
    fmt: PixelFormat,
}

/// `None` when the layer's own window is fully transparent: nothing to remove and nothing to fill
/// from (a no-op without a history step, as the other retouch tools on an empty layer).
fn prepare(doc: &mut Document, plan: &Plan, p: &Value) -> Result<Option<Prepared>> {
    let hole = hole(plan, doc.selection.as_ref())?;
    if !hole.iter().any(|m| *m) {
        return Err(bad(CMD, "the stroke doesn't cover anything that can change (check the selection)"));
    }
    if hole.iter().all(|m| *m) {
        return Err(bad(CMD, "nothing is left around the stroke to fill it from"));
    }
    let (img, fmt) = read_window(doc, plan, p)?;
    if !plan.all_layers && alpha_index(&fmt).is_some_and(|a| img.data.chunks_exact(img.ch).all(|px| px.get(a).is_none_or(|v| *v <= 0.0))) {
        return Ok(None);
    }
    Ok(Some(Prepared { hole, img, fmt }))
}

/// The inpainting model's input: the window as sRGB with its hole.
fn model_tensor(doc: &Document, w: &Prepared, model: &'static Model) -> Result<Tensor> {
    let colour = w.img.ch - usize::from(alpha_index(&w.fmt).is_some());
    let mut rgb: Vec<[f32; 3]> = w.img.data.chunks_exact(w.img.ch).map(|px| if colour >= 3 { [px[0], px[1], px[2]] } else { [px[0]; 3] }).collect();
    if let Some((t, n)) = crate::ai_cmds::srgb_transform(doc, false) {
        t.apply(rgb.as_flattened_mut(), n);
    }
    let r = w.img.rect;
    Ok(photocraft_ml::tensor::inpaint_input(&rgb, &w.hole, r.width() as usize, r.height() as usize, model)?)
}

/// The window with its hole painted from the model's output; everything else is untouched.
fn model_fill(doc: &Document, w: &Prepared, model: &'static Model, output: &[f32]) -> Result<Vec<f32>> {
    let r = w.img.rect;
    let mut px = photocraft_ml::tensor::inpaint_output(output, model, r.width() as usize, r.height() as usize)?;
    if let Some((t, n)) = crate::ai_cmds::srgb_transform(doc, true) {
        t.apply(px.as_flattened_mut(), n);
    }
    let alpha = alpha_index(&w.fmt);
    let colour = w.img.ch - usize::from(alpha.is_some());
    let mut data = w.img.data.clone();
    for ((cell, p), hole) in data.chunks_exact_mut(w.img.ch).zip(&px).zip(&w.hole) {
        if !*hole {
            continue;
        }
        match cell.get_mut(..colour) {
            Some([gray]) => *gray = (p[0] + p[1] + p[2]) / 3.0,
            Some([r, g, b]) => (*r, *g, *b) = (p[0], p[1], p[2]),
            _ => {}
        }
        if let Some(a) = alpha.and_then(|a| cell.get_mut(a)) {
            *a = 1.0;
        }
    }
    Ok(data)
}

/// For the shell (through `ai_cmds::inpaint_request`): the model and its input when this removal
/// would use a model that this process can't run.
pub(crate) fn remove_model_input(s: &Session, p: &Value) -> Result<Option<(&'static Model, Tensor)>> {
    let plan = plan(s, p)?;
    let Some(model) = model_for(s, &plan, p)? else { return Ok(None) };
    if crate::ai_cmds::runs_here(s, model) {
        return Ok(None);
    }
    // A copy to read from (documents are copy-on-write, so this is cheap).
    let mut doc = (*s.active().ok_or(EngineError::NoDocument)?.doc).clone();
    let Some(prepared) = prepare(&mut doc, &plan, p)? else { return Ok(None) };
    Ok(Some((model, model_tensor(&doc, &prepared, model)?)))
}

/// The filled window and the hole it fills, computed on `doc` (a snapshot). `None` when the
/// layer's own window is fully transparent: nothing to remove and nothing to fill from.
fn run_remove(doc: &mut Document, plan: &Plan, p: &Value, ctx: &crate::jobs::JobCtx, label: &str, fill: Fill) -> Result<Option<(Region, Vec<bool>)>> {
    let Some(prepared) = prepare(doc, plan, p)? else { return Ok(None) };
    ctx.check()?;
    let win = plan.window;
    let (w, h) = (win.width() as usize, win.height() as usize);
    let mut data = match fill {
        Fill::Classical => ctx
            .stage(0.0, 1.0, label, |ctl| photocraft_algo::remove::remove_with(w, h, prepared.img.ch, &prepared.img.data, &prepared.hole, SEED, ctl))
            .map_err(|_| EngineError::Cancelled)?,
        Fill::Output(model, output) => model_fill(doc, &prepared, model, &output)?,
        #[cfg(feature = "ml-runtime")]
        Fill::Local(model, local) => {
            let input = model_tensor(doc, &prepared, model)?;
            let rt = local.loaded(ctx)?;
            ctx.progress(0.3, &format!("Filling with {}", model.name));
            let output = rt.run(model, input)?;
            drop(rt);
            ctx.check()?;
            model_fill(doc, &prepared, model, &output.data)?
        }
    };
    // The fill can step slightly past the storable range.
    clamp_samples(&prepared.fmt, &mut data);
    Ok(Some((Region { rect: win, ch: prepared.img.ch, data }, prepared.hole)))
}

/// The fill on a new layer above `id`: the hole's pixels (opaque where the source had no alpha),
/// transparent elsewhere. Returns the damage and the new layer.
fn fill_to_new_layer(doc: &mut Document, id: Option<LayerId>, out: &Region, hole: &[bool]) -> Result<(Rect, LayerId)> {
    let base = doc.pixel_format();
    let fmt = PixelFormat::new(base.mode, base.sample, true);
    let n = fmt.channels();
    let colours = n.saturating_sub(1);
    if out.ch != n && out.ch != colours {
        return Err(EngineError::Other("internal error: the fill's channels don't match the document".into()));
    }
    let mut data = vec![0.0f32; hole.len().saturating_mul(n)];
    for ((dst, src), m) in data.chunks_exact_mut(n).zip(out.data.chunks_exact(out.ch)).zip(hole) {
        if !*m {
            continue;
        }
        for (d, s) in dst.iter_mut().zip(src.iter().take(colours)) {
            *d = *s;
        }
        if let Some(a) = dst.last_mut() {
            *a = if out.ch == n { src.last().copied().unwrap_or(1.0) } else { 1.0 };
        }
    }
    let mut l = Layer::raster(doc.next_layer_name("Layer"), fmt);
    crate::pixels_mut(&mut l)?.write_region(out.rect, &data);
    let nid = doc.insert_above(id, l);
    Ok((out.rect, nid))
}

pub(super) fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let plan = plan(s, p)?;
    let label = "Remove";
    // A model's output the shell computed, the model run here, or the classical fill. Asked for by
    // name, a model that can't run is an error; "auto" falls back.
    let (fill, model) = match model_for(s, &plan, p)? {
        None => (Fill::Classical, "classical"),
        Some(model) => {
            let auto = matches!(p.get("model").and_then(Value::as_str), None | Some("auto"));
            if let Some(output) = crate::ai_cmds::take_inpaint(s, p) {
                (Fill::Output(model, output), model.id)
            } else if let Some(fill) = local_fill(s, model) {
                (fill, model.id)
            } else if auto {
                (Fill::Classical, "classical")
            } else {
                return Err(EngineError::Other(format!("The {} model runs in the app's background worker and has no result for this stroke yet.", model.name)));
            }
        }
    };
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let doc = d.doc.clone();
    let p = p.clone();
    let id = plan.id;
    let new_layer = plan.new_layer;
    crate::jobs::run(
        s,
        label,
        true,
        {
            let p = p.clone();
            move |ctx| {
                ctx.progress(0.0, label);
                let mut work = (*doc).clone();
                run_remove(&mut work, &plan, &p, ctx, label, fill)
            }
        },
        move |s, filled| {
            let Some((out, hole)) = filled else { return Ok(json!({"changed": false})) };
            if new_layer {
                let (damage, nid) = s.edit(label, |doc, active| {
                    let r = fill_to_new_layer(doc, id, &out, &hole)?;
                    *active = Some(r.1);
                    Ok(r)
                })?;
                if let Some(st) = s.active_mut() {
                    st.last_damage = Some(damage);
                }
                return Ok(json!({ "damage": damage_json(damage), "layer": nid.0 }));
            }
            let damage = s.edit(label, |doc, _| replace(doc, Target { id, all_layers: false }, &p, &out, &hole))?;
            if let Some(st) = s.active_mut() {
                st.last_damage = Some(damage);
            }
            Ok(json!({ "damage": damage_json(damage), "model": model }))
        },
    )
}

#[cfg(feature = "ml-runtime")]
fn local_fill(s: &Session, model: &'static Model) -> Option<Fill> {
    crate::ai_cmds::local(s, model).map(|local| Fill::Local(model, local))
}

#[cfg(not(feature = "ml-runtime"))]
fn local_fill(_: &Session, _: &'static Model) -> Option<Fill> {
    None
}
