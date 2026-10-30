//! Local AI: the model commands (`ai.*`) and what the model-backed commands share.
//!
//! Models run on the user's device and are optional: every command that can use one
//! (`layer.removeBackground`, `select.subject`, `select.object`) keeps its classical algorithm for
//! when none is installed. The models are listed in `photocraft_ml::MODELS` and `docs/ai-models.md`;
//! they are downloaded by the app shell, never bundled.
//!
//! Where a model runs depends on the build (`docs/decisions.md`):
//! - Desktop and CLI (cargo feature `ml-runtime`): in this process, inside the command's job, from
//!   `<model_dir>/<id>.onnx`.
//! - Browser: in the shell's worker. The shell asks for the model's input with
//!   [`subject_request`], runs it, hands the output back with [`subject_ready`] and then runs the
//!   command, which finds the output waiting. The main wasm links no runtime.

use std::collections::BTreeSet;

use photocraft_algo::segment::{self, Region};
use photocraft_doc::{DocId, Document, LayerId};
use photocraft_geom::Rect;
use photocraft_ml::{MODELS, Model, Purpose, Tensor};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, layer_param};
use crate::jobs::JobCtx;
use crate::smartselect_cmds::with_doc_sampler;
use crate::{EngineError, Result, Session};

/// The largest area a model mask is made for (it is one byte per pixel at full resolution).
const MAX_MASK_PX: u64 = 400 << 20;

/// The session's AI state.
#[derive(Default)]
pub struct AiState {
    /// Folder that holds installed models as `<id>.onnx` (desktop and CLI; `None` elsewhere).
    pub model_dir: Option<std::path::PathBuf>,
    /// Models the shell keeps where the engine can't reach them (the browser's IndexedDB).
    external: BTreeSet<&'static str>,
    /// A model output computed outside the engine, waiting for the command it was made for.
    ready: Option<(SubjectKey, Vec<f32>)>,
    /// An inpainting result the shell's worker computed, waiting for the Remove tool's command.
    inpaint: Option<(InpaintKey, Vec<f32>)>,
    /// An upscale the shell's worker finished, waiting for its command (see `ai_upscale`).
    pub(crate) upscale: Option<crate::ai_upscale::UpscaleJob>,
    #[cfg(feature = "ml-runtime")]
    runtime: std::sync::Arc<std::sync::Mutex<photocraft_ml::runtime::Runtime>>,
}

/// What a subject mask was computed from. An output is only used for the exact request it
/// answers: an edit in between (`revision`) makes it stale.
#[derive(Clone, Debug, PartialEq)]
pub struct SubjectKey {
    doc: DocId,
    revision: u64,
    layer: Option<LayerId>,
    sample_all: bool,
    area: Rect,
    /// The clicked point of a click-to-select.
    point: Option<[f64; 2]>,
    model: &'static str,
}

/// What an inpainting result was computed from: the document state and the stroke.
#[derive(Clone, Debug, PartialEq)]
pub struct InpaintKey {
    doc: DocId,
    revision: u64,
    params: String,
}

/// An inpainting run the shell does on the engine's behalf, for `paint.remove`.
#[derive(Clone, Debug)]
pub struct InpaintRequest {
    pub key: InpaintKey,
    pub model: &'static Model,
    pub input: Tensor,
}

fn inpaint_key(s: &Session, p: &Value) -> Result<InpaintKey> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    Ok(InpaintKey { doc: d.doc.id, revision: d.revision, params: p.to_string() })
}

/// For the shell: if `paint.remove` with `p` would use a model that this process can't run, the
/// model's input for it. `None`: the command needs nothing from the shell.
pub fn inpaint_request(s: &Session, p: &Value) -> Result<Option<InpaintRequest>> {
    let Some((model, input)) = crate::retouch_cmds::remove_model_input(s, p)? else { return Ok(None) };
    Ok(Some(InpaintRequest { key: inpaint_key(s, p)?, model, input }))
}

