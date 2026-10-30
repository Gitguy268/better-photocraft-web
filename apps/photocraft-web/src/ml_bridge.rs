//! Local AI in the browser: models are downloaded into Cache Storage (never into the app's own
//! memory) after their SHA-256 is checked, and run in the AI worker, a second wasm in a Web Worker
//! (`apps/photocraft-ml-worker`), so the page keeps drawing. See `docs/decisions.md`.
//!
//! Cache Storage and `crypto.subtle` need a secure context (https or localhost). Elsewhere the
//! service reports that, and the built-in methods keep working.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Float32Array, Object, Reflect, Uint8Array};
use photocraft_ml::{MODELS, Model, Tensor};
use photocraft_ui_egui::ai_ui::{AiEvent, AiService};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};

/// The cache the models live in. The worker reads the same one.
const CACHE: &str = "photocraft-ai-models-v1";
/// Written by the build next to `index.html`: the folder that holds this build's worker files.
const WORKER_DIR_FILE: &str = "ai-worker.txt";
const WORKER_LOADER: &str = "photocraft-ml-worker_loader.js";

#[derive(Default)]
struct Shared {
    events: Vec<AiEvent>,
    /// Aborts the download in progress.
    abort: Option<web_sys::AbortController>,
    worker: Option<web_sys::Worker>,
    /// The worker's wasm has loaded and it listens.
    ready: bool,
    /// A request made before the worker was ready.
    queued: Option<JsValue>,
    /// The run in progress, to fail it if the worker dies.
    ticket: Option<u64>,
}

type State = Rc<RefCell<Shared>>;

fn js(e: JsValue) -> String {
    // A DOMException or Error says what went wrong in its `message`.
    Reflect::get(&e, &"message".into()).ok().and_then(|m| m.as_string()).unwrap_or_else(|| format!("{e:?}"))
}

fn push(state: &State, ctx: &egui::Context, event: AiEvent) {
    state.borrow_mut().events.push(event);
    ctx.request_repaint();
}

async fn cache() -> Result<web_sys::Cache, String> {
    let window = web_sys::window().ok_or("no window")?;
    if !window.is_secure_context() {
        return Err("AI models need a secure page (https, or localhost)".into());
    }
    JsFuture::from(window.caches().map_err(js)?.open(CACHE)).await.map_err(js)?.dyn_into().map_err(|_| "no cache storage".to_string())
}

