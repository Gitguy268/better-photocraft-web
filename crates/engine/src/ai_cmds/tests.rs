use super::*;
use photocraft_geom::Rect;

const W: u32 = 200;
const H: u32 = 160;

fn inside(x: f32, y: f32) -> bool {
    let (dx, dy) = (x - 100.0, y - 80.0);
    dx * dx + dy * dy <= 45.0 * 45.0
}

/// An orange disc (centre (100, 80), radius 45) on dark blue, as the Background layer.
fn disc(params: Value) -> Session {
    let mut s = Session::new();
    s.execute("file.new", params).unwrap();
    s.edit("paint", |doc, _| {
        let bg = doc.layers[0].surface_mut().unwrap();
        let n = bg.channels();
        let mut data = Vec::new();
        for y in 0..H {
            for x in 0..W {
                let c = if inside(x as f32 + 0.5, y as f32 + 0.5) { [0.9, 0.5, 0.1] } else { [0.05, 0.1, 0.3] };
                // Colour channels first (one for gray, four for CMYK), then alpha when there is one.
                let colour = if bg.format().alpha { n - 1 } else { n };
                data.extend((0..colour).map(|i| c[i % 3]));
                if bg.format().alpha {
                    data.push(1.0);
                }
            }
        }
        bg.write_region(Rect::new(0, 0, W as i32, H as i32), &data);
        Ok(())
    })
    .unwrap();
    s
}

fn rgb8() -> Session {
    disc(json!({"width": W, "height": H}))
}

/// What a perfect model would answer for the disc: saliency at the model's resolution.
fn perfect_output(m: &Model, area: Rect) -> Vec<f32> {
    let side = m.side as f32;
    (0..m.side * m.side)
        .map(|i| {
            let (x, y) = (((i % m.side) as f32 + 0.5) / side, ((i / m.side) as f32 + 0.5) / side);
            // Raw model outputs are not 0..1: the engine stretches them.
            if inside(area.x0 as f32 + x * area.width() as f32, area.y0 as f32 + y * area.height() as f32) { 7.5 } else { -3.0 }
        })
        .collect()
}

/// Plays the browser shell: the model is "installed" outside the engine, and its output for the
/// command is handed in before the command runs.
fn answer(s: &mut Session, cmd: &str, p: &Value) {
    set_external(s, ["u2netp"]);
    let req = subject_request(s, cmd, p).unwrap().expect("a model request");
    assert_eq!(req.input.shape, [1, 3, req.model.side, req.model.side]);
    assert!(req.input.data.iter().all(|v| v.is_finite()));
    let out = perfect_output(req.model, req.key.area);
    subject_ready(s, req.key, out);
}

fn mask_at(s: &Session, layer: u64, x: i32, y: i32) -> f32 {
    let doc = &s.active().unwrap().doc;
    let m = doc.layer(LayerId(layer)).unwrap().mask.as_ref().expect("a layer mask");
    photocraft_algo::selection::mask_from_surface(Some(&m.surface), Rect::new(x, y, x + 1, y + 1))[0]
}

#[test]
fn models_are_listed_and_none_is_installed_by_default() {
    let mut s = Session::new();
    let r = s.execute("ai.models", json!({})).unwrap();
    let list = r["models"].as_array().unwrap();
    assert_eq!(list.len(), MODELS.len());
    assert_eq!(list[0]["id"], "u2netp");
    assert_eq!(list[0]["license"], "Apache-2.0");
    assert!(list.iter().all(|m| m["installed"] == false && m["url"].as_str().unwrap().starts_with("https://")));
    set_external(&mut s, ["isnet", "no-such-model"]);
    let r = s.execute("ai.models", json!({})).unwrap();
    assert_eq!((r["models"][0]["installed"].clone(), r["models"][1]["installed"].clone()), (json!(false), json!(true)));
}

#[test]
fn without_a_model_the_classical_method_runs_and_an_explicit_model_is_an_error() {
    let mut s = rgb8();
    assert!(subject_request(&s, "layer.removeBackground", &json!({})).unwrap().is_none(), "nothing for the shell to do");
    let r = s.execute("layer.removeBackground", json!({})).unwrap();
    assert_eq!(r["model"], "classical");
    s.execute("edit.undo", json!({})).unwrap();
    let e = s.execute("layer.removeBackground", json!({"model": "u2netp"})).unwrap_err().to_string();
    assert!(e.contains("U²-Netp") && e.contains("not installed"), "{e}");
    assert!(s.active().unwrap().doc.layers[0].mask.is_none(), "a failed command changes nothing");
}