/// For the shell: the model's output for an [`InpaintRequest`]. The next matching command uses it.
pub fn inpaint_ready(s: &mut Session, key: InpaintKey, output: Vec<f32>) {
    s.ai.inpaint = Some((key, output));
}

/// The waiting inpainting output, if it answers exactly this command on this document state. It
/// is used once.
pub(crate) fn take_inpaint(s: &mut Session, p: &Value) -> Option<Vec<f32>> {
    let want = inpaint_key(s, p).ok()?;
    s.ai.inpaint.take().filter(|(have, _)| *have == want).map(|(_, output)| output)
}

/// A model run the shell does on the engine's behalf (see the module docs).
#[derive(Clone, Debug)]
pub struct SubjectRequest {
    pub key: SubjectKey,
    pub model: &'static Model,
    pub input: Tensor,
}

/// How a command finds its subject.
pub(crate) enum Subject {
    /// The classical algorithm (no model chosen or installed).
    Classical,
    /// A model output the shell computed.
    Output(&'static Model, Vec<f32>),
    /// Run the model here, from its file.
    #[cfg(feature = "ml-runtime")]
    Local(&'static Model, std::path::PathBuf, std::sync::Arc<std::sync::Mutex<photocraft_ml::runtime::Runtime>>),
}

impl From<photocraft_ml::MlError> for EngineError {
    fn from(e: photocraft_ml::MlError) -> Self {
        EngineError::Other(e.to_string())
    }
}

/// A headless session (CLI, MCP): a session that has no app around it to manage its models. It
/// uses the models in `PHOTOCRAFT_MODEL_DIR`, when that is set. Explicit, so tests and embedders
/// are not changed by the environment.
pub fn headless(mut s: Session) -> Session {
    if !cfg!(target_arch = "wasm32") {
        s.ai.model_dir = std::env::var_os("PHOTOCRAFT_MODEL_DIR").filter(|d| !d.is_empty()).map(std::path::PathBuf::from);
    }
    s
}

/// Where an installed model lives on disk, if it is there.
/// (`<id>.onnx`, plus `<id>.data` for a model that keeps its weights in a second file.)
fn model_file(s: &Session, m: &Model) -> Option<std::path::PathBuf> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    let path = s.ai.model_dir.as_ref()?.join(format!("{}.onnx", m.id));
    (path.is_file() && (m.data.is_none() || path.with_extension("data").is_file())).then_some(path)
}

/// Does this process run the model itself (rather than the shell's worker)?
pub(crate) fn runs_here(s: &Session, m: &Model) -> bool {
    cfg!(feature = "ml-runtime") && model_file(s, m).is_some()
}

/// An installed model this process can run: its file and the shared runtime.
#[cfg(feature = "ml-runtime")]
pub(crate) struct Local {
    model: &'static Model,
    path: std::path::PathBuf,
    runtime: std::sync::Arc<std::sync::Mutex<photocraft_ml::runtime::Runtime>>,
}

#[cfg(feature = "ml-runtime")]
pub(crate) fn local(s: &Session, model: &'static Model) -> Option<Local> {
    model_file(s, model).map(|path| Local { model, path, runtime: s.ai.runtime.clone() })
}

#[cfg(feature = "ml-runtime")]
impl Local {
    /// The runtime with this model loaded (read and checked on first use).
    pub(crate) fn loaded(&self, ctx: &JobCtx) -> Result<std::sync::MutexGuard<'_, photocraft_ml::runtime::Runtime>> {
        let mut rt = self.runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !rt.is_loaded(self.model) {
            ctx.progress(0.0, &format!("Loading {}", self.model.name));
            let (bytes, data) = read_model(self.model, &self.path)?;
            rt.load(self.model, &bytes, data.as_deref())?;
            ctx.check()?;
        }
        Ok(rt)
    }
}