/// Tell the app which models are in the cache.
fn report_installed(state: State, ctx: egui::Context) {
    spawn_local(async move {
        let mut ids = Vec::new();
        if let Ok(cache) = cache().await {
            for m in MODELS {
                let mut all = true;
                for (url, _, _) in m.files() {
                    all &= JsFuture::from(cache.match_with_str(url)).await.is_ok_and(|r| r.is_instance_of::<web_sys::Response>());
                }
                if all {
                    ids.push(m.id);
                }
            }
        }
        push(&state, &ctx, AiEvent::Installed(ids));
    });
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Fetch a model's files, reporting progress; check each SHA-256; put them in the cache.
async fn download(state: &State, ctx: &egui::Context, model: &'static Model, signal: web_sys::AbortSignal) -> Result<(), String> {
    let cache = cache().await?;
    let window = web_sys::window().ok_or("no window")?;
    let subtle = window.crypto().map_err(js)?.subtle();
    let (total, mut before) = (model.total_bytes(), 0u64);
    // Checked first, stored last: a model is in the cache whole or not at all.
    let mut checked = Vec::new();
    for (url, sha256, bytes) in model.files() {
        let init = web_sys::RequestInit::new();
        init.set_signal(Some(&signal));
        let resp: web_sys::Response = JsFuture::from(window.fetch_with_str_and_init(url, &init)).await.map_err(js)?.dyn_into().map_err(|_| "not a response")?;
        if !resp.ok() {
            return Err(format!("the server answered HTTP {}", resp.status()));
        }
        // Read the body in chunks for the progress bar. The chunks stay in the browser's memory.
        let reader: web_sys::ReadableStreamDefaultReader = resp.body().ok_or("empty response")?.get_reader().dyn_into().map_err(|_| "no stream reader")?;
        let chunks = Array::new();
        let (mut received, mut reported) = (0u64, 0u64);
        loop {
            let step = JsFuture::from(reader.read()).await.map_err(js)?;
            if Reflect::get(&step, &"done".into()).is_ok_and(|d| d.is_truthy()) {
                break;
            }
            let chunk: Uint8Array = Reflect::get(&step, &"value".into()).map_err(js)?.dyn_into().map_err(|_| "unexpected data")?;
            received += u64::from(chunk.length());
            // A wrong or endless file stops here, not when memory runs out.
            if received > bytes {
                let _ = reader.cancel();
                return Err("the file is larger than the model should be".into());
            }
            chunks.push(&chunk);
            if received - reported >= total / 100 {
                reported = received;
                push(state, ctx, AiEvent::Progress { model, received: before + received });
            }
        }
        before += received;
        let blob = web_sys::Blob::new_with_u8_array_sequence(&chunks).map_err(js)?;
        let buffer = JsFuture::from(blob.array_buffer()).await.map_err(js)?;
        let digest = JsFuture::from(subtle.digest_with_str_and_buffer_source("SHA-256", &buffer.unchecked_into::<js_sys::ArrayBuffer>()).map_err(js)?)
            .await
            .map_err(js)?;
        if hex(&Uint8Array::new(&digest).to_vec()) != sha256 {
            return Err(photocraft_ml::MlError::Checksum { name: model.name }.to_string());
        }
        checked.push((url, blob));
    }
    for (url, blob) in checked {
        let stored = web_sys::Response::new_with_opt_blob(Some(&blob)).map_err(js)?;
        JsFuture::from(cache.put_with_str(url, &stored)).await.map_err(|e| format!("the browser could not store it ({})", js(e)))?;
    }
    Ok(())
}

fn stop_worker(state: &State) {
    let mut s = state.borrow_mut();
    if let Some(w) = s.worker.take() {
        w.terminate();
    }
    (s.ready, s.queued, s.ticket) = (false, None, None);
}

/// The folder of this build's worker files. Always asked from the server (`no-cache`): a stale
/// answer after an update would point at the previous build's worker.
async fn worker_dir() -> Result<String, String> {
    let init = web_sys::RequestInit::new();
    init.set_cache(web_sys::RequestCache::NoCache);
    let window = web_sys::window().ok_or("no window")?;
    let resp: web_sys::Response =
        JsFuture::from(window.fetch_with_str_and_init(WORKER_DIR_FILE, &init)).await.map_err(js)?.dyn_into().map_err(|_| "not a response")?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    Ok(JsFuture::from(resp.text().map_err(js)?).await.map_err(js)?.as_string().unwrap_or_default().trim().to_string())
}

/// Start the worker. Its files live in a folder named after their content (so they can be cached
/// forever); `ai-worker.txt` names the folder of this build.
fn start_worker(state: State, ctx: egui::Context) {
    spawn_local(async move {
        let fail = |state: &State, ctx: &egui::Context, error: String| {
            let ticket = state.borrow_mut().ticket.take();
            stop_worker(state);
            if let Some(ticket) = ticket {
                push(state, ctx, AiEvent::Inferred { ticket, output: Err(error) });
            }
        };
        let dir = match worker_dir().await {
            Ok(dir) => dir,
            Err(e) => return fail(&state, &ctx, format!("this site has no AI worker ({WORKER_DIR_FILE}: {e})")),
        };
        // A folder name, not a path elsewhere or an HTML error page.
        if dir.is_empty() || !dir.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return fail(&state, &ctx, format!("this site's {WORKER_DIR_FILE} is not valid"));
        }
        let worker = match web_sys::Worker::new(&format!("{dir}/{WORKER_LOADER}")) {
            Ok(w) => w,
            Err(e) => return fail(&state, &ctx, format!("the AI worker could not start ({})", js(e))),
        };
        let (s, c) = (state.clone(), ctx.clone());
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
            let msg = e.data();
            let field = |k: &str| Reflect::get(&msg, &k.into()).unwrap_or(JsValue::UNDEFINED);
            if field("ready").is_truthy() {
                let mut st = s.borrow_mut();
                st.ready = true;
                if let (Some(w), Some(queued)) = (st.worker.clone(), st.queued.take()) {
                    let _ = w.post_message(&queued);
                }
                return;
            }
            let Some(ticket) = field("id").as_f64().map(|t| t as u64) else { return };
            let output = match field("output").dyn_into::<Float32Array>() {
                Ok(out) => Ok(out.to_vec()),
                Err(_) => Err(field("error").as_string().unwrap_or_else(|| "the AI worker sent no result".into())),
            };
            s.borrow_mut().ticket = None;
            push(&s, &c, AiEvent::Inferred { ticket, output });
        });
        let (s, c) = (state.clone(), ctx.clone());
        // A trap in the worker (out of memory, a browser without WebAssembly SIMD) arrives here.
        let on_error = Closure::<dyn FnMut(web_sys::ErrorEvent)>::new(move |e: web_sys::ErrorEvent| {
            e.prevent_default();
            let why = e.message();
            fail(
                &s,
                &c,
                if why.is_empty() {
                    "the AI worker stopped; this browser may be too old or out of memory".into()
                } else {
                    format!("the AI worker stopped: {why}")
                },
            );
        });
        worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        // The worker lives as long as the page; its listeners with it.
        on_message.forget();
        on_error.forget();
        state.borrow_mut().worker = Some(worker);
    });
}

