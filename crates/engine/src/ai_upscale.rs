//! AI Upscale (`filter.ai.upscale`): enlarges the document 2x or 4x with a local super-resolution
//! model (see `ai_cmds` for how models are chosen, installed and run). AI Denoise
//! (`filter.ai.denoise`) runs the same tiled job with a denoising model at 1x, on the active layer.
//!
//! The document is first resized the ordinary way, so everything in it is handled as Image Size
//! handles it: masks, type, shapes, effects, guides, the selection. Then every pixel layer's
//! colours are replaced, tile by tile, with the model's rendering of the original pixels; the
//! layer's own transparency (resized bicubically) is kept. Tiles are 128 px squares that overlap,
//! and each output tile goes straight into the enlarged layer, so memory stays bounded by one
//! tile whatever the image's size. One undo step.
//!
//! The model was trained on sRGB: pixels are converted from the document's profile and back with
//! the colour-management engine, at the document's own bit depth. RGB and grayscale documents
//! only; without a model the command resamples with Preserve Details.

use std::sync::Arc;

use photocraft_doc::{DocId, Document, LayerContent, LayerId};
use photocraft_geom::Rect;
use photocraft_ml::tensor::{Tile, tile_input, tile_output, tiles};
use photocraft_ml::{Model, Purpose, Tensor};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

pub const CMD: &str = "filter.ai.upscale";
pub const DENOISE_CMD: &str = "filter.ai.denoise";

/// Pixels of overlap each tile gives up on the sides it shares with a neighbour.
const MARGIN: usize = 8;

/// What a model's output is larger than its input by.
fn factor(model: &Model) -> usize {
    if model.purpose == Purpose::Upscale { 4 } else { 1 }
}

/// What an upscale was computed from: it is only applied to that exact document state.
#[derive(Clone, Debug, PartialEq)]
pub struct UpscaleKey {
    doc: DocId,
    revision: u64,
    scale: u32,
    model: &'static str,
    /// The one layer it works on (AI Denoise); `None`: every pixel layer.
    layer: Option<LayerId>,
}

/// An upscale in progress: hands out the model's inputs one tile at a time and takes its outputs.
/// The same object drives the model in this process (desktop, CLI) and in the shell's worker
/// (the browser), so the tiling is written once.
pub struct UpscaleJob {
    key: UpscaleKey,
    model: &'static Model,
    /// What the model's output is larger by ([`factor`]).
    up: usize,
    /// 1 for 4x, 2 for 2x (the model's 4x output is halved).
    shrink: usize,
    src: Arc<Document>,
    /// The document at its new size; model pixels are written into it as they arrive.
    out: Document,
    /// Every tile of every pixel layer: the layer, its content bounds, the tile within them.
    work: Vec<(LayerId, Rect, Tile)>,
    next: usize,
    to_srgb: Option<(Arc<photocraft_cms::Transform>, usize)>,
    from_srgb: Option<(Arc<photocraft_cms::Transform>, usize)>,
}

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: CMD.into(), msg: msg.into() }
}

/// `"scale"`: 2 or 4, as a number or (from the dialog's choice) a string.
fn scale_param(p: &Value) -> Result<u32> {
    let v = match p.get("scale") {
        None | Some(Value::Null) => Some(2),
        Some(v) => v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())),
    };
    v.filter(|s| matches!(s, 2 | 4)).map(|s| s as u32).ok_or_else(|| bad("\"scale\" must be 2 or 4"))
}

fn new_size(doc: &Document, scale: u32) -> Result<(u32, u32)> {
    if scale == 1 {
        return Ok((doc.size.width, doc.size.height));
    }
    let (w, h) = (doc.size.width.checked_mul(scale), doc.size.height.checked_mul(scale));
    let (Some(w), Some(h)) = (w, h) else { return Err(bad("the document is too large to enlarge")) };
    crate::image_cmds::check_resample_budget(CMD, doc, w, h, "")?;
    Ok((w, h))
}

