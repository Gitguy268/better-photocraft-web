//! Open and save through the File System Access API (`showOpenFilePicker` /
//! `showSaveFilePicker`), where the browser has it: File › Open keeps the file's handle, so
//! File › Save writes back to the same file, and Save As shows the browser's save dialog.
//! Browsers without the API (Firefox, Safari, Brave by default) keep the upload dialog and
//! downloads; so does any call the browser refuses.
//!
//! The app names files by path, and a page never learns one. A handle is kept under a *token*,
//! the file's name (made unique when two different files share it), which the app carries as
//! the document's path and hands back to `Services::write`.
//!
//! The API is reached through `js_sys::Reflect`: its web-sys bindings need
//! `web_sys_unstable_apis`, which this build doesn't turn on.

/// The token of the `n`th different file called `name` (`n` ≥ 2): `art (2).psd`.
pub fn numbered(name: &str, n: usize) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

/// Extensions as a picker's `accept` list takes them: `.ext`, only the ones the API allows
/// (ASCII letters and digits, at most 16 characters), without repeats.
pub fn accept_extensions<S: AsRef<str>>(extensions: &[S]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for ext in extensions {
        let ext = ext.as_ref().trim_start_matches('.').to_ascii_lowercase();
        let dotted = format!(".{ext}");
        if !ext.is_empty() && ext.len() <= 16 && ext.bytes().all(|b| b.is_ascii_alphanumeric()) && !out.contains(&dotted) {
            out.push(dotted);
        }
    }
    out
}

