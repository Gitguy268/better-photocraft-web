//! PhotoCraft's AI worker for the browser.
//!
//! The app's main wasm has no room for a model runtime under its size gate and must never block
//! the UI thread, so inference lives here: a second wasm, loaded on demand in a Web Worker
//! (`docs/decisions.md`). The page posts `{id, model, cache, input}`; the worker loads the model
//! from the browser's Cache Storage the first time (the page downloaded and verified it there),
//! runs it, and posts back `{id, output}` or `{id, error}`. Cancelling is the page terminating
//! the worker.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_arch = "wasm32")]
mod worker {
    use std::cell::RefCell;
    use std::rc::Rc;

    use js_sys::{Float32Array, Object, Reflect, Uint8Array};
    use photocraft_ml::runtime::Runtime;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

    fn get(msg: &JsValue, key: &str) -> JsValue {
        Reflect::get(msg, &key.into()).unwrap_or(JsValue::UNDEFINED)
    }

    fn reply(id: &JsValue, key: &str, value: &JsValue) -> JsValue {
        let o = Object::new();
        let _ = Reflect::set(&o, &"id".into(), id);
        let _ = Reflect::set(&o, &key.into(), value);
        o.into()
    }

    /// The model file from the cache the page filled.
    async fn cached(scope: &DedicatedWorkerGlobalScope, cache: &str, url: &str) -> Result<Vec<u8>, String> {
        let js = |e: JsValue| format!("{e:?}");
        let storage = scope.caches().map_err(js)?;
        let cache: web_sys::Cache = JsFuture::from(storage.open(cache)).await.map_err(js)?.dyn_into().map_err(js)?;
        let found = JsFuture::from(cache.match_with_str(url)).await.map_err(js)?;
        let resp: web_sys::Response = found.dyn_into().map_err(|_| "the model is not in the browser's cache; download it again".to_string())?;
        let buf = JsFuture::from(resp.array_buffer().map_err(js)?).await.map_err(js)?;
        Ok(Uint8Array::new(&buf).to_vec())
    }

    async fn handle(scope: &DedicatedWorkerGlobalScope, rt: &RefCell<Runtime>, msg: &JsValue) -> Result<Float32Array, String> {
        let model = photocraft_ml::find(&get(msg, "model").as_string().unwrap_or_default()).map_err(|e| e.to_string())?;
        let input = get(msg, "input").dyn_into::<Float32Array>().map_err(|_| "the request has no input".to_string())?;
        if !rt.borrow().is_loaded(model) {
            let cache = get(msg, "cache").as_string().unwrap_or_default();
            let bytes = cached(scope, &cache, model.url).await?;
            let data = match model.data {
                Some(d) => Some(cached(scope, &cache, d.url).await?),
                None => None,
            };
            rt.borrow_mut().load(model, &bytes, data.as_deref()).map_err(|e| e.to_string())?;
        }
        let tensor = photocraft_ml::Tensor { shape: model.input_shape(), data: input.to_vec() };
        let out = rt.borrow().run(model, tensor).map_err(|e| e.to_string())?;
        Ok(Float32Array::from(out.data.as_slice()))
    }

    pub fn start() {
        let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
        let rt = Rc::new(RefCell::new(Runtime::new()));
        let outer = scope.clone();
        let on = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let (scope, rt, msg) = (outer.clone(), rt.clone(), e.data());
            wasm_bindgen_futures::spawn_local(async move {
                let id = get(&msg, "id");
                let answer = match handle(&scope, &rt, &msg).await {
                    Ok(output) => reply(&id, "output", &output),
                    Err(error) => reply(&id, "error", &error.into()),
                };
                let _ = scope.post_message(&answer);
            });
        });
        scope.set_onmessage(Some(on.as_ref().unchecked_ref()));
        on.forget();
        // The wasm loads asynchronously: the page sends work only after this.
        let _ = scope.post_message(&reply(&JsValue::NULL, "ready", &true.into()));
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {
    worker::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("photocraft-ml-worker only runs in the browser: it is built by `trunk build --release` in apps/photocraft-web");
}