impl UpscaleJob {
    fn new(doc: &Arc<Document>, revision: u64, scale: u32, model: &'static Model, only: Option<LayerId>) -> Result<Self> {
        use photocraft_cms::ColorSpace;
        if !matches!(crate::color_cmds::mode_space(doc.mode), ColorSpace::Rgb | ColorSpace::Gray) {
            return Err(EngineError::Other(if model.purpose == Purpose::Denoise {
                "AI Denoise works on RGB and grayscale documents. Convert the mode first, or use Filter › Noise › Reduce Noise.".into()
            } else {
                "AI Upscale works on RGB and grayscale documents. Convert the mode first, or use Image › Image Size.".into()
            }));
        }
        let (w, h) = new_size(doc, scale)?;
        let mut out = (**doc).clone();
        if scale != 1 {
            crate::image_cmds::resample_doc(&mut out, w, h, photocraft_algo::resample::Resample::Bicubic);
        }
        let mut work = Vec::new();
        for (_, _, layer) in doc.walk() {
            let LayerContent::Raster(surface) = &layer.content else { continue };
            if only.is_some_and(|id| id != layer.id) {
                continue;
            }
            let bounds = surface.content_bounds();
            if bounds.is_empty() {
                continue;
            }
            work.extend(tiles(bounds.width() as usize, bounds.height() as usize, model.side, MARGIN).into_iter().map(|t| (layer.id, bounds, t)));
        }
        let (to_srgb, from_srgb) = (crate::ai_cmds::srgb_transform(doc, false), crate::ai_cmds::srgb_transform(doc, true));
        let key = UpscaleKey { doc: doc.id, revision, scale, model: model.id, layer: only };
        let up = factor(model);
        let shrink = up.checked_div(scale as usize).filter(|k| *k > 0).ok_or_else(|| bad("the scale does not fit the model"))?;
        Ok(Self { key, model, up, shrink, src: doc.clone(), out, work, next: 0, to_srgb, from_srgb })
    }

    pub fn model(&self) -> &'static Model {
        self.model
    }

    /// How far along, `0.0..=1.0`.
    pub fn progress(&self) -> f32 {
        if self.work.is_empty() { 1.0 } else { self.next as f32 / self.work.len() as f32 }
    }

    pub fn is_done(&self) -> bool {
        self.next >= self.work.len()
    }

    /// The model's input for the next tile; `None` when every tile is done.
    pub fn next_input(&self) -> Option<Tensor> {
        let (id, bounds, tile) = self.work.get(self.next)?;
        let surface = self.src.layer(*id)?.surface()?;
        let side = self.model.side as i32;
        let (x, y) = (bounds.x0 + tile.x as i32, bounds.y0 + tile.y as i32);
        // The part of the tile inside the layer's pixels; beyond it the edge is repeated.
        let r = Rect::new(x, y, x + side, y + side).intersect(bounds);
        let (rw, rh) = (r.width() as usize, r.height() as usize);
        if rw == 0 || rh == 0 {
            return None;
        }
        let mut px = vec![[0.0f32; 4]; rw * rh];
        surface.read_rgba_into(r, &mut px);
        let mut rgb: Vec<[f32; 3]> = px.iter().map(|p| [p[0], p[1], p[2]]).collect();
        if let Some((t, n)) = &self.to_srgb {
            t.apply(rgb.as_flattened_mut(), *n);
        }
        let at = |x: usize, y: usize| rgb.get((y - tile.y).min(rh - 1) * rw + (x - tile.x).min(rw - 1)).copied().unwrap_or([0.0; 3]);
        Some(tile_input(at, tile, self.model.side))
    }

    /// The model's output for the tile [`next_input`](Self::next_input) gave: written into the
    /// enlarged layer, keeping that layer's own alpha.
    pub fn put(&mut self, output: &[f32]) -> Result<()> {
        let &(id, bounds, tile) = self.work.get(self.next).ok_or_else(|| EngineError::Other("the upscale has no tile waiting for a result".into()))?;
        let (w, h, mut px) = tile_output(output, self.model, self.up, self.shrink, &tile)?;
        if let Some((t, n)) = &self.from_srgb {
            t.apply(px.as_flattened_mut(), *n);
        }
        let k = (self.up / self.shrink) as i32;
        let (x, y) = ((bounds.x0 + (tile.x + tile.keep[0]) as i32) * k, (bounds.y0 + (tile.y + tile.keep[1]) as i32) * k);
        let dst = Rect::new(x, y, x + w as i32, y + h as i32);
        let surface = self.out.layer_mut(id).and_then(|l| l.surface_mut()).ok_or(EngineError::NoLayer(id))?;
        let n = surface.channels();
        let colour = n - usize::from(surface.format().alpha);
        let mut data = surface.read_region(dst);
        if data.len() != px.len() * n {
            return Err(EngineError::Other("the enlarged layer has an unexpected size".into()));
        }
        for (cell, p) in data.chunks_exact_mut(n).zip(&px) {
            match cell.get_mut(..colour) {
                Some([gray]) => *gray = (p[0] + p[1] + p[2]) / 3.0,
                Some([r, g, b]) => (*r, *g, *b) = (p[0], p[1], p[2]),
                _ => {}
            }
        }
        surface.write_region(dst, &data);
        self.next += 1;
        Ok(())
    }
}