#[cfg(target_arch = "wasm32")]
pub use browser::{open, pick_save, set_context, supported, take_failures, write};

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
    use photocraft_engine::file_cmds::saves_in_place;
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;

    use super::{accept_extensions, numbered};
    use crate::indexed_presets::js;

    #[derive(Default)]
    struct State {
        /// File handles by token.
        handles: HashMap<String, JsValue>,
        /// Writes that failed after `Services::write` returned: (token, why).
        failures: Vec<(String, String)>,
        ctx: Option<egui::Context>,
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::default();
    }

    /// The context to repaint when a write fails.
    pub fn set_context(ctx: &egui::Context) {
        STATE.with(|s| s.borrow_mut().ctx = Some(ctx.clone()));
    }

    fn method(target: &JsValue, name: &str) -> Option<Function> {
        Reflect::get(target, &JsValue::from_str(name)).ok()?.dyn_into().ok()
    }

    /// Calls `target.name(...args)` and awaits the promise it returns.
    async fn call(target: &JsValue, name: &str, args: &[&JsValue]) -> Result<JsValue, JsValue> {
        let f = method(target, name).ok_or_else(|| JsValue::from_str(&format!("{name} is not available")))?;
        let promise: Promise = f.apply(target, &args.iter().copied().collect::<Array>())?.dyn_into()?;
        JsFuture::from(promise).await
    }

    fn set(object: &Object, key: &str, value: &JsValue) {
        let _ = Reflect::set(object, &JsValue::from_str(key), value);
    }

    /// The user closed the picker: not an error.
    fn aborted(e: &JsValue) -> bool {
        Reflect::get(e, &JsValue::from_str("name")).ok().and_then(|v| v.as_string()).as_deref() == Some("AbortError")
    }

    fn name_of(handle: &JsValue) -> String {
        Reflect::get(handle, &JsValue::from_str("name")).ok().and_then(|v| v.as_string()).unwrap_or_else(|| "untitled".into())
    }

    /// One entry of a picker's `types`.
    fn file_type(description: &str, mime: &str, extensions: &[String]) -> Array {
        let accept = Object::new();
        set(&accept, mime, &extensions.iter().map(|e| JsValue::from_str(e)).collect::<Array>());
        let ty = Object::new();
        set(&ty, "description", &JsValue::from_str(description));
        set(&ty, "accept", &accept);
        Array::of1(&ty)
    }

    /// Whether this browser, in this context, has both pickers.
    pub fn supported() -> bool {
        web_sys::window().is_some_and(|w| method(&w, "showOpenFilePicker").is_some() && method(&w, "showSaveFilePicker").is_some())
    }

    /// Keeps `handle` and returns its token: the file's name, or a numbered one when another
    /// file of that name is already held (two `art.psd` from different folders must never save
    /// into each other).
    async fn register(handle: JsValue) -> String {
        let name = name_of(&handle);
        let mut n = 1;
        loop {
            let token = if n == 1 { name.clone() } else { numbered(&name, n) };
            let held = STATE.with(|s| s.borrow().handles.get(&token).cloned());
            let same = match held {
                None => true,
                Some(held) => call(&held, "isSameEntry", &[&handle]).await.ok().and_then(|v| v.as_bool()).unwrap_or(false),
            };
            if same {
                STATE.with(|s| s.borrow_mut().handles.insert(token.clone(), handle));
                return token;
            }
            n += 1;
        }
    }

    /// One picked file: its name, the token to save back to (only for a kept handle), its bytes.
    pub type Opened = (String, Option<String>, Vec<u8>);

    /// Shows the open picker. `Ok(None)`: the user cancelled. `Err`: the browser refused (the
    /// caller falls back to the upload dialog). With `keep`, layered documents keep their handle
    /// and get a token, so Save writes back to them.
    pub async fn open(multiple: bool, extensions: &[String], keep: bool) -> Result<Option<Vec<Opened>>, String> {
        let window: JsValue = web_sys::window().ok_or("no window")?.into();
        let options = Object::new();
        set(&options, "multiple", &JsValue::from_bool(multiple));
        let extensions = accept_extensions(extensions);
        if !extensions.is_empty() {
            set(&options, "types", &file_type("Supported Files", "application/octet-stream", &extensions));
        }
        let handles = match call(&window, "showOpenFilePicker", &[&options]).await {
            Ok(handles) => Array::from(&handles),
            Err(e) if aborted(&e) => return Ok(None),
            Err(e) => return Err(js(e)),
        };
        let mut files = Vec::new();
        for handle in handles.iter() {
            let name = name_of(&handle);
            let bytes = async {
                let file: web_sys::Blob = call(&handle, "getFile", &[]).await?.dyn_into()?;
                Ok::<_, JsValue>(Uint8Array::new(&JsFuture::from(file.array_buffer()).await?).to_vec())
            };
            match bytes.await {
                Ok(bytes) => {
                    let token = if keep && saves_in_place(&name) { Some(register(handle).await) } else { None };
                    files.push((name, token, bytes));
                }
                Err(e) => log::error!("couldn't read {name}: {}", js(e)),
            }
        }
        Ok(Some(files))
    }

    /// Shows the save picker for `suggested` (a file name) and returns the chosen file's token.
    /// `Ok(None)`: the user cancelled. `Err`: the browser refused (the caller downloads).
    pub async fn pick_save(suggested: &str) -> Result<Option<String>, String> {
        let window: JsValue = web_sys::window().ok_or("no window")?.into();
        let options = Object::new();
        set(&options, "suggestedName", &JsValue::from_str(suggested));
        let extension = accept_extensions(&[suggested.rsplit_once('.').map_or("", |(_, ext)| ext)]);
        if let Some(ext) = extension.first() {
            set(&options, "types", &file_type(&ext.trim_start_matches('.').to_ascii_uppercase(), crate::web::mime_for(suggested), &extension));
        }
        match call(&window, "showSaveFilePicker", &[&options]).await {
            Ok(handle) => Ok(Some(register(handle).await)),
            Err(e) if aborted(&e) => Ok(None),
            Err(e) => Err(js(e)),
        }
    }

    async fn write_handle(handle: &JsValue, data: &JsValue) -> Result<(), String> {
        // A handle from the open picker is read-only until the user allows changes.
        if method(handle, "requestPermission").is_some() {
            let options = Object::new();
            set(&options, "mode", &JsValue::from_str("readwrite"));
            let state = call(handle, "requestPermission", &[&options]).await.map_err(js)?;
            if state.as_string().as_deref() != Some("granted") {
                return Err("the browser was not allowed to change the file".into());
            }
        }
        let writable = call(handle, "createWritable", &[]).await.map_err(js)?;
        // The browser writes to a temporary file and swaps it in on close: a failed write
        // leaves the file as it was.
        if let Err(e) = call(&writable, "write", &[data]).await {
            let _ = call(&writable, "abort", &[]).await;
            return Err(js(e));
        }
        call(&writable, "close", &[]).await.map(|_| ()).map_err(js)
    }

    /// Writes `bytes` to the file held under `path`. False when no handle is held for it (the
    /// caller downloads). The write finishes later; a failure arrives in [`take_failures`].
    pub fn write(path: &str, bytes: &[u8]) -> bool {
        // shortcut: an exact match, so a save dialog answer whose extension the app lower-cased
        // (`ART.PSD`) downloads instead; match case-insensitively if that turns out to matter.
        let Some(handle) = STATE.with(|s| s.borrow().handles.get(path).cloned()) else { return false };
        let data: JsValue = Uint8Array::from(bytes).into();
        let token = path.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            let result = write_handle(&handle, &data).await;
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                // Only a layered document saves again without a dialog; a flat file's handle has done its work.
                if !saves_in_place(&token) {
                    s.handles.remove(&token);
                }
                if let Err(e) = result {
                    s.failures.push((token, e));
                    if let Some(ctx) = &s.ctx {
                        ctx.request_repaint();
                    }
                }
            });
        });
        true
    }

    /// The writes that failed since the last call: (token, why).
    pub fn take_failures() -> Vec<(String, String)> {
        STATE.with(|s| std::mem::take(&mut s.borrow_mut().failures))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_file_of_the_same_name_gets_a_numbered_token() {
        assert_eq!(numbered("art.psd", 2), "art (2).psd");
        assert_eq!(numbered("my.art.v2.pcraft", 3), "my.art.v2 (3).pcraft");
        assert_eq!(numbered("noext", 2), "noext (2)");
        assert_eq!(numbered(".hidden", 2), ".hidden (2)");
        assert_eq!(numbered("", 2), " (2)");
    }

    #[test]
    fn accept_lists_hold_only_what_the_picker_allows() {
        assert_eq!(accept_extensions(&["PSD", ".png", "psd", "", "tar.gz", "a b", "averyveryverylongextension", "jpg"]), [".psd", ".png", ".jpg"]);
        assert!(accept_extensions::<&str>(&[]).is_empty());
    }
}
