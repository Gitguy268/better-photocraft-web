//! Fonts installed on the visitor's device, through the Local Font Access API
//! (`window.queryLocalFonts`, Chromium browsers). The app otherwise has two fonts of its own plus
//! whatever the host serves (`fonts/manifest.txt`).
//!
//! The browser shows a permission prompt and needs a user gesture, so this runs only when the user
//! asks (Type › Use Fonts from This Device). The families then appear in the font menus through
//! the served-fonts path (`photocraft_ui_egui::served_fonts`), and a family's files are read from
//! the device only when it is first picked or a document's text needs it. Nothing is uploaded.

use std::cell::RefCell;
use std::collections::BTreeMap;

use js_sys::{Array, Function, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

thread_local! {
    /// The device's fonts by family: the browser's `FontData` objects, read when needed.
    static FONTS: RefCell<BTreeMap<String, Vec<JsValue>>> = const { RefCell::new(BTreeMap::new()) };
    /// What to tell the user about the last request.
    static MESSAGES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn js(e: JsValue) -> String {
    Reflect::get(&e, &"message".into()).ok().and_then(|m| m.as_string()).unwrap_or_else(|| format!("{e:?}"))
}

fn query_function() -> Option<(web_sys::Window, Function)> {
    let window = web_sys::window()?;
    let f = Reflect::get(&window, &"queryLocalFonts".into()).ok()?.dyn_into::<Function>().ok()?;
    Some((window, f))
}

/// Does this browser have the API?
pub fn supported() -> bool {
    query_function().is_some()
}

/// Ask the browser for the device's fonts and list their families in the font menus.
pub fn request(ctx: egui::Context) {
    wasm_bindgen_futures::spawn_local(async move {
        use photocraft_ui_egui::i18n;
        let message = match query().await {
            Ok(0) => i18n::t("This device offered no fonts.").to_string(),
            Ok(n) => i18n::fmt(i18n::t("Font families added from this device: {count}"), &[("count", &n.to_string())]),
            Err(e) => i18n::fmt(i18n::t("The device's fonts could not be read: {error}"), &[("error", &e)]),
        };
        MESSAGES.with_borrow_mut(|m| m.push(message));
        ctx.request_repaint();
    });
}

async fn query() -> Result<usize, String> {
    let (window, f) = query_function().ok_or("this browser has no access to local fonts")?;
    let promise: js_sys::Promise = f.call0(&window).map_err(js)?.dyn_into().map_err(|_| "unexpected answer".to_string())?;
    // A refused permission arrives here as a rejection.
    let list: Array = JsFuture::from(promise).await.map_err(js)?.dyn_into().map_err(|_| "unexpected answer".to_string())?;
    let mut families: BTreeMap<String, Vec<JsValue>> = BTreeMap::new();
    for font in list.iter() {
        if let Some(family) = Reflect::get(&font, &"family".into()).ok().and_then(|v| v.as_string()).filter(|f| !f.is_empty()) {
            families.entry(family).or_default().push(font);
        }
    }
    let count = families.len();
    photocraft_ui_egui::served_fonts::add_families(families.keys().cloned());
    FONTS.with_borrow_mut(|f| *f = families);
    Ok(count)
}

/// The device fonts of `family`, if it is one of them (taken: each is read once).
pub fn take(family: &str) -> Option<Vec<JsValue>> {
    FONTS.with_borrow_mut(|f| f.remove(family))
}

/// Read one font's file.
pub async fn bytes(font: JsValue) -> Result<Vec<u8>, String> {
    let blob_fn: Function = Reflect::get(&font, &"blob".into()).map_err(js)?.dyn_into().map_err(|_| "no font data".to_string())?;
    let promise: js_sys::Promise = blob_fn.call0(&font).map_err(js)?.dyn_into().map_err(|_| "no font data".to_string())?;
    let blob: web_sys::Blob = JsFuture::from(promise).await.map_err(js)?.dyn_into().map_err(|_| "no font data".to_string())?;
    let buffer = JsFuture::from(blob.array_buffer()).await.map_err(js)?;
    Ok(Uint8Array::new(&buffer).to_vec())
}

pub fn take_messages() -> Vec<String> {
    MESSAGES.with_borrow_mut(std::mem::take)
}