fn key(s: &Session, scale: u32, model: &'static Model, layer: Option<LayerId>) -> Result<UpscaleKey> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    Ok(UpscaleKey { doc: d.doc.id, revision: d.revision, scale, model: model.id, layer })
}

/// For the shell: if the command with `p` would use a model that this process can't run, the job
/// to drive with its worker. `None`: the command needs nothing from the shell.
pub fn upscale_request(s: &Session, p: &Value) -> Result<Option<UpscaleJob>> {
    let scale = scale_param(p)?;
    let Some(model) = crate::ai_cmds::chosen_model(s, CMD, p, Purpose::Upscale)? else { return Ok(None) };
    if crate::ai_cmds::runs_here(s, model) {
        return Ok(None);
    }
    let d = s.active().ok_or(EngineError::NoDocument)?;
    UpscaleJob::new(&d.doc, d.revision, scale, model, None).map(Some)
}

/// For the shell: a finished job. The next matching command applies it.
pub fn upscale_ready(s: &mut Session, job: UpscaleJob) {
    s.ai.upscale = Some(job);
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn apply(s: &mut Session, doc: Document, model: &str) -> Result<Value> {
    let size = doc.size;
    replace(s, "AI Upscale", doc)?;
    Ok(json!({ "width": size.width, "height": size.height, "model": model }))
}

/// Swap in the document a job made, as one undo step.
fn replace(s: &mut Session, label: &str, doc: Document) -> Result<()> {
    s.edit(label, move |d, _| {
        *d = doc;
        Ok(())
    })
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let scale = scale_param(p)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (doc, revision) = (st.doc.clone(), st.revision);
    let (w, h) = new_size(&doc, scale)?;
    let classical = |s: &mut Session| {
        let mut out = (*doc).clone();
        crate::image_cmds::resample_doc(&mut out, w, h, photocraft_algo::resample::Resample::PreserveDetails);
        apply(s, out, "classical")
    };
    let Some(model) = crate::ai_cmds::chosen_model(s, CMD, p, Purpose::Upscale)? else { return classical(s) };
    let want = key(s, scale, model, None)?;
    // A job the shell finished is used once, and only for the document state it was made from.
    if let Some(job) = s.ai.upscale.take()
        && job.key == want
        && job.is_done()
    {
        return apply(s, job.out, model.id);
    }
    #[cfg(feature = "ml-runtime")]
    if let Some(local) = crate::ai_cmds::local(s, model) {
        return crate::jobs::run(
            s,
            "AI Upscale",
            true,
            move |ctx| {
                ctx.progress(0.01, "Preparing");
                let mut job = UpscaleJob::new(&doc, revision, scale, model, None)?;
                let rt = local.loaded(ctx)?;
                while let Some(input) = job.next_input() {
                    ctx.check()?;
                    let output = rt.run(model, input)?;
                    job.put(&output.data)?;
                    ctx.progress(0.02 + 0.97 * job.progress(), "Enlarging");
                }
                Ok(job.out)
            },
            move |s, out| apply(s, out, model.id),
        );
    }
    let _ = revision;
    // Installed where only the shell can run it, and the shell has not: "auto" resamples instead.
    if matches!(p.get("model").and_then(Value::as_str), None | Some("auto")) {
        return classical(s);
    }
    Err(EngineError::Other(format!("The {} model runs in the app's background worker and has no result for this image yet.", model.name)))
}

// ---- AI Denoise ------------------------------------------------------------------------------

fn denoise_bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: DENOISE_CMD.into(), msg: msg.into() }
}