#[test]
fn a_model_output_becomes_the_layer_mask_in_one_undo_step() {
    for depth in [8, 16, 32] {
        let mut s = disc(json!({"width": W, "height": H, "depth": depth}));
        let p = json!({"model": "u2netp"});
        answer(&mut s, "layer.removeBackground", &p);
        let before = s.active().unwrap().history.past_len();
        let r = s.execute("layer.removeBackground", p).unwrap();
        assert_eq!(r["model"], "u2netp", "depth {depth}");
        let id = r["layer"].as_u64().unwrap();
        assert!(mask_at(&s, id, 100, 80) > 0.95, "depth {depth}: centre {}", mask_at(&s, id, 100, 80));
        assert!(mask_at(&s, id, 10, 10) < 0.05 && mask_at(&s, id, 190, 150) < 0.05, "depth {depth}: corners");
        // The edge follows the disc to within the model's own pixel size here (under a pixel).
        assert!(mask_at(&s, id, 100, 30) < 0.2 && mask_at(&s, id, 100, 40) > 0.8, "depth {depth}: top edge at y = 35");
        assert_eq!(s.active().unwrap().history.past_len(), before + 1);
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.active().unwrap().doc.layers[0].mask.is_none());
        s.execute("edit.redo", json!({})).unwrap();
        assert!(mask_at(&s, id, 100, 80) > 0.95);
    }
}

#[test]
fn an_output_is_used_once_and_only_for_the_image_it_was_made_from() {
    let mut s = rgb8();
    let p = json!({"model": "u2netp"});
    answer(&mut s, "layer.removeBackground", &p);
    // The document changes while the model runs: the answer is stale.
    s.execute("image.adjustments.invert", json!({})).unwrap();
    let e = s.execute("layer.removeBackground", p.clone()).unwrap_err().to_string();
    assert!(e.contains("no result"), "{e}");
    // "auto" falls back to the classical method instead of failing.
    answer(&mut s, "layer.removeBackground", &json!({}));
    s.execute("image.adjustments.invert", json!({})).unwrap();
    assert_eq!(s.execute("layer.removeBackground", json!({})).unwrap()["model"], "classical");
    s.execute("edit.undo", json!({})).unwrap();
    // A fresh answer works once, then is gone.
    answer(&mut s, "layer.removeBackground", &p);
    assert_eq!(s.execute("layer.removeBackground", p.clone()).unwrap()["model"], "u2netp");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.execute("layer.removeBackground", p).is_err());
}

#[test]
fn output_can_be_a_new_layer_or_a_cut_out() {
    // A new layer holding only the subject; the source is hidden and kept.
    let mut s = rgb8();
    let p = json!({"model": "u2netp", "output": "newLayer"});
    answer(&mut s, "layer.removeBackground", &p);
    let r = s.execute("layer.removeBackground", p).unwrap();
    let doc = s.active().unwrap().doc.clone();
    assert_eq!(doc.layers.len(), 2);
    let new = doc.layer(LayerId(r["layer"].as_u64().unwrap())).unwrap();
    assert!(new.mask.is_none() && !doc.layers[0].visible && s.active().unwrap().active_layer == Some(new.id));
    let px = |x, y| new.surface().unwrap().pixel(x, y);
    assert!(px(100, 80)[3] > 0.95 && px(10, 10)[3] < 0.05, "{:?} {:?}", px(100, 80), px(10, 10));

    // Decontaminating colours needs pixels of its own, so a mask becomes a new masked layer.
    let mut s = rgb8();
    let p = json!({"model": "u2netp", "decontaminate": true});
    answer(&mut s, "layer.removeBackground", &p);
    let r = s.execute("layer.removeBackground", p).unwrap();
    let doc = s.active().unwrap().doc.clone();
    assert!(doc.layers.len() == 2 && doc.layer(LayerId(r["layer"].as_u64().unwrap())).unwrap().mask.is_some());

    // Cut-out: the layer's own background becomes transparent; no mask, no new layer.
    let mut s = rgb8();
    let p = json!({"model": "u2netp", "output": "cutout", "feather": 2});
    answer(&mut s, "layer.removeBackground", &p);
    s.execute("layer.removeBackground", p).unwrap();
    let doc = s.active().unwrap().doc.clone();
    assert!(doc.layers.len() == 1 && doc.layers[0].mask.is_none());
    let px = |x, y| doc.layers[0].surface().unwrap().pixel(x, y);
    assert!(px(100, 80)[3] > 0.95 && px(10, 10)[3] < 0.05);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.active().unwrap().doc.layers[0].surface().unwrap().pixel(10, 10)[3] > 0.95, "undo brings the background back");
}