/// Can this model be used in this session?
pub fn is_installed(s: &Session, m: &Model) -> bool {
    s.ai.external.contains(m.id) || model_file(s, m).is_some()
}

/// The shell reports which models it holds (the browser's cache). Unknown ids are ignored.
pub fn set_external<'a>(s: &mut Session, ids: impl IntoIterator<Item = &'a str>) {
    s.ai.external = ids.into_iter().filter_map(|id| photocraft_ml::find(id).ok().map(|m| m.id)).collect();
}

/// The model a command's `model` param asks for: `"classical"` is none, a model id is that model
/// (an error when it is not installed), and `"auto"` (the default) is the first installed model
/// for the purpose, or none.
pub(crate) fn chosen_model(s: &Session, cmd: &str, p: &Value, purpose: Purpose) -> Result<Option<&'static Model>> {
    let want = match p.get("model") {
        None | Some(Value::Null) => "auto",
        Some(Value::String(id)) => id.as_str(),
        Some(_) => return Err(EngineError::BadParams { cmd: cmd.into(), msg: "\"model\" must be \"auto\", \"classical\" or a model id".into() }),
    };
    match want {
        "classical" => Ok(None),
        "auto" => Ok(MODELS.iter().find(|m| m.purpose == purpose && is_installed(s, m))),
        id => {
            let m = photocraft_ml::find(id).map_err(|e| EngineError::BadParams { cmd: cmd.into(), msg: e.to_string() })?;
            if m.purpose != purpose {
                return Err(EngineError::BadParams { cmd: cmd.into(), msg: format!("the {} model can't be used for this", m.name) });
            }
            if !is_installed(s, m) {
                return Err(EngineError::Other(format!("The {} model is not installed. Install it from Filter › AI › AI Models.", m.name)));
            }
            Ok(Some(m))
        }
    }
}

/// The part of the canvas a subject command looks at, and which pixels it reads.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ask {
    pub layer: Option<LayerId>,
    pub sample_all: bool,
    pub area: Rect,
    /// Object Selection's click (document pixels): a segmentation model selects what is there.
    pub point: Option<[f64; 2]>,
}

/// `"point": [x, y]` of a click-to-select, when given.
pub(crate) fn point_param(p: &Value) -> Option<[f64; 2]> {
    let a = p.get("point")?.as_array()?;
    match (a.first().and_then(Value::as_f64), a.get(1).and_then(Value::as_f64)) {
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Some([x, y]),
        _ => None,
    }
}

/// The kind of model a subject command uses: a click asks a segmentation model.
fn purpose(cmd: &str, p: &Value) -> Purpose {
    if cmd == "select.object" && p.get("point").is_some() { Purpose::Segment } else { Purpose::Subject }
}

/// The same reading of `p` as the commands themselves make, so the shell's request and the
/// command agree on what the mask is of.
fn ask(s: &Session, cmd: &str, p: &Value) -> Result<Ask> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let flag = |k: &str, default: bool| p.get(k).and_then(Value::as_bool).unwrap_or(default);
    let canvas = d.doc.bounds();
    match cmd {
        "layer.removeBackground" => {
            let id = layer_param(s, p)?;
            let sample_all = flag("sampleAllLayers", false);
            let area = crate::cutout_cmds::subject_area(d.doc.layer(id).ok_or(EngineError::NoLayer(id))?, canvas, sample_all);
            Ok(Ask { layer: Some(id), sample_all, area, point: None })
        }
        "select.subject" => Ok(Ask { layer: d.active_layer, sample_all: flag("sampleAllLayers", true), area: canvas, point: None }),
        "select.object" if p.get("point").is_some() => {
            let point = point_param(p)
                .filter(|[x, y]| canvas.contains(x.floor() as i32, y.floor() as i32))
                .ok_or_else(|| EngineError::BadParams { cmd: cmd.into(), msg: "\"point\" must be [x, y] on the canvas".into() })?;
            Ok(Ask { layer: d.active_layer, sample_all: flag("sampleAllLayers", false), area: canvas, point: Some(point) })
        }
        "select.object" => {
            let rect = crate::smartselect_cmds::rect_param(p)
                .filter(|r| !r.is_empty())
                .ok_or_else(|| EngineError::BadParams { cmd: cmd.into(), msg: "needs \"rect\": [x, y, w, h]".into() })?;
            Ok(Ask { layer: d.active_layer, sample_all: flag("sampleAllLayers", false), area: rect.intersect(&canvas), point: None })
        }
        _ => Err(EngineError::Other(format!("`{cmd}` does not use a subject model"))),
    }
}

