//! Downloads local AI models for the desktop app (see `photocraft_ui_egui::ai_ui`).
//!
//! The engine runs the models itself here; the platform's job is only to fetch a model from its
//! pinned URL when the user asks, on a worker thread, with progress and cancel. The files land
//! beside the installed models as `<id>.onnx.download` (and `<id>.data.download` for a model with a
//! separate weights file); the engine's `ai.model.install` checks their SHA-256 before they become
//! `<id>.onnx` and `<id>.data`.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};

use photocraft_ml::Model;
use photocraft_ui_egui::ai_ui::{AiEvent, AiService};

/// Where installed models live: `PHOTOCRAFT_MODEL_DIR`, else `models` in the settings folder.
pub fn model_dir() -> Option<PathBuf> {
    std::env::var_os("PHOTOCRAFT_MODEL_DIR").filter(|d| !d.is_empty()).map(PathBuf::from).or_else(|| crate::app_dirs::config_dir().map(|d| d.join("models")))
}

/// Fetch one file of `model` to `path`. `Ok(false)`: cancelled. `before` is what earlier files of
/// the model already counted towards its progress.
fn fetch_file(
    model: &'static Model,
    (url, bytes): (&str, u64),
    path: &Path,
    before: u64,
    events: &Sender<AiEvent>,
    cancel: &AtomicBool,
) -> Result<bool, String> {
    let mut response = ureq::get(url).header("User-Agent", "PhotoCraft").call().map_err(|e| e.to_string())?;
    let mut body = response.body_mut().as_reader();
    let mut file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0u8; 64 * 1024];
    let (mut received, mut reported) = (0u64, 0u64);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let n = match body.read(&mut buf) {
            Ok(0) => return Ok(true),
            Ok(n) => n,
            Err(e) => return Err(e.to_string()),
        };
        received += n as u64;
        // A wrong or endless file stops here, not when the disk is full.
        if received > bytes {
            return Err("the file is larger than the model should be".into());
        }
        file.write_all(buf.get(..n).unwrap_or(&[])).map_err(|e| format!("{}: {e}", path.display()))?;
        if received - reported >= model.total_bytes() / 100 {
            reported = received;
            let _ = events.send(AiEvent::Progress { model, received: before + received });
        }
    }
}

/// Fetch `model` into `dir`: its model file and, if it has one, its weights file. `Ok(None)`:
/// cancelled. Nothing is left behind unless every file arrived.
fn fetch(model: &'static Model, dir: &Path, events: &Sender<AiEvent>, cancel: &AtomicBool) -> Result<Option<(PathBuf, Option<PathBuf>)>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let paths = [dir.join(format!("{}.onnx.download", model.id)), dir.join(format!("{}.data.download", model.id))];
    let mut before = 0;
    let mut outcome = Ok(true);
    for ((url, _, bytes), path) in model.files().zip(&paths) {
        outcome = fetch_file(model, (url, bytes), path, before, events, cancel);
        if !matches!(outcome, Ok(true)) {
            break;
        }
        before += bytes;
    }
    let [graph, data] = paths;
    match outcome {
        Ok(true) => Ok(Some((graph, model.data.map(|_| data)))),
        other => {
            let _ = std::fs::remove_file(&graph);
            let _ = std::fs::remove_file(&data);
            other.map(|_| None)
        }
    }
}

/// The desktop's AI platform service: downloads only (`dir` is where models are kept).
pub fn service(dir: PathBuf) -> AiService {
    let (tx, rx) = mpsc::channel::<AiEvent>();
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    AiService {
        download: Box::new(move |model| {
            cancel.store(false, Ordering::Relaxed);
            let (tx, dir, cancel) = (tx.clone(), dir.clone(), cancel.clone());
            let failed = tx.clone();
            let spawned = std::thread::Builder::new().name("photocraft-model-download".into()).spawn(move || {
                let _ = match fetch(model, &dir, &tx, &cancel) {
                    Ok(Some((path, data))) => {
                        tx.send(AiEvent::Downloaded { model, path: path.display().to_string(), data: data.map(|d| d.display().to_string()) })
                    }
                    Ok(None) => Ok(()),
                    Err(error) => tx.send(AiEvent::DownloadFailed { model, error }),
                };
            });
            if let Err(e) = spawned {
                let _ = failed.send(AiEvent::DownloadFailed { model, error: format!("could not start the download: {e}") });
            }
        }),
        cancel_download: Box::new(move || stop.store(true, Ordering::Relaxed)),
        // Models are files the engine manages (`ai.model.remove`) and runs in its own jobs.
        remove: None,
        infer: None,
        cancel_infer: Box::new(|| {}),
        poll: Box::new(move || rx.try_iter().collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_download_leaves_no_file_and_reports_nothing() {
        let dir = std::env::temp_dir().join(format!("photocraft-ai-dl-{}", std::process::id()));
        let (tx, rx) = mpsc::channel();
        // Cancelled before the first byte: no network is needed past the connection.
        let cancel = AtomicBool::new(true);
        let model = &photocraft_ml::MODELS[0];
        match fetch(model, &dir, &tx, &cancel) {
            // Offline machines fail to connect instead; either way nothing is left behind.
            Ok(None) | Err(_) => {}
            Ok(Some((p, _))) => panic!("a cancelled download produced {}", p.display()),
        }
        assert!(!dir.join(format!("{}.onnx.download", model.id)).exists());
        assert!(rx.try_iter().next().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