#[test]
fn select_subject_and_object_selection_use_the_model() {
    let mut s = rgb8();
    let p = json!({"model": "u2netp"});
    answer(&mut s, "select.subject", &p);
    let r = s.execute("select.subject", p).unwrap();
    assert_eq!(r["changed"], true);
    let sel = |s: &Session, x, y| photocraft_algo::selection::mask_from_surface(s.active().unwrap().doc.selection.as_ref(), Rect::new(x, y, x + 1, y + 1))[0];
    assert!(sel(&s, 100, 80) > 0.95 && sel(&s, 10, 10) < 0.05);

    // Object Selection looks only inside its rectangle: here the left half of the disc.
    let mut s = rgb8();
    let p = json!({"model": "u2netp", "rect": [20, 10, 80, 140]});
    answer(&mut s, "select.object", &p);
    s.execute("select.object", p).unwrap();
    assert!(sel(&s, 80, 80) > 0.9, "inside the rectangle and the disc: {}", sel(&s, 80, 80));
    assert!(sel(&s, 120, 80) < 0.05, "in the disc but outside the rectangle: {}", sel(&s, 120, 80));
    assert!(sel(&s, 30, 20) < 0.05);
}

/// A click asks the segmentation model about the whole image and the point; its low-resolution
/// logits (over the padded 1024 square) become the selection.
#[test]
fn a_click_selects_what_the_segmentation_model_finds_there() {
    let mut s = rgb8();
    set_external(&mut s, ["mobilesam"]);
    let p = json!({"point": [100, 80]});
    let req = subject_request(&s, "select.object", &p).unwrap().expect("a request for the shell");
    assert_eq!(req.model.id, "mobilesam");
    assert_eq!(req.input.shape, [1, 3 * 1024 * 1024 + 2]);
    // The 200 × 160 image fills 1024 × 819 of the square; the click lands at (512, 410).
    let point = &req.input.data[3 * 1024 * 1024..];
    assert!((point[0] - 512.0).abs() < 1.0 && (point[1] - 409.6).abs() < 1.0, "{point:?}");
    // A model that finds the disc: positive inside it, in the 256 square's image part (k = 1.28).
    let out: Vec<f32> = (0..256 * 256).map(|i| if inside((i % 256) as f32 / 1.28, (i / 256) as f32 / 1.28) { 6.0 } else { -6.0 }).collect();
    subject_ready(&mut s, req.key, out);
    let r = s.execute("select.object", p.clone()).unwrap();
    assert_eq!(r["changed"], true);
    let sel = |s: &Session, x, y| photocraft_algo::selection::mask_from_surface(s.active().unwrap().doc.selection.as_ref(), Rect::new(x, y, x + 1, y + 1))[0];
    assert!(sel(&s, 100, 80) > 0.95 && sel(&s, 130, 80) > 0.9, "the disc: {} {}", sel(&s, 100, 80), sel(&s, 130, 80));
    assert!(sel(&s, 10, 10) < 0.05 && sel(&s, 190, 150) < 0.05, "not the background");

    // Without the model a click falls back to the Object Finder; a point off the canvas is refused.
    let mut plain = rgb8();
    assert!(plain.execute("select.object", json!({"point": [100, 80]})).is_ok());
    for bad in [json!({"point": [500, 80]}), json!({"point": [1]}), json!({"point": "x"}), json!({"point": [f64::MAX, 0]})] {
        assert!(s.execute("select.object", bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn other_colour_modes_give_the_model_srgb_it_can_read() {
    for mode in ["gray", "cmyk", "lab"] {
        let mut s = disc(json!({"width": W, "height": H, "mode": mode}));
        set_external(&mut s, ["u2netp"]);
        let req = subject_request(&s, "select.subject", &json!({})).unwrap().unwrap_or_else(|| panic!("{mode}: a request"));
        assert_eq!(req.input.data.len(), 3 * req.model.side * req.model.side, "{mode}");
        assert!(req.input.data.iter().all(|v| v.is_finite() && v.abs() < 4.0), "{mode}: normalised sRGB values");
    }
}

#[test]
fn bad_params_are_errors_not_panics() {
    let mut s = rgb8();
    set_external(&mut s, ["u2netp"]);
    for p in [json!({"model": 5}), json!({"model": "nope"}), json!({"model": ""}), json!({"output": "poster"}), json!({"output": 3}), json!({"layer": 999_999})]
    {
        assert!(s.execute("layer.removeBackground", p.clone()).is_err(), "{p}");
    }
    for p in
        [json!({"model": "u2netp", "rect": [0, 0, 0, 0]}), json!({"rect": "x"}), json!({"model": "u2netp", "rect": [1e300, 0, 5, 5]}), json!({"model": {}})]
    {
        assert!(s.execute("select.object", p.clone()).is_err(), "{p}");
    }
    // Numbers out of range are clamped, not trusted.
    answer(&mut s, "layer.removeBackground", &json!({"feather": 1e30, "amount": -4}));
    s.execute("layer.removeBackground", json!({"feather": 1e30, "amount": -4})).unwrap();
    for (cmd, p) in [
        ("ai.model.install", json!({})),
        ("ai.model.install", json!({"id": 7})),
        ("ai.model.install", json!({"id": "nope", "path": "/x"})),
        ("ai.model.remove", json!({})),
        ("ai.model.remove", json!({"id": "nope"})),
    ] {
        assert!(s.execute(cmd, p.clone()).is_err(), "{cmd} {p}");
    }
    // Removing a model that is not installed is fine, and forgets what the shell reported.
    assert_eq!(s.execute("ai.model.remove", json!({"id": "u2netp"})).unwrap()["installed"], false);
    assert!(!is_installed(&s, &MODELS[0]));
    // A tiny document has nothing to segment; that is a message, not a crash.
    let mut tiny = Session::new();
    tiny.execute("file.new", json!({"width": 2, "height": 2})).unwrap();
    set_external(&mut tiny, ["u2netp"]);
    assert!(subject_request(&tiny, "select.subject", &json!({})).is_err());
    assert!(Session::new().execute("select.subject", json!({"model": "u2netp"})).is_err(), "no document");
}

/// With the real model (never committed; set `PHOTOCRAFT_MODEL_DIR` to a folder holding
/// `u2netp.onnx` from its pinned URL): install it from a file, then Remove Background finds the
/// disc in this process, and removing the model deletes it.
#[cfg(feature = "ml-runtime")]
#[test]
fn the_real_model_is_installed_from_a_file_and_runs_in_process() {
    let Some(src) = std::env::var_os("PHOTOCRAFT_MODEL_DIR").map(|d| std::path::Path::new(&d).join("u2netp.onnx")).filter(|p| p.is_file()) else {
        eprintln!("skipped: set PHOTOCRAFT_MODEL_DIR to a folder with u2netp.onnx");
        return;
    };
    let dir = std::env::temp_dir().join(format!("photocraft-ai-test-{}", std::process::id()));
    let mut s = rgb8();
    assert!(s.execute("ai.model.install", json!({"id": "u2netp", "path": src})).is_err(), "no model folder yet");
    s.ai.model_dir = Some(dir.clone());
    // A file that is not the model is refused by its checksum.
    let wrong = dir.join("wrong.bin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&wrong, b"not a model").unwrap();
    let e = s.execute("ai.model.install", json!({"id": "u2netp", "path": wrong})).unwrap_err().to_string();
    assert!(e.contains("checksum"), "{e}");
    assert!(s.execute("ai.model.install", json!({"id": "u2netp", "path": dir.join("missing.onnx")})).is_err());
    assert_eq!(s.execute("ai.model.install", json!({"id": "u2netp", "path": src})).unwrap()["installed"], true);
    assert!(is_installed(&s, &MODELS[0]) && subject_request(&s, "layer.removeBackground", &json!({})).unwrap().is_none(), "runs here, not in the shell");
    let r = s.execute("layer.removeBackground", json!({})).unwrap();
    assert_eq!(r["model"], "u2netp");
    let id = r["layer"].as_u64().unwrap();
    assert!(mask_at(&s, id, 100, 80) > 0.9 && mask_at(&s, id, 8, 8) < 0.1, "{} {}", mask_at(&s, id, 100, 80), mask_at(&s, id, 8, 8));
    s.execute("ai.model.remove", json!({"id": "u2netp"})).unwrap();
    assert!(!is_installed(&s, &MODELS[0]));
    let _ = std::fs::remove_dir_all(&dir);
}

/// With the real MobileSAM files in `PHOTOCRAFT_MODEL_DIR` (`mobilesam.onnx`, the encoder, and
/// `mobilesam.data`, the decoder; never committed): a click selects the disc in this process.
#[cfg(feature = "ml-runtime")]
#[test]
fn a_click_runs_the_real_segmenter_in_process() {
    let Some(dir) = std::env::var_os("PHOTOCRAFT_MODEL_DIR").map(std::path::PathBuf::from).filter(|d| d.join("mobilesam.data").is_file()) else {
        eprintln!("skipped: set PHOTOCRAFT_MODEL_DIR to a folder with mobilesam.onnx and mobilesam.data");
        return;
    };
    let mut s = rgb8();
    s.ai.model_dir = Some(dir);
    assert!(subject_request(&s, "select.object", &json!({"point": [100, 80]})).unwrap().is_none(), "runs here, not in the shell");
    assert_eq!(s.execute("select.object", json!({"point": [100, 80]})).unwrap()["changed"], true);
    let sel = |x, y| photocraft_algo::selection::mask_from_surface(s.active().unwrap().doc.selection.as_ref(), Rect::new(x, y, x + 1, y + 1))[0];
    assert!(sel(100, 80) > 0.9 && sel(130, 80) > 0.9, "the disc: {} {}", sel(100, 80), sel(130, 80));
    assert!(sel(8, 8) < 0.1 && sel(190, 150) < 0.1, "not the background: {} {}", sel(8, 8), sel(190, 150));
}