fn key(s: &Session, a: &Ask, m: &'static Model) -> Result<SubjectKey> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    Ok(SubjectKey { doc: d.doc.id, revision: d.revision, layer: a.layer, sample_all: a.sample_all, area: a.area, point: a.point, model: m.id })
}

fn check_area(area: Rect) -> Result<(usize, usize)> {
    let (w, h) = (u64::from(area.width()), u64::from(area.height()));
    if w < 4 || h < 4 {
        return Err(EngineError::Other("the area is too small to find a subject in".into()));
    }
    if w * h > MAX_MASK_PX {
        return Err(EngineError::Other(format!("the area is too large for an AI model ({w} × {h} px); use the classical method or a smaller selection")));
    }
    Ok((w as usize, h as usize))
}

/// The model's input for `area` of the document: sampled at about twice the model's resolution,
/// converted from the document's colours to sRGB (what the models were trained on), then scaled
/// to the model's square.
///
/// CMYK, Lab and other non-RGB documents are read through the compositor, which converts them
/// with the colour-management engine; a single layer's raw values would not be colours a model
/// understands. The cost is that other visible layers are in the picture too.
fn model_input(doc: &Document, a: &Ask, model: &'static Model) -> Result<Tensor> {
    let Ask { layer, sample_all, area, point } = *a;
    check_area(area)?;
    let rgbish = matches!(crate::color_cmds::mode_space(doc.mode), photocraft_cms::ColorSpace::Rgb | photocraft_cms::ColorSpace::Gray);
    let step = segment::scale_for(area, 4 * model.side * model.side);
    let mut img = with_doc_sampler(doc, layer, sample_all || !rgbish, |smp, _| smp.rgb_scaled(area, step));
    let srgb = photocraft_cms::Builtin::Srgb.profile();
    if let Ok(t) = photocraft_cms::cached(&crate::color_cmds::composite_profile(doc), srgb, photocraft_cms::TransformOptions::default())
        && t.inputs() == 3
        && t.outputs() == 3
    {
        t.apply(img.px.as_flattened_mut(), 3);
    }
    if let (Purpose::Segment, Some([x, y])) = (model.purpose, point) {
        let at = [((x - f64::from(area.x0)) / step as f64) as f32, ((y - f64::from(area.y0)) / step as f64) as f32];
        return Ok(photocraft_ml::tensor::segment_input(&img.px, img.w, img.h, at, model)?);
    }
    Ok(photocraft_ml::tensor::subject_input(&img.px, img.w, img.h, model)?)
}

/// For the shell: if `cmd` with `p` would use a model that this process can't run, the input to
/// run it on. `None` means the command needs nothing from the shell (classical, or the model runs
/// in-process).
pub fn subject_request(s: &Session, cmd: &str, p: &Value) -> Result<Option<SubjectRequest>> {
    let Some(model) = chosen_model(s, cmd, p, purpose(cmd, p))? else { return Ok(None) };
    if runs_here(s, model) {
        return Ok(None);
    }
    let a = ask(s, cmd, p)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    Ok(Some(SubjectRequest { key: key(s, &a, model)?, model, input: model_input(&d.doc, &a, model)? }))
}

/// For the shell: the model's output for a [`SubjectRequest`]. The next matching command uses it.
pub fn subject_ready(s: &mut Session, key: SubjectKey, output: Vec<f32>) {
    s.ai.ready = Some((key, output));
}