/// The unlocked pixel layer AI Denoise works on: the active one.
fn denoise_layer(s: &Session) -> Result<LayerId> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let id = d.active_layer.ok_or_else(|| denoise_bad("no active layer"))?;
    let l = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    if !matches!(l.content, LayerContent::Raster(_)) {
        return Err(EngineError::Other(format!(
            "AI Denoise needs a pixel layer (the active layer is {} {} layer)",
            l.content.article(),
            l.content.kind_name()
        )));
    }
    let locks = d.doc.effective_locks(id);
    if locks.pixels || locks.all {
        return Err(EngineError::Other(format!("Could not complete your request because the layer \"{}\" is locked", l.name)));
    }
    Ok(id)
}

/// For the shell: like [`upscale_request`], for AI Denoise.
pub fn denoise_request(s: &Session, p: &Value) -> Result<Option<UpscaleJob>> {
    let Some(model) = crate::ai_cmds::chosen_model(s, DENOISE_CMD, p, Purpose::Denoise)? else { return Ok(None) };
    if crate::ai_cmds::runs_here(s, model) {
        return Ok(None);
    }
    let layer = denoise_layer(s)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    UpscaleJob::new(&d.doc, d.revision, 1, model, Some(layer)).map(Some)
}

fn denoise_enabled(s: &Session) -> std::result::Result<(), String> {
    denoise_layer(s).map(|_| ()).map_err(|e| e.to_string())
}

// shortcut: the whole layer is denoised and a selection is ignored; mask the result through the
// selection if people ask for it.
fn run_denoise(s: &mut Session, p: &Value) -> Result<Value> {
    let layer = denoise_layer(s)?;
    let classical = |s: &mut Session| crate::filters::run_filter(s, "filter.noise.reduceNoise", &json!({})).map(|_| json!({ "model": "classical" }));
    let Some(model) = crate::ai_cmds::chosen_model(s, DENOISE_CMD, p, Purpose::Denoise)? else { return classical(s) };
    let want = key(s, 1, model, Some(layer))?;
    if let Some(job) = s.ai.upscale.take()
        && job.key == want
        && job.is_done()
    {
        replace(s, "AI Denoise", job.out)?;
        return Ok(json!({ "model": model.id }));
    }
    #[cfg(feature = "ml-runtime")]
    if let Some(local) = crate::ai_cmds::local(s, model) {
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let (doc, revision) = (st.doc.clone(), st.revision);
        return crate::jobs::run(
            s,
            "AI Denoise",
            true,
            move |ctx| {
                ctx.progress(0.01, "Preparing");
                let mut job = UpscaleJob::new(&doc, revision, 1, model, Some(layer))?;
                let rt = local.loaded(ctx)?;
                while let Some(input) = job.next_input() {
                    ctx.check()?;
                    let output = rt.run(model, input)?;
                    job.put(&output.data)?;
                    ctx.progress(0.02 + 0.97 * job.progress(), "Removing noise");
                }
                Ok(job.out)
            },
            move |s, out| replace(s, "AI Denoise", out).map(|_| json!({ "model": model.id })),
        );
    }
    if matches!(p.get("model").and_then(Value::as_str), None | Some("auto")) {
        return classical(s);
    }
    Err(EngineError::Other(format!("The {} model runs in the app's background worker and has no result for this image yet.", model.name)))
}

/// AI Upscale and AI Denoise command specs.
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: CMD,
            label: "AI Upscale…",
            menu: &["Filter", "AI"],
            shortcut: None,
            params: r##"{"scale":"2|4","model":"auto|classical|realesrgan"} → {width,height,model} (enlarges the document; pixel layers are rendered by a local super-resolution model, tile by tile; "auto" uses an installed model, else Preserve Details resampling; RGB and grayscale documents)"##,
            enabled,
            run,
            journal: true,
        },
        CommandSpec {
            id: DENOISE_CMD,
            label: "AI Denoise…",
            menu: &["Filter", "AI"],
            shortcut: None,
            params: r##"{"model":"auto|classical|scunet"} → {model} (removes noise from the active pixel layer with a local denoising model, tile by tile; "auto" uses an installed model, else Filter › Noise › Reduce Noise with its defaults; RGB and grayscale documents)"##,
            enabled: denoise_enabled,
            run: run_denoise,
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests;
