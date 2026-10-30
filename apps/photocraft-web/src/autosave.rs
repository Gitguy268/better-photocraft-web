//! Crash recovery in the browser: the desktop's autosave services (`Services::autosave` and
//! friends, driven by `prefs_ui`) on IndexedDB.
//!
//! One entry per document, replaced by each autosave: a `.pcraft` bundle in the `data` store and
//! its [`RecoveryInfo`] (JSON) in the `meta` store, under the same key, written in one
//! transaction. An entry goes when its document is saved or closed. Start-up reads only the small
//! `meta` store; the bundles are read when the user chooses to restore the last session.

use photocraft_format::autosave::RecoveryInfo;

/// Entries kept at most. Open documents and entries waiting for the user's answer are never
/// dropped to get there; what goes first is the oldest leftover of an earlier session.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub const MAX_ENTRIES: usize = 8;

/// An entry's key. Document ids restart with every page load, so the key carries the session:
/// a new session's first autosave can't overwrite an entry the last one left behind.
pub fn entry_key(session: &str, doc: u64) -> String {
    format!("{session}-{doc}")
}

pub fn encode_meta(info: &RecoveryInfo) -> String {
    serde_json::to_string(info).unwrap_or_default()
}

/// The record stored under `key`; `None` when it is unreadable or belongs to another key.
pub fn decode_meta(key: &str, text: &str) -> Option<RecoveryInfo> {
    serde_json::from_str::<RecoveryInfo>(text).ok().filter(|info| info.key == key)
}

/// The keys to delete so that at most `max` of `entries` (key, saved-at) stay: oldest first,
/// never a key in `live`.
pub fn prune(entries: &[(String, u64)], live: &[String], max: usize) -> Vec<String> {
    let mut old: Vec<&(String, u64)> = entries.iter().filter(|(key, _)| !live.contains(key)).collect();
    old.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let excess = entries.len().saturating_sub(max).min(old.len());
    old.into_iter().take(excess).map(|(key, _)| key.clone()).collect()
}

/// A storage error as the status bar shows it ("Autosave failed: …").
pub fn readable(error: &str) -> String {
    if error.contains("QuotaExceeded") {
        "the browser's storage for this site is full. Save the document to a file to keep your work.".into()
    } else {
        error.to_string()
    }
}