/// Decide what `cmd` looks at and how it finds its subject. Takes a waiting output when it
/// answers exactly this ask.
pub(crate) fn subject_plan(s: &mut Session, cmd: &str, p: &Value) -> Result<(Ask, Subject)> {
    let a = ask(s, cmd, p)?;
    Ok((a, plan(s, cmd, p, &a)?))
}

fn plan(s: &mut Session, cmd: &str, p: &Value, a: &Ask) -> Result<Subject> {
    let Some(model) = chosen_model(s, cmd, p, purpose(cmd, p))? else { return Ok(Subject::Classical) };
    let want = key(s, a, model)?;
    // An output is used once, and only for the request it answers.
    if let Some((have, output)) = s.ai.ready.take()
        && have == want
    {
        return Ok(Subject::Output(model, output));
    }
    #[cfg(feature = "ml-runtime")]
    if let Some(path) = model_file(s, model) {
        return Ok(Subject::Local(model, path, s.ai.runtime.clone()));
    }
    // Installed where only the shell can run it, and the shell has not: an explicit request for
    // the model fails; "auto" quietly uses the classical method.
    if matches!(p.get("model").and_then(Value::as_str), None | Some("auto")) {
        return Ok(Subject::Classical);
    }
    Err(EngineError::Other(format!("The {} model runs in the app's background worker and has no result for this image yet.", model.name)))
}

/// A model output as a region over `area`.
fn output_region(output: &[f32], model: &'static Model, area: Rect) -> Result<Option<Region>> {
    let (w, h) = check_area(area)?;
    let mask = if model.purpose == Purpose::Segment {
        photocraft_ml::tensor::segment_mask(output, model, w, h)?
    } else {
        photocraft_ml::tensor::subject_mask(output, model, w, h)?
    };
    Ok(segment::trim_region(Region { bbox: area, mask }))
}

/// Read a file of at most `max` bytes (a wrong file can't exhaust memory).
#[cfg(feature = "ml-runtime")]
fn read_bounded(model: &Model, path: &std::path::Path, max: u64) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).map_err(|e| EngineError::Other(format!("could not read the {} model ({}): {e}", model.name, path.display())))?;
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes).map_err(|e| EngineError::Other(format!("could not read the {} model: {e}", model.name)))?;
    Ok(bytes)
}

/// Read and check a model file, and its weights file beside it (`.data`) when it has one.
#[cfg(feature = "ml-runtime")]
fn read_model(model: &Model, path: &std::path::Path) -> Result<(Vec<u8>, Option<Vec<u8>>)> {
    let bytes = read_bounded(model, path, model.bytes)?;
    photocraft_ml::runtime::verify(model, &bytes)?;
    let data = match model.data {
        Some(d) => {
            let data = read_bounded(model, &path.with_extension("data"), d.bytes)?;
            photocraft_ml::runtime::verify_data(model, &data)?;
            Some(data)
        }
        None => None,
    };
    Ok((bytes, data))
}

/// Find the subject (runs inside the command's job, on its copy of the document). `None`:
/// nothing found. `object` is Object Selection: the subject inside the rectangle `a.area`.
pub(crate) fn find_subject(doc: &Document, a: &Ask, object: bool, plan: Subject, ctx: &JobCtx) -> Result<Option<Region>> {
    let Ask { layer, sample_all, area, .. } = *a;
    ctx.check()?;
    match plan {
        Subject::Classical if object => Ok(with_doc_sampler(doc, layer, sample_all, |smp, d| segment::grabcut::object_select(smp, d.bounds(), area, 160_000))),
        // Cancel reaches the min cuts inside Select Subject, so Esc doesn't wait for them.
        Subject::Classical => ctx
            .stage(0.1, 0.6, "Finding subject", |ctl| with_doc_sampler(doc, layer, sample_all, |smp, _| segment::subject::select_subject_with(smp, area, ctl)))
            .map_err(|_| EngineError::Cancelled),
        Subject::Output(model, output) => output_region(&output, model, area),
        #[cfg(feature = "ml-runtime")]
        Subject::Local(model, path, runtime) => {
            ctx.progress(0.1, "Reading the image");
            let input = model_input(doc, a, model)?;
            ctx.check()?;
            let local = Local { model, path, runtime };
            let rt = local.loaded(ctx)?;
            ctx.progress(0.35, &format!("Finding the subject with {}", model.name));
            let output = rt.run(model, input)?;
            drop(rt);
            ctx.check()?;
            output_region(&output.data, model, area)
        }
    }
}