fn infer(state: &State, ctx: &egui::Context, ticket: u64, model: &'static Model, input: Tensor) {
    let msg = Object::new();
    for (key, value) in
        [("id", JsValue::from(ticket as f64)), ("model", model.id.into()), ("cache", CACHE.into()), ("input", Float32Array::from(input.data.as_slice()).into())]
    {
        let _ = Reflect::set(&msg, &key.into(), &value);
    }
    let mut s = state.borrow_mut();
    s.ticket = Some(ticket);
    match (s.worker.clone(), s.ready) {
        (Some(w), true) => {
            if let Err(e) = w.post_message(&msg) {
                s.ticket = None;
                s.events.push(AiEvent::Inferred { ticket, output: Err(js(e)) });
            }
        }
        (Some(_), false) => s.queued = Some(msg.into()),
        (None, _) => {
            s.queued = Some(msg.into());
            drop(s);
            start_worker(state.clone(), ctx.clone());
        }
    }
}

/// The platform service the app's AI features use (see `photocraft_ui_egui::ai_ui`).
pub fn service(ctx: egui::Context) -> AiService {
    let state: State = Rc::default();
    report_installed(state.clone(), ctx.clone());
    let (s1, c1) = (state.clone(), ctx.clone());
    let s2 = state.clone();
    let (s3, c3) = (state.clone(), ctx.clone());
    let (s4, c4) = (state.clone(), ctx.clone());
    let s5 = state.clone();
    AiService {
        download: Box::new(move |model| {
            let (state, ctx) = (s1.clone(), c1.clone());
            let Ok(abort) = web_sys::AbortController::new() else {
                return push(&state, &ctx, AiEvent::DownloadFailed { model, error: "downloads can't be started".into() });
            };
            let signal = abort.signal();
            state.borrow_mut().abort = Some(abort);
            spawn_local(async move {
                let result = download(&state, &ctx, model, signal.clone()).await;
                state.borrow_mut().abort = None;
                match result {
                    Ok(()) => report_installed(state, ctx),
                    // Cancelled by the user: the app already knows.
                    Err(_) if signal.aborted() => {}
                    Err(error) => push(&state, &ctx, AiEvent::DownloadFailed { model, error }),
                }
            });
        }),
        cancel_download: Box::new(move || {
            if let Some(abort) = s2.borrow_mut().abort.take() {
                abort.abort();
            }
        }),
        remove: Some(Box::new(move |model| {
            let (state, ctx) = (s3.clone(), c3.clone());
            // The worker may hold the model in memory.
            stop_worker(&state);
            spawn_local(async move {
                if let Ok(cache) = cache().await {
                    for (url, _, _) in model.files() {
                        let _ = JsFuture::from(cache.delete_with_str(url)).await;
                    }
                }
                report_installed(state, ctx);
            });
        })),
        infer: Some(Box::new(move |ticket, model, input| infer(&s4, &c4, ticket, model, input))),
        // The only way to stop a running model is to stop its worker; the next run starts a new one.
        cancel_infer: Box::new(move || stop_worker(&s5)),
        poll: Box::new(move || std::mem::take(&mut state.borrow_mut().events)),
    }
}