#[cfg(target_arch = "wasm32")]
pub use browser::{Recovery, open};

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::Arc;

    use js_sys::{Array, Uint8Array};
    use photocraft_doc::Document;
    use photocraft_format::autosave::RecoveryInfo;
    use photocraft_ui_egui::{Recoverable, Services};
    use wasm_bindgen::{JsCast, JsValue};
    use web_sys::{Blob, IdbDatabase, IdbTransaction, IdbTransactionMode};

    use super::{MAX_ENTRIES, decode_meta, encode_meta, entry_key, prune, readable};
    use crate::indexed_presets::{before, committed, js, open_database, request};

    const DATABASE: &str = "photocraft.recovery";
    const META: &str = "meta";
    const DATA: &str = "data";
    const TIMEOUT_MS: f64 = 10_000.0;

    /// A finished write: (document id, revision, result), as `Services::autosave_results` reports it.
    type Outcome = (u64, u64, Result<(), String>);

    struct Inner {
        db: IdbDatabase,
        session: String,
        /// Open documents' entries: `DocId` value to key.
        keys: RefCell<HashMap<u64, String>>,
        /// Every stored entry: key to saved-at.
        index: RefCell<HashMap<String, u64>>,
        results: RefCell<Vec<Outcome>>,
        /// Entries of earlier sessions the user hasn't answered about yet.
        found: RefCell<Vec<RecoveryInfo>>,
        /// Their bundles once the user chose Restore, for the next `Services::recover` call.
        restored: RefCell<Vec<(RecoveryInfo, Vec<u8>)>>,
        ready: Cell<bool>,
    }

    /// The recovery store of this page load.
    #[derive(Clone)]
    pub struct Recovery(Rc<Inner>);

    fn deadline() -> f64 {
        js_sys::Date::now() + TIMEOUT_MS
    }

    fn transaction(db: &IdbDatabase, mode: IdbTransactionMode) -> Result<IdbTransaction, String> {
        let stores = Array::of2(&JsValue::from_str(META), &JsValue::from_str(DATA));
        db.transaction_with_str_sequence_and_mode(&stores, mode).map_err(js)
    }

    /// Opens the store and lists what earlier sessions left behind. `Err` (private browsing, storage
    /// blocked, a time-out) means this session runs without autosave.
    pub async fn open() -> Result<Recovery, String> {
        let deadline = deadline();
        let db = open_database(DATABASE, &[META, DATA], deadline).await?;
        let tx = db.transaction_with_str_and_mode(META, IdbTransactionMode::Readonly).map_err(js)?;
        let store = tx.object_store(META).map_err(js)?;
        let (keys, values) = (request(&store.get_all_keys().map_err(js)?), request(&store.get_all().map_err(js)?));
        let keys = Array::from(&before(&keys.promise, deadline).await?);
        let values = Array::from(&before(&values.promise, deadline).await?);
        let mut found: Vec<RecoveryInfo> =
            keys.iter().zip(values.iter()).filter_map(|(key, value)| decode_meta(&key.as_string()?, &value.as_string()?)).collect();
        found.sort_by_key(|info| info.saved_at);
        let index = found.iter().map(|info| (info.key.clone(), info.saved_at)).collect();
        Ok(Recovery(Rc::new(Inner {
            db,
            // Milliseconds since the epoch: unique enough across this profile's page loads.
            session: format!("{:x}", js_sys::Date::now() as u64),
            keys: RefCell::default(),
            index: RefCell::new(index),
            results: RefCell::default(),
            found: RefCell::new(found),
            restored: RefCell::default(),
            ready: Cell::new(false),
        })))
    }

    async fn put(db: &IdbDatabase, info: &RecoveryInfo, bytes: &[u8]) -> Result<(), String> {
        let tx = transaction(db, IdbTransactionMode::Readwrite)?;
        let queued = (|| -> Result<(), String> {
            let key = JsValue::from_str(&info.key);
            let blob = Blob::new_with_u8_array_sequence(&Array::of1(&Uint8Array::from(bytes))).map_err(js)?;
            tx.object_store(DATA).map_err(js)?.put_with_key(&blob, &key).map_err(js)?;
            tx.object_store(META).map_err(js)?.put_with_key(&JsValue::from_str(&encode_meta(info)), &key).map_err(js)?;
            Ok(())
        })();
        if let Err(e) = queued {
            let _ = tx.abort();
            return Err(e);
        }
        committed(&tx).await
    }

    async fn delete(db: &IdbDatabase, keys: &[String]) -> Result<(), String> {
        let tx = transaction(db, IdbTransactionMode::Readwrite)?;
        for key in keys {
            let key = JsValue::from_str(key);
            for store in [META, DATA] {
                tx.object_store(store).map_err(js)?.delete(&key).map_err(js)?;
            }
        }
        committed(&tx).await
    }

    async fn read(db: &IdbDatabase, key: &str) -> Result<Vec<u8>, String> {
        let deadline = deadline();
        let tx = db.transaction_with_str_and_mode(DATA, IdbTransactionMode::Readonly).map_err(js)?;
        let pending = request(&tx.object_store(DATA).map_err(js)?.get(&JsValue::from_str(key)).map_err(js)?);
        let blob: Blob = before(&pending.promise, deadline).await?.dyn_into().map_err(|_| "the recovery data is missing")?;
        let buffer = before(&blob.array_buffer(), deadline).await?;
        Ok(Uint8Array::new(&buffer).to_vec())
    }

    impl Recovery {
        fn forget(&self, keys: Vec<String>) {
            if keys.is_empty() {
                return;
            }
            self.0.index.borrow_mut().retain(|key, _| !keys.contains(key));
            let inner = self.0.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = delete(&inner.db, &keys).await {
                    log::warn!("could not delete recovery data: {e}");
                }
            });
        }

        /// Open documents' entries and the ones still offered: never pruned.
        fn live(&self) -> Vec<String> {
            let mut live: Vec<String> = self.0.keys.borrow().values().cloned().collect();
            live.extend(self.0.found.borrow().iter().map(|info| info.key.clone()));
            live.extend(self.0.restored.borrow().iter().map(|(info, _)| info.key.clone()));
            live
        }

        /// Names of the documents earlier sessions left unsaved, oldest first.
        pub fn found(&self) -> Vec<String> {
            self.0.found.borrow().iter().map(|info| info.document_name.clone()).collect()
        }

        /// The user chose Restore: read the bundles, then [`Self::take_ready`] turns true once.
        pub fn restore(&self, ctx: egui::Context) {
            let found = std::mem::take(&mut *self.0.found.borrow_mut());
            let this = self.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let mut unreadable = Vec::new();
                for info in found {
                    match read(&this.0.db, &info.key).await {
                        Ok(bytes) => this.0.restored.borrow_mut().push((info, bytes)),
                        Err(e) => {
                            log::error!("could not read the recovery data of {}: {e}", info.document_name);
                            unreadable.push(info.key);
                        }
                    }
                }
                this.forget(unreadable);
                this.0.ready.set(true);
                ctx.request_repaint();
            });
        }

        /// True once after [`Self::restore`] finished reading: time to ask `Services::recover` again.
        pub fn take_ready(&self) -> bool {
            self.0.ready.replace(false)
        }

        /// The user chose Discard: the offered entries are deleted.
        pub fn discard_found(&self) {
            let keys = std::mem::take(&mut *self.0.found.borrow_mut()).into_iter().map(|info| info.key).collect();
            self.forget(keys);
        }

        /// Recovery at launch is switched off: nothing is offered, and the entries count as leftovers.
        pub fn ignore_found(&self) {
            self.0.found.borrow_mut().clear();
        }

        /// The five recovery services, as the desktop's `recovery_services`.
        pub fn install(&self, services: &mut Services, ctx: &egui::Context) {
            let (this, wake) = (self.clone(), ctx.clone());
            services.autosave = Some(Box::new(move |doc: &Arc<Document>, revision: u64, path: Option<&str>| {
                let id = doc.id.0;
                // shortcut: encodes on the UI thread (the browser build has no worker threads), so a
                // very large document pauses the app for the encode; move to a worker when one exists.
                let bytes = photocraft_format::save_to_bytes(doc, &photocraft_format::SaveOptions::default()).map_err(|e| e.to_string())?;
                let key = this.0.keys.borrow_mut().entry(id).or_insert_with(|| entry_key(&this.0.session, id)).clone();
                let info = RecoveryInfo {
                    key,
                    document_name: doc.name.clone(),
                    original_path: path.map(str::to_string),
                    saved_at: (js_sys::Date::now() / 1000.0) as u64,
                    revision,
                };
                let (this, wake) = (this.clone(), wake.clone());
                wasm_bindgen_futures::spawn_local(async move {
                    let result = put(&this.0.db, &info, &bytes).await.map_err(|e| readable(&e));
                    // Saved or closed meanwhile: its delete is queued behind this write.
                    if result.is_ok() && this.0.keys.borrow().get(&id) == Some(&info.key) {
                        this.0.index.borrow_mut().insert(info.key.clone(), info.saved_at);
                        let entries: Vec<(String, u64)> = this.0.index.borrow().iter().map(|(key, at)| (key.clone(), *at)).collect();
                        this.forget(prune(&entries, &this.live(), MAX_ENTRIES));
                    }
                    this.0.results.borrow_mut().push((id, revision, result));
                    wake.request_repaint();
                });
                Ok(())
            }));
            let this = self.clone();
            services.autosave_results = Some(Box::new(move || std::mem::take(&mut *this.0.results.borrow_mut())));
            let this = self.clone();
            services.discard_autosave = Some(Box::new(move |id: u64| {
                let key = this.0.keys.borrow_mut().remove(&id);
                this.forget(key.into_iter().collect());
            }));
            let this = self.clone();
            services.recover = Some(Box::new(move || {
                std::mem::take(&mut *this.0.restored.borrow_mut())
                    .into_iter()
                    .map(|(info, bytes)| Recoverable {
                        key: info.key,
                        name: info.document_name,
                        path: info.original_path,
                        load: Box::new(move || photocraft_format::load_from_bytes(&bytes).map_err(|e| e.to_string())),
                    })
                    .collect()
            }));
            let this = self.clone();
            services.adopt_autosave = Some(Box::new(move |id: u64, key: &str| {
                this.0.keys.borrow_mut().insert(id, key.to_string());
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(key: &str) -> RecoveryInfo {
        RecoveryInfo {
            key: key.into(),
            document_name: "Pearl \"final\".psd".into(),
            original_path: Some("pearl.psd".into()),
            saved_at: 1_760_000_000,
            revision: 7,
        }
    }

    #[test]
    fn keys_differ_between_sessions_for_the_same_document_id() {
        assert_eq!(entry_key("19a2b", 1), "19a2b-1");
        assert_ne!(entry_key("19a2b", 1), entry_key("19a2c", 1));
        assert_ne!(entry_key("19a2b", 1), entry_key("19a2b", 11));
    }

    #[test]
    fn meta_round_trips_and_rejects_foreign_or_broken_records() {
        let record = encode_meta(&info("s-1"));
        assert_eq!(decode_meta("s-1", &record), Some(info("s-1")));
        assert_eq!(decode_meta("s-2", &record), None, "a record stored under another key");
        for broken in ["", "{", "[]", "{\"key\":\"s-1\"}", "null"] {
            assert_eq!(decode_meta("s-1", broken), None, "{broken}");
        }
    }

    #[test]
    fn pruning_drops_the_oldest_leftovers_and_never_a_live_entry() {
        let entries: Vec<(String, u64)> = [("a", 5), ("b", 1), ("c", 3), ("d", 2), ("e", 4)].map(|(k, at)| (k.to_string(), at)).into();
        assert!(prune(&entries, &[], 5).is_empty());
        assert_eq!(prune(&entries, &[], 3), ["b", "d"]);
        // "b" is an open document: the next oldest goes instead.
        assert_eq!(prune(&entries, &["b".to_string()], 3), ["d", "c"]);
        // Everything is live: nothing to drop, even over the cap.
        let live: Vec<String> = entries.iter().map(|(k, _)| k.clone()).collect();
        assert!(prune(&entries, &live, 1).is_empty());
        assert_eq!(prune(&entries, &[], 0).len(), 5);
        assert!(prune(&[], &[], 0).is_empty());
    }

    #[test]
    fn a_full_store_is_reported_in_plain_words() {
        assert!(readable("QuotaExceededError: The quota has been exceeded.").contains("storage for this site is full"));
        assert_eq!(readable("TransactionInactiveError: x"), "TransactionInactiveError: x");
    }
}