/// The transform from a document's colours to sRGB (what the models were trained on), or back,
/// for pixels held as RGB triples (a gray document's as three equal values), with the stride to
/// apply it at. Gray goes by its tone curve to sGray (sRGB's curve), one value at a time: the
/// gray profile's RGB view has no inverse to come back through.
pub(crate) fn srgb_transform(doc: &Document, back: bool) -> Option<(std::sync::Arc<photocraft_cms::Transform>, usize)> {
    use photocraft_cms::{Builtin, ColorSpace};
    let (profile, srgb, n) = match crate::color_cmds::mode_space(doc.mode) {
        ColorSpace::Gray => (crate::color_cmds::document_profile(doc), Builtin::SGray.profile(), 1),
        _ => (crate::color_cmds::composite_profile(doc), Builtin::Srgb.profile(), 3),
    };
    let t = if back { photocraft_cms::cached(srgb, &profile, Default::default()) } else { photocraft_cms::cached(&profile, srgb, Default::default()) };
    t.ok().filter(|t| t.inputs() == n && t.outputs() == n).map(|t| (t, n))
}

/// How wide the soft edge of a model mask is, in document pixels: the mask was made at the
/// model's resolution and scaled up, so its edge is as wide as one model pixel.
pub(crate) fn mask_edge_px(plan: &Subject, area: Rect) -> f32 {
    let side = match plan {
        Subject::Classical => return 0.0,
        Subject::Output(m, _) => m,
        #[cfg(feature = "ml-runtime")]
        Subject::Local(m, ..) => m,
    };
    // A segmentation decoder's mask is a quarter of its input's side.
    let side = if side.purpose == Purpose::Segment { side.side / 4 } else { side.side };
    area.width().max(area.height()).max(1) as f32 / side as f32
}

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

fn id_param(cmd: &str, p: &Value) -> Result<&'static Model> {
    let id = p
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| EngineError::BadParams { cmd: cmd.into(), msg: "needs \"id\": a model id (see ai.models)".into() })?;
    photocraft_ml::find(id).map_err(|e| EngineError::BadParams { cmd: cmd.into(), msg: e.to_string() })
}

fn models(s: &mut Session, _: &Value) -> Result<Value> {
    let list: Vec<Value> = MODELS
        .iter()
        .map(|m| {
            json!({
                "id": m.id, "name": m.name, "license": m.license, "bytes": m.total_bytes(), "size": m.size_label(), "url": m.url,
                "dataUrl": m.data.map(|d| d.url), "dataSha256": m.data.map(|d| d.sha256),
                "sha256": m.sha256, "about": m.blurb, "installed": is_installed(s, m),
                "purpose": match m.purpose { Purpose::Subject => "subject", Purpose::Upscale => "upscale", Purpose::Inpaint => "inpaint", Purpose::Denoise => "denoise", Purpose::Segment => "segment" },
            })
        })
        .collect();
    Ok(json!({ "models": list, "runsHere": cfg!(feature = "ml-runtime"), "modelDir": s.ai.model_dir.as_ref().map(|d| d.display().to_string()) }))
}

