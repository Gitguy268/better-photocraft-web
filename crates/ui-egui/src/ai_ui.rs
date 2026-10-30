//! The shell's side of the local AI features: asking before a model is downloaded, showing the
//! download, running a model in the platform's worker when the engine can't (the browser), and
//! the model manager (Filter › AI › AI Models).
//!
//! Everything a model does still goes through an engine command (`layer.removeBackground`,
//! `select.subject`, `select.object`; see `photocraft_engine::ai_cmds`). This module only gets the
//! model in place first. Without a platform service ([`crate::Services::ai`]) nothing here runs
//! and the commands use what the engine has.

use photocraft_engine::ai_cmds::{self, InpaintKey, SubjectKey};
use photocraft_engine::ai_upscale::{self, UpscaleJob};
use photocraft_ml::{MODELS, Model, Purpose, Tensor};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::widgets::{ButtonRole, DialogButton};

/// Runs a model outside the engine: `(ticket, model, input)`.
pub type InferFn = Box<dyn FnMut(u64, &'static Model, Tensor)>;

/// What the platform does for the AI features.
pub struct AiService {
    /// Download a model the user agreed to. Progress and the outcome arrive through `poll`.
    pub download: Box<dyn FnMut(&'static Model)>,
    /// Stop the download in progress.
    pub cancel_download: Box<dyn FnMut()>,
    /// Delete a model the platform holds itself (the browser's cache). `None`: the engine's
    /// `ai.model.remove` deletes the file.
    pub remove: Option<Box<dyn FnMut(&'static Model)>>,
    /// Run a model outside the engine: `(ticket, model, input)`, answered by an
    /// [`AiEvent::Inferred`]. `None` when the engine runs models itself (desktop).
    pub infer: Option<InferFn>,
    /// Abandon the run in progress (its answer never comes).
    pub cancel_infer: Box<dyn FnMut()>,
    pub poll: Box<dyn FnMut() -> Vec<AiEvent>>,
}

/// What the platform reports back.
pub enum AiEvent {
    /// The models the platform holds itself, whenever that changes (and once at start-up).
    Installed(Vec<&'static str>),
    /// A download finished into a file (and a second one, for a model whose weights are a
    /// separate file); the engine verifies and installs them.
    Downloaded {
        model: &'static Model,
        path: String,
        data: Option<String>,
    },
    Progress {
        model: &'static Model,
        received: u64,
    },
    DownloadFailed {
        model: &'static Model,
        error: String,
    },
    Inferred {
        ticket: u64,
        output: Result<Vec<f32>, String>,
    },
}

/// A command waiting for a model.
type Then = Option<(String, Value)>;

/// The shell's AI state.
#[derive(Default)]
pub struct AiUi {
    /// "Download this model?", and the command that asked.
    offer: Option<(&'static Model, Then)>,
    downloading: Option<(&'static Model, u64, Then)>,
    /// A command whose model is running in the platform's worker.
    task: Option<Task>,
    pub manager_open: bool,
    /// The user chose the built-in method when offered a model for these purposes: don't ask
    /// again this session.
    declined: Vec<Purpose>,
    /// The options last chosen in Remove Background Options, used by the one-click command.
    last_options: Map<String, Value>,
    /// The next command runs as it is (it is ours, coming back with its model ready).
    passthrough: bool,
    tickets: u64,
}

/// The fields of the Remove Background Options dialog, in the command's own parameter notation.
const OPTIONS_SPEC: &str = r##"{"model":"auto|classical|u2netp|isnet","output":"mask|newLayer|newLayerWithMask|cutout","refine":bool=true,"feather":0..250=0,"decontaminate":bool=false,"amount":0..100=100}"##;

/// What the worker's answers are for.
enum Work {
    /// One run: the subject mask of this request.
    Subject(SubjectKey),
    /// A run per tile: the upscale (or denoise) in progress.
    Upscale(Box<UpscaleJob>),
    /// One run: the Remove tool's fill for this stroke.
    Inpaint(InpaintKey),
}

/// A command waiting on the platform's worker.
struct Task {
    ticket: u64,
    model: &'static Model,
    work: Work,
    /// The command to run when the work is done.
    id: String,
    params: Value,
}

/// The kind of model a command can use, for the commands this module prepares.
fn purpose_of(id: &str) -> Option<Purpose> {
    match id {
        "layer.removeBackground" | "select.subject" | "select.object" => Some(Purpose::Subject),
        ai_upscale::CMD => Some(Purpose::Upscale),
        ai_upscale::DENOISE_CMD => Some(Purpose::Denoise),
        "paint.remove" => Some(Purpose::Inpaint),
        _ => None,
    }
}

/// Called for every command the UI runs. `Some`: this module took it (a dialog is open, or the
/// model is running) and the command itself runs later.
pub fn intercept(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if std::mem::take(&mut app.ai.passthrough) || app.automation_input || app.services.ai.is_none() {
        return None;
    }
    let purpose = purpose_of(id)?;
    let mut params = if params.is_object() { params.clone() } else { json!({}) };
    if id == "layer.removeBackground"
        && let Some(obj) = params.as_object_mut()
    {
        if obj.contains_key("output") {
            // From the options dialog: remember the choices for the one-click command.
            app.ai.last_options = obj.iter().filter(|(k, _)| !k.starts_with("__") && *k != "layer").map(|(k, v)| (k.clone(), v.clone())).collect();
        } else {
            for (k, v) in &app.ai.last_options {
                obj.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
    }
    let explicit = params.get("model").and_then(Value::as_str).is_some_and(|m| m != "auto");
    let installed = MODELS.iter().any(|m| m.purpose == purpose && ai_cmds::is_installed(&app.session, m));
    // Nothing installed and nothing asked for by name: offer the default model once per session.
    // Object Selection and the Remove tool work on the canvas, where a dialog would interrupt the
    // tool: their models are installed from Filter › AI › AI Models.
    let on_canvas = matches!(id, "select.object" | "paint.remove");
    if !installed && !explicit && !app.ai.declined.contains(&purpose) && !on_canvas && app.ai.downloading.is_none() {
        app.ai.offer = MODELS.iter().find(|m| m.purpose == purpose).map(|m| (m, Some((id.to_string(), params))));
        return Some(Ok(json!({"pending": "download offer"})));
    }
    // A model named in the options but not installed: offer that one.
    if let Some(m) = params.get("model").and_then(Value::as_str).and_then(|m| photocraft_ml::find(m).ok())
        && !ai_cmds::is_installed(&app.session, m)
        && app.ai.downloading.is_none()
    {
        app.ai.offer = Some((m, Some((id.to_string(), params))));
        return Some(Ok(json!({"pending": "download offer"})));
    }
    // What the worker has to do first, if anything: the model, its first input, and what for.
    let request = match purpose {
        // A click with Object Selection asks the segmentation model through the same request.
        Purpose::Subject | Purpose::Segment => {
            ai_cmds::subject_request(&app.session, id, &params).map(|r| r.map(|r| (r.model, Some(r.input), Work::Subject(r.key))))
        }
        Purpose::Upscale => ai_upscale::upscale_request(&app.session, &params).map(|j| j.map(|j| (j.model(), j.next_input(), Work::Upscale(Box::new(j))))),
        Purpose::Denoise => ai_upscale::denoise_request(&app.session, &params).map(|j| j.map(|j| (j.model(), j.next_input(), Work::Upscale(Box::new(j))))),
        Purpose::Inpaint => ai_cmds::inpaint_request(&app.session, &params).map(|r| r.map(|r| (r.model, Some(r.input), Work::Inpaint(r.key)))),
    };
    match request {
        Ok(Some((model, input, work))) => {
            if app.ai.task.is_some() {
                return Some(Err(tl!("An AI model is already running. Wait for it to finish, or cancel it.").to_string()));
            }
            app.services.ai.as_ref().and_then(|s| s.infer.as_ref())?;
            let mut task = Task { ticket: 0, model, work, id: id.to_string(), params };
            match input {
                Some(input) => {
                    send(app, &mut task, input);
                    app.ai.task = Some(task);
                }
                // Nothing for the model to do (a document without pixel layers): finish now.
                None => finish(app, task),
            }
            Some(Ok(json!({"pending": "model"})))
        }
        // The engine runs it (or the built-in method does): pass the merged options on.
        Ok(None) => Some(run_now(app, id, params)),
        Err(e) => Some(Err(e.to_string())),
    }
}

/// Give the worker the task's next input.
fn send(app: &mut PhotocraftApp, task: &mut Task, input: Tensor) {
    app.ai.tickets += 1;
    task.ticket = app.ai.tickets;
    if let Some(infer) = app.services.ai.as_mut().and_then(|s| s.infer.as_mut()) {
        infer(task.ticket, task.model, input);
    }
}

/// The worker's work is done: hand the result to the engine and run the command.
fn finish(app: &mut PhotocraftApp, task: Task) {
    if let Work::Upscale(job) = task.work {
        ai_upscale::upscale_ready(&mut app.session, *job);
    }
    if let Err(e) = run_now(app, &task.id, task.params) {
        crate::notices::error(app, e);
    }
}

/// The worker answered the task's last input. `Err`: the task is over, with this message.
fn answered(app: &mut PhotocraftApp, mut task: Task, output: Vec<f32>) -> Result<(), String> {
    match &mut task.work {
        Work::Subject(key) => {
            ai_cmds::subject_ready(&mut app.session, key.clone(), output);
            finish(app, task);
        }
        Work::Inpaint(key) => {
            ai_cmds::inpaint_ready(&mut app.session, key.clone(), output);
            finish(app, task);
        }
        Work::Upscale(job) => {
            job.put(&output).map_err(|e| e.to_string())?;
            match job.next_input() {
                Some(input) => {
                    send(app, &mut task, input);
                    app.ai.task = Some(task);
                }
                None => finish(app, task),
            }
        }
    }
    Ok(())
}

/// Run a command past [`intercept`].
fn run_now(app: &mut PhotocraftApp, id: &str, params: Value) -> Result<Value, String> {
    app.ai.passthrough = true;
    let r = crate::jobs_ui::run(app, id, params);
    app.ai.passthrough = false;
    r
}

fn run_then(app: &mut PhotocraftApp, then: Then) {
    let Some((id, params)) = then else { return };
    match crate::jobs_ui::run(app, &id, params) {
        Ok(_) => {}
        Err(e) => crate::notices::error(app, e),
    }
}

/// Filter › AI › AI Models, and Layer › Remove Background Options.
pub fn menu(app: &mut PhotocraftApp, id: &str) -> Option<Result<Value, String>> {
    match id {
        "filter.ai.models" => {
            app.ai.manager_open = true;
            Some(Ok(Value::Null))
        }
        "layer.removeBackgroundOptions" => {
            if !crate::menus::is_enabled(app, "layer.removeBackground") {
                return Some(Err(tl!("Remove Background needs an unlocked pixel layer.").to_string()));
            }
            let mut fixed = Map::new();
            for (k, v) in &app.ai.last_options {
                fixed.insert(k.clone(), v.clone());
            }
            Some(Ok(json!({"dialog": crate::filter_dialog::open_with_spec(app, "layer.removeBackground", "Remove Background", OPTIONS_SPEC, fixed)})))
        }
        _ => None,
    }
}

/// Apply what the platform reported since the last frame.
pub fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(events) = app.services.ai.as_mut().map(|s| (s.poll)()) else { return };
    if !events.is_empty() {
        ctx.request_repaint();
    }
    for event in events {
        match event {
            AiEvent::Installed(ids) => {
                ai_cmds::set_external(&mut app.session, ids);
                finish_download(app, None);
            }
            AiEvent::Downloaded { model, path, data } => {
                let done =
                    app.session.execute("ai.model.install", json!({"id": model.id, "path": path, "dataPath": data})).map(|_| ()).map_err(|e| e.to_string());
                #[cfg(not(target_arch = "wasm32"))]
                for file in [Some(&path), data.as_ref()].into_iter().flatten() {
                    let _ = std::fs::remove_file(file);
                }
                finish_download(app, done.err().map(|e| (model, e)));
            }
            AiEvent::Progress { model, received } => {
                if let Some(d) = app.ai.downloading.as_mut().filter(|d| d.0.id == model.id) {
                    d.1 = received;
                }
            }
            AiEvent::DownloadFailed { model, error } => finish_download(app, Some((model, error))),
            AiEvent::Inferred { ticket, output } => {
                let Some(task) = app.ai.task.take_if(|t| t.ticket == ticket) else { continue };
                let model = task.model;
                if let Err(e) = output.and_then(|output| answered(app, task, output)) {
                    crate::notices::error(app, crate::i18n::fmt(tl!("The {model} model failed: {error}"), &[("model", model.name), ("error", &e)]));
                }
            }
        }
    }
    if app.ai.downloading.is_some() || app.ai.task.is_some() {
        // Progress arrives between frames; keep drawing while something runs.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// A download ended: with an error, or (once its model is installed) by running what waited.
fn finish_download(app: &mut PhotocraftApp, failed: Option<(&'static Model, String)>) {
    match failed {
        Some((model, error)) => {
            if app.ai.downloading.take_if(|d| d.0.id == model.id).is_some() {
                crate::notices::error(
                    app,
                    crate::i18n::fmt(tl!("The {model} model could not be downloaded: {error}"), &[("model", model.name), ("error", &error)]),
                );
            }
        }
        None => {
            if let Some((_, _, then)) = app.ai.downloading.take_if(|d| ai_cmds::is_installed(&app.session, d.0)) {
                run_then(app, then);
            }
        }
    }
}

fn start_download(app: &mut PhotocraftApp, model: &'static Model, then: Then) {
    let Some(svc) = app.services.ai.as_mut() else { return };
    (svc.download)(model);
    app.ai.downloading = Some((model, 0, then));
}

/// The lines every download prompt shows: what, how big, under which licence, from where.
fn model_facts(ui: &mut egui::Ui, m: &Model) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(m.name).font(crate::theme::semibold(14.0)));
    ui.label(tl!(m.blurb));
    ui.add_space(4.0);
    let host = m.url.split('/').nth(2).unwrap_or(m.url);
    for (label, value) in [(tl!("Download size"), m.size_label()), (tl!("License"), m.license.to_string()), (tl!("Downloaded from"), host.to_string())] {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{label}:")).color(t.text_dim));
            ui.label(value);
        });
    }
}

/// Draw the AI dialogs that are open.
pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    show_offer(app, ctx);
    show_progress(app, ctx);
    show_manager(app, ctx);
}

fn show_offer(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some((model, _)) = app.ai.offer.as_ref().map(|o| (o.0, ())) else { return };
    let mut choice = None;
    egui::Window::new(tl!("Download an AI model?"))
        .id(egui::Id::unique("ai-model-offer"))
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            model_facts(ui, model);
            ui.add_space(6.0);
            ui.label(tl!("The model runs on this device. Your images are not uploaded. Without it, PhotoCraft uses its built-in method."));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let buttons = [
                    DialogButton::new(ButtonRole::Default, tl!("Download"), 110.0),
                    DialogButton::new(ButtonRole::Alternate, tl!("Use Built-in Method"), 150.0),
                    DialogButton::new(ButtonRole::Cancel, tl!("Cancel"), 80.0),
                ];
                choice = crate::widgets::dialog_buttons(ui, &buttons);
            });
        });
    if let Some(choice) = choice {
        answer_offer(app, choice);
    }
}

/// The user's answer to the download prompt.
fn answer_offer(app: &mut PhotocraftApp, choice: ButtonRole) {
    let Some((model, then)) = app.ai.offer.take() else { return };
    match choice {
        ButtonRole::Default => start_download(app, model, then),
        ButtonRole::Alternate => {
            app.ai.declined.push(model.purpose);
            // The command as it was asked for, with the classical method.
            run_then(
                app,
                then.map(|(id, mut p)| {
                    if let Some(o) = p.as_object_mut() {
                        o.insert("model".into(), json!("classical"));
                    }
                    (id, p)
                }),
            );
        }
        _ => {}
    }
}

fn show_progress(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let (title, text, fraction) = match (&app.ai.downloading, &app.ai.task) {
        (Some((m, received, _)), _) => {
            let mb = |b: u64| format!("{:.1}", b as f64 / (1024.0 * 1024.0));
            let text = crate::i18n::fmt(tl!("{done} of {total} MB"), &[("done", &mb(*received)), ("total", &mb(m.total_bytes()))]);
            let fraction = (*received as f32 / m.total_bytes().max(1) as f32).clamp(0.0, 1.0);
            (crate::i18n::fmt(tl!("Downloading {model}"), &[("model", m.name)]), text, Some(fraction))
        }
        (None, Some(task)) => {
            let (text, fraction) = match &task.work {
                Work::Subject(_) => (tl!("Finding the subject on this device…"), None),
                Work::Upscale(job) if job.model().purpose == Purpose::Denoise => (tl!("Removing noise on this device…"), Some(job.progress())),
                Work::Upscale(job) => (tl!("Enlarging on this device…"), Some(job.progress())),
                Work::Inpaint(_) => (tl!("Filling on this device…"), None),
            };
            (crate::i18n::fmt(tl!("Running {model}"), &[("model", task.model.name)]), text.to_string(), fraction)
        }
        (None, None) => return,
    };
    let mut cancel = false;
    egui::Window::new(title)
        .id(egui::Id::unique("ai-progress"))
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -64.0))
        .show(ctx, |ui| {
            ui.label(text);
            match fraction {
                Some(f) => ui.add(egui::ProgressBar::new(f).desired_width(320.0)),
                None => ui.add(egui::Spinner::new()),
            };
            ui.add_space(6.0);
            cancel = crate::widgets::secondary_button(ui, tl!("Cancel"), 80.0).clicked();
        });
    if cancel {
        cancel_running(app);
    }
}

