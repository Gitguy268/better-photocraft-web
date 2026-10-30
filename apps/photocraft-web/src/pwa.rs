//! Installable and offline: registers the service worker (`sw.js`, the one hand-written script,
//! see `docs/decisions.md`) that caches the app's files as the page loads them. The web app
//! manifest is linked from `index.html`.
//!
//! Only in a secure context (https or localhost; browsers refuse service workers elsewhere),
//! and never with the `?nosw` URL flag, which also removes a worker registered earlier.

/// Whether the URL query (`?nosw`, `?webgl&nosw`, `?nosw=1`) has the flag `name`.
pub fn has_flag(query: &str, name: &str) -> bool {
    query.trim_start_matches('?').split('&').any(|part| part.split('=').next() == Some(name))
}

/// Whether to register the service worker for a page with this URL query.
pub fn wanted(secure_context: bool, query: &str) -> bool {
    secure_context && !has_flag(query, "nosw")
}

#[cfg(target_arch = "wasm32")]
pub fn register(query: &str) {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen_futures::JsFuture;
    let Some(window) = web_sys::window() else { return };
    let container = window.navigator().service_worker();
    // `navigator.serviceWorker` is undefined where the browser has none (insecure pages, some private windows).
    if container.is_undefined() {
        return;
    }
    let register = wanted(window.is_secure_context(), query);
    wasm_bindgen_futures::spawn_local(async move {
        let result = if register {
            // Relative: the site works under any path, and the worker's scope is its folder.
            let options = web_sys::RegistrationOptions::new();
            options.set_scope("./");
            JsFuture::from(container.register_with_options("sw.js", &options)).await.map(|_| ())
        } else {
            match JsFuture::from(container.get_registration()).await.map(|r| r.dyn_into::<web_sys::ServiceWorkerRegistration>()) {
                Ok(Ok(registration)) => match registration.unregister() {
                    Ok(promise) => JsFuture::from(promise).await.map(|_| ()),
                    Err(e) => Err(e),
                },
                // No registration (undefined): nothing to remove.
                Ok(Err(_)) => Ok(()),
                Err(e) => Err(e),
            }
        };
        if let Err(e) = result {
            log::warn!("photocraft-web: service worker: {e:?}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nosw_and_insecure_pages_get_no_service_worker() {
        assert!(wanted(true, ""));
        assert!(wanted(true, "?webgl"));
        assert!(!wanted(false, ""));
        for query in ["?nosw", "?webgl&nosw", "?nosw=1&cpu", "nosw"] {
            assert!(!wanted(true, query), "{query}");
        }
        // A flag is a whole key, not a substring.
        assert!(wanted(true, "?file=nosw.psd"));
        assert!(wanted(true, "?noswap"));
    }

    /// The worker never touches other origins, and the page's own files are found under any path.
    #[test]
    fn service_worker_rules_cover_the_site_files() {
        let sw = include_str!("../sw.js");
        assert!(sw.contains("url.origin !== self.location.origin"), "cross-origin requests are left alone");
        assert!(sw.contains("request.method !== 'GET'"));
        for name in ["index\\.html", "manifest\\.webmanifest", "fonts\\/manifest\\.txt", "ai-worker\\.txt", "app\\/photocraft-web\\.js", "app\\/parts\\.txt"] {
            assert!(sw.contains(name), "{name} is fetched network-first");
        }
        assert!(sw.contains("ai-worker-"), "the AI worker's checksum-named folder is cached like the hashed files");
        assert!(sw.contains("caches.delete(name)") && sw.contains("const CACHE = 'photocraft-v"), "old caches go on activate");
        assert!(sw.contains("name.startsWith(OWN)"), "but never the app's own caches (downloaded AI models)");
        assert!(!sw.contains("import") && !sw.contains("http"), "no dependencies, no absolute URLs");
        let manifest = include_str!("../manifest.webmanifest");
        let v: serde_json::Value = serde_json::from_str(manifest).unwrap();
        assert_eq!(v["name"], "PhotoCraft");
        assert_eq!((v["start_url"].as_str(), v["scope"].as_str()), (Some("./"), Some("./")), "relative, so any path works");
        let html = include_str!("../index.html");
        assert!(html.contains("<link rel=\"manifest\" href=\"manifest.webmanifest\">"));
        for icon in v["icons"].as_array().unwrap() {
            let src = icon["src"].as_str().unwrap();
            assert!(!src.starts_with('/') && !src.contains("://"), "{src}");
            let size = icon["sizes"].as_str().unwrap().split('x').next().unwrap();
            assert!(
                html.contains(&format!("hicolor/{size}x{size}/apps/ai.storyteller.photocraft.png\" data-target-path=\"icons/{size}\"")),
                "{src} is copied by trunk"
            );
        }
    }
}