/// Install a model from a file already on this machine (the app downloads it first; a user or an
/// agent can also fetch the pinned URL by any means). The file must match the registry's SHA-256.
#[cfg(all(feature = "ml-runtime", not(target_arch = "wasm32")))]
fn install(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "ai.model.install";
    let model = id_param(CMD, p)?;
    let from = p
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| EngineError::BadParams { cmd: CMD.into(), msg: "needs \"path\": the downloaded model file".into() })?;
    let dir = s.ai.model_dir.clone().ok_or_else(|| EngineError::Other("this session has no folder to keep AI models in".into()))?;
    let from = std::path::Path::new(from);
    let bytes = read_bounded(model, from, model.bytes)?;
    photocraft_ml::runtime::verify(model, &bytes)?;
    // A model whose weights are a second file needs that file too, checked the same way.
    let data = match model.data {
        Some(d) => {
            let path = p.get("dataPath").and_then(Value::as_str).ok_or_else(|| EngineError::BadParams {
                cmd: CMD.into(),
                msg: format!("the {} model has a second file: needs \"dataPath\" (its weights, from {})", model.name, d.url),
            })?;
            let data = read_bounded(model, std::path::Path::new(path), d.bytes)?;
            photocraft_ml::runtime::verify_data(model, &data)?;
            Some(data)
        }
        None => None,
    };
    let fail = |e: std::io::Error| EngineError::Other(format!("could not save the {} model in {}: {e}", model.name, dir.display()));
    std::fs::create_dir_all(&dir).map_err(fail)?;
    // Written beside its final name and renamed, so a crash never leaves half a model in place.
    // The weights go first: the model counts as installed only once its `.onnx` is there.
    let to = dir.join(format!("{}.onnx", model.id));
    for (bytes, to) in data.iter().map(|d| (d, to.with_extension("data"))).chain([(&bytes, to.clone())]) {
        let tmp = to.with_extension("part");
        std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, &to)).map_err(fail)?;
    }
    Ok(json!({ "id": model.id, "installed": true, "path": to.display().to_string() }))
}

#[cfg(not(all(feature = "ml-runtime", not(target_arch = "wasm32"))))]
fn install(_: &mut Session, p: &Value) -> Result<Value> {
    id_param("ai.model.install", p)?;
    Err(EngineError::Other("In this build the app keeps AI models itself: use Filter › AI › AI Models.".into()))
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let model = id_param("ai.model.remove", p)?;
    if let Some(path) = model_file(s, model) {
        std::fs::remove_file(&path).map_err(|e| EngineError::Other(format!("could not remove {}: {e}", path.display())))?;
        let _ = std::fs::remove_file(path.with_extension("data"));
    }
    #[cfg(feature = "ml-runtime")]
    s.ai.runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner).unload(model);
    s.ai.external.remove(model.id);
    (s.ai.ready, s.ai.upscale, s.ai.inpaint) = (None, None, None);
    Ok(json!({ "id": model.id, "installed": false }))
}

/// AI command specs.
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "ai.models",
            label: "List AI Models",
            menu: &[],
            shortcut: None,
            params: r##"{} → {models:[{id,name,purpose,license,bytes,size,url,sha256,dataUrl,dataSha256,about,installed}],runsHere,modelDir} (the local models PhotoCraft can use; none is bundled)"##,
            enabled: always,
            run: models,
            journal: false,
        },
        CommandSpec {
            id: "ai.model.install",
            label: "Install AI Model",
            menu: &[],
            shortcut: None,
            params: r##"{"id":model,"path":file,"dataPath":file?} → {id,installed,path} (installs a model file downloaded from its pinned URL; refused unless its SHA-256 matches; "dataPath" is the weights file of a model that has one, see dataUrl in ai.models)"##,
            enabled: always,
            run: install,
            journal: false,
        },
        CommandSpec {
            id: "ai.model.remove",
            label: "Remove AI Model",
            menu: &[],
            shortcut: None,
            params: r##"{"id":model} → {id,installed} (deletes the installed model file)"##,
            enabled: always,
            run: remove,
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests;