/// Cancel the download or the model run in progress.
fn cancel_running(app: &mut PhotocraftApp) {
    let Some(svc) = app.services.ai.as_mut() else { return };
    if app.ai.downloading.take().is_some() {
        (svc.cancel_download)();
    } else if app.ai.task.take().is_some() {
        (svc.cancel_infer)();
    }
}

fn show_manager(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ai.manager_open {
        return;
    }
    let t = crate::theme::Tokens::get(ctx);
    let mut open = true;
    let (mut download, mut remove) = (None, None);
    let busy = app.ai.downloading.is_some();
    let can_download = app.services.ai.is_some();
    let list_h = (ctx.content_rect().height() - 230.0).max(120.0);
    egui::Window::new(tl!("AI Models"))
        .id(egui::Id::unique("ai-models"))
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .show(ctx, |ui| {
            ui.label(tl!("AI models run on this device. None is included: each is downloaded once, when you ask for it."));
            // The list scrolls: with every model it is taller than a small window.
            egui::ScrollArea::vertical().max_height(list_h).min_scrolled_height(list_h).show(ui, |ui| {
                for m in MODELS {
                    ui.add_space(8.0);
                    crate::widgets::hairline(ui);
                    ui.add_space(8.0);
                    model_facts(ui, m);
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ai_cmds::is_installed(&app.session, m) {
                            ui.label(egui::RichText::new(tl!("Installed")).color(t.accent_text));
                            if crate::widgets::secondary_button(ui, tl!("Remove"), 90.0).clicked() {
                                remove = Some(m);
                            }
                        } else if ui.add_enabled(can_download && !busy, egui::Button::new(tl!("Download"))).clicked() {
                            download = Some(m);
                        }
                    });
                }
            });
        });
    app.ai.manager_open = open;
    if let Some(m) = download {
        start_download(app, m, None);
    }
    if let Some(m) = remove {
        match app.services.ai.as_mut().and_then(|s| s.remove.as_mut()) {
            Some(platform) => platform(m),
            None => {
                if let Err(e) = app.session.execute("ai.model.remove", json!({"id": m.id})) {
                    crate::notices::error(app, e.to_string());
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "ai_ui_tests.rs"]
mod tests;
