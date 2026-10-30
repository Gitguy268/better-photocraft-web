use std::cell::RefCell;
use std::rc::Rc;

use photocraft_engine::Session;
use photocraft_geom::Rect;

use super::*;

/// What the fake platform was asked to do, and what it will report next.
#[derive(Default)]
struct Platform {
    downloads: Vec<&'static str>,
    runs: Vec<(u64, &'static str, usize)>,
    cancelled: (u32, u32),
    events: Vec<AiEvent>,
    /// The input of the latest run, for tests that answer it.
    last_input: Option<Tensor>,
}

/// An app with an orange disc on dark blue (200 × 160) and a platform that runs models itself,
/// as the browser does.
fn app() -> (PhotocraftApp, Rc<RefCell<Platform>>) {
    let mut session = Session::new();
    session.execute("file.new", json!({"width": 200, "height": 160})).unwrap();
    session
        .edit("paint", |doc, _| {
            let bg = doc.layers[0].surface_mut().unwrap();
            let mut data = Vec::new();
            for i in 0..200 * 160 {
                data.extend(if inside((i % 200) as f32 + 0.5, (i / 200) as f32 + 0.5) { [0.9, 0.5, 0.1] } else { [0.05, 0.1, 0.3] });
                data.extend(bg.format().alpha.then_some(1.0));
            }
            bg.write_region(Rect::new(0, 0, 200, 160), &data);
            Ok(())
        })
        .unwrap();
    let p: Rc<RefCell<Platform>> = Rc::default();
    let (a, b, c, d, e) = (p.clone(), p.clone(), p.clone(), p.clone(), p.clone());
    let ai = AiService {
        download: Box::new(move |m| a.borrow_mut().downloads.push(m.id)),
        cancel_download: Box::new(move || b.borrow_mut().cancelled.0 += 1),
        remove: None,
        infer: Some(Box::new(move |ticket, m, input| {
            let mut p = c.borrow_mut();
            p.runs.push((ticket, m.id, input.data.len()));
            p.last_input = Some(input);
        })),
        cancel_infer: Box::new(move || d.borrow_mut().cancelled.1 += 1),
        poll: Box::new(move || std::mem::take(&mut e.borrow_mut().events)),
    };
    (PhotocraftApp::new(session, crate::Services { ai: Some(ai), ..Default::default() }), p)
}

fn inside(x: f32, y: f32) -> bool {
    (x - 100.0).powi(2) + (y - 80.0).powi(2) <= 45.0 * 45.0
}

/// What a perfect model answers for the disc.
fn disc_output(m: &Model) -> Vec<f32> {
    let side = m.side as f32;
    (0..m.side * m.side)
        .map(|i| if inside(((i % m.side) as f32 + 0.5) / side * 200.0, ((i / m.side) as f32 + 0.5) / side * 160.0) { 5.0 } else { -5.0 })
        .collect()
}

fn has_mask(app: &PhotocraftApp) -> bool {
    app.session.active().unwrap().doc.layers[0].mask.is_some()
}

#[test]
fn the_first_use_asks_before_downloading_then_runs_the_model_and_the_command() {
    let (mut app, p) = app();
    let ctx = egui::Context::default();
    // One click: nothing is installed, so nothing is downloaded or changed until the user agrees.
    let r = crate::jobs_ui::run(&mut app, "layer.removeBackground", json!({})).unwrap();
    assert_eq!(r["pending"], "download offer");
    assert!(app.ai.offer.as_ref().is_some_and(|o| o.0.id == "u2netp") && p.borrow().downloads.is_empty() && !has_mask(&app));
    answer_offer(&mut app, ButtonRole::Default);
    assert_eq!(p.borrow().downloads, ["u2netp"]);
    // Progress, then the platform says the model is in place: the waiting command goes on by itself.
    p.borrow_mut().events.push(AiEvent::Progress { model: &MODELS[0], received: 1_000_000 });
    tick(&mut app, &ctx);
    assert!(app.ai.downloading.as_ref().is_some_and(|d| d.1 == 1_000_000) && p.borrow().runs.is_empty());
    p.borrow_mut().events.push(AiEvent::Installed(vec!["u2netp"]));
    tick(&mut app, &ctx);
    let (ticket, model, len) = p.borrow().runs[0];
    assert_eq!((model, len), ("u2netp", 3 * 320 * 320));
    assert!(app.ai.downloading.is_none() && app.ai.task.is_some() && !has_mask(&app), "the model is running; the document waits");
    // An answer for another run is ignored; the right one lands as the layer mask.
    p.borrow_mut().events.push(AiEvent::Inferred { ticket: ticket + 7, output: Ok(vec![0.0; 320 * 320]) });
    tick(&mut app, &ctx);
    assert!(app.ai.task.is_some() && !has_mask(&app));
    p.borrow_mut().events.push(AiEvent::Inferred { ticket, output: Ok(disc_output(&MODELS[0])) });
    tick(&mut app, &ctx);
    assert!(app.ai.task.is_none() && has_mask(&app));
    let mask = &app.session.active().unwrap().doc.layers[0].mask.as_ref().unwrap().surface;
    let at = |x, y| photocraft_algo::selection::mask_from_surface(Some(mask), Rect::new(x, y, x + 1, y + 1))[0];
    assert!(at(100, 80) > 0.95 && at(10, 10) < 0.05);
    // From now on it is one click: no prompt, straight to the model.
    app.session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(crate::jobs_ui::run(&mut app, "select.subject", json!({})).unwrap()["pending"], "model");
    assert!(app.ai.offer.is_none() && p.borrow().runs.len() == 2);
}

#[test]
fn declining_uses_the_built_in_method_and_does_not_ask_again() {
    let (mut app, p) = app();
    crate::jobs_ui::run(&mut app, "layer.removeBackground", json!({})).unwrap();
    answer_offer(&mut app, ButtonRole::Alternate);
    assert!(has_mask(&app) && p.borrow().downloads.is_empty() && p.borrow().runs.is_empty());
    app.session.execute("edit.undo", json!({})).unwrap();
    crate::jobs_ui::run(&mut app, "layer.removeBackground", json!({})).unwrap();
    assert!(app.ai.offer.is_none() && has_mask(&app), "second time: no prompt");
    // Cancelling the prompt does nothing at all.
    let (mut app, p) = self::app();
    crate::jobs_ui::run(&mut app, "select.subject", json!({})).unwrap();
    answer_offer(&mut app, ButtonRole::Cancel);
    assert!(app.ai.offer.is_none() && app.session.active().unwrap().doc.selection.is_none() && p.borrow().downloads.is_empty());
}

#[test]
fn cancelling_and_failures_leave_the_document_alone() {
    let (mut app, p) = app();
    let ctx = egui::Context::default();
    crate::jobs_ui::run(&mut app, "layer.removeBackground", json!({})).unwrap();
    answer_offer(&mut app, ButtonRole::Default);
    cancel_running(&mut app);
    assert_eq!(p.borrow().cancelled, (1, 0));
    assert!(app.ai.downloading.is_none());
    // A failed download says why.
    crate::jobs_ui::run(&mut app, "layer.removeBackground", json!({})).unwrap();
    answer_offer(&mut app, ButtonRole::Default);
    p.borrow_mut().events.push(AiEvent::DownloadFailed { model: &MODELS[0], error: "offline".into() });
    tick(&mut app, &ctx);
    assert!(
        app.ai.downloading.is_none() && app.ui.notices.iter().any(|n| n.error && n.title.contains("offline")),
        "{:?}",
        app.ui.notices.iter().map(|n| &n.title).collect::<Vec<_>>()
    );
    // A cancelled or failed model run changes nothing; a late answer to a cancelled run is dropped.
    p.borrow_mut().events.push(AiEvent::Installed(vec!["u2netp"]));
    tick(&mut app, &ctx);
    crate::jobs_ui::run(&mut app, "layer.removeBackground", json!({})).unwrap();
    let ticket = p.borrow().runs.last().unwrap().0;
    cancel_running(&mut app);
    assert_eq!(p.borrow().cancelled, (1, 1));
    p.borrow_mut().events.push(AiEvent::Inferred { ticket, output: Ok(disc_output(&MODELS[0])) });
    tick(&mut app, &ctx);
    assert!(!has_mask(&app));
    crate::jobs_ui::run(&mut app, "layer.removeBackground", json!({})).unwrap();
    let ticket = p.borrow().runs.last().unwrap().0;
    p.borrow_mut().events.push(AiEvent::Inferred { ticket, output: Err("out of memory".into()) });
    tick(&mut app, &ctx);
    assert!(!has_mask(&app) && app.ui.notices.iter().any(|n| n.title.contains("out of memory")));
}

#[test]
fn scripts_and_platforms_without_the_service_run_commands_as_given() {
    let (mut app, p) = app();
    app.automation_input = true;
    crate::jobs_ui::run(&mut app, "layer.removeBackground", json!({})).unwrap();
    assert!(has_mask(&app) && app.ai.offer.is_none() && p.borrow().downloads.is_empty(), "no prompt for a script");
    let mut plain = PhotocraftApp::new(Session::new(), Default::default());
    plain.session.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
    assert!(intercept(&mut plain, "select.subject", &json!({})).is_none());
    assert!(intercept(&mut app, "filter.blur.gaussianBlur", &json!({})).is_none(), "other commands are never touched");
}

#[test]
fn options_are_remembered_for_the_one_click_command_and_the_dialogs_open() {
    let (mut app, p) = app();
    let ctx = egui::Context::default();
    p.borrow_mut().events.push(AiEvent::Installed(vec!["u2netp"]));
    tick(&mut app, &ctx);
    // The options dialog runs the command with every field; the classical method needs no model.
    crate::jobs_ui::run(
        &mut app,
        "layer.removeBackground",
        json!({"model": "classical", "output": "newLayer", "refine": true, "feather": 0, "decontaminate": false, "amount": 100}),
    )
    .unwrap();
    assert_eq!(app.session.active().unwrap().doc.layers.len(), 2);
    app.session.execute("edit.undo", json!({})).unwrap();
    crate::jobs_ui::run(&mut app, "layer.removeBackground", json!({})).unwrap();
    assert_eq!(app.session.active().unwrap().doc.layers.len(), 2, "one click repeats the last options");
    assert!(p.borrow().runs.is_empty());
    assert!(crate::menus::invoke(&mut app, &ctx, "layer.removeBackgroundOptions", json!({})).unwrap()["dialog"].is_u64());
    crate::menus::invoke(&mut app, &ctx, "filter.ai.models", json!({})).unwrap();
    assert!(app.ai.manager_open);
    for id in ["layer.removeBackgroundOptions", "filter.ai.models"] {
        assert!(crate::menus::is_live(id) && crate::menus::is_enabled(&app, id), "{id}");
    }
    // The manager needs no document; the options need a layer to act on.
    let empty = PhotocraftApp::new(Session::new(), Default::default());
    assert!(crate::menus::is_enabled(&empty, "filter.ai.models") && !crate::menus::is_enabled(&empty, "layer.removeBackgroundOptions"));
    // The dialog's fields are the command's own parameters.
    let keys: Vec<String> = crate::filter_dialog::parse_spec(OPTIONS_SPEC).into_iter().map(|p| p.key).collect();
    assert_eq!(keys, ["model", "output", "refine", "feather", "decontaminate", "amount"]);
    let spec = photocraft_engine::commands::find("layer.removeBackground").unwrap().params;
    assert!(keys.iter().all(|k| spec.contains(&format!("\"{k}\":"))), "{spec}");
    let models = crate::filter_dialog::parse_spec(OPTIONS_SPEC).remove(0).kind;
    assert_eq!(
        models,
        crate::filter_dialog::Kind::Choice {
            options: ["auto", "classical"].into_iter().chain(MODELS.iter().filter(|m| m.purpose == Purpose::Subject).map(|m| m.id)).map(String::from).collect(),
            default: None,
        }
    );
}

/// A stand-in for the upscale model: every input pixel becomes a 4 x 4 block.
fn blocks(input: &Tensor) -> Vec<f32> {
    let side = input.shape[2];
    let big = side * 4;
    (0..3 * big * big).map(|i| input.data[(i / (big * big)) * side * side + ((i % (big * big)) / big / 4) * side + (i % big) / 4]).collect()
}

#[test]
fn upscale_asks_for_its_own_model_then_runs_tile_by_tile_in_the_worker() {
    let (mut app, p) = app();
    let ctx = egui::Context::default();
    // The subject model being installed doesn't matter: upscaling needs its own, and asks.
    p.borrow_mut().events.push(AiEvent::Installed(vec!["u2netp"]));
    tick(&mut app, &ctx);
    let r = crate::jobs_ui::run(&mut app, "filter.ai.upscale", json!({"scale": "2"})).unwrap();
    assert_eq!(r["pending"], "download offer");
    assert!(app.ai.offer.as_ref().is_some_and(|o| o.0.id == "realesrgan"));
    answer_offer(&mut app, ButtonRole::Default);
    assert_eq!(p.borrow().downloads, ["realesrgan"]);
    p.borrow_mut().events.push(AiEvent::Installed(vec!["u2netp", "realesrgan"]));
    tick(&mut app, &ctx);
    // The waiting command went on: tile after tile, each answered before the next is asked.
    let mut tiles = 0;
    while app.ai.task.is_some() {
        let (ticket, input) = {
            let mut platform = p.borrow_mut();
            (platform.runs.last().unwrap().0, platform.last_input.take().expect("one tile at a time"))
        };
        assert_eq!((p.borrow().runs.last().unwrap().1, input.shape.clone()), ("realesrgan", vec![1, 3, 128, 128]));
        assert_eq!(app.session.active().unwrap().doc.size.width, 200, "the document waits until every tile is in");
        p.borrow_mut().events.push(AiEvent::Inferred { ticket, output: Ok(blocks(&input)) });
        tick(&mut app, &ctx);
        tiles += 1;
        assert!(tiles < 50);
    }
    assert_eq!(tiles, 4, "200 x 160 in overlapping 128 px tiles");
    let doc = &app.session.active().unwrap().doc;
    assert_eq!((doc.size.width, doc.size.height), (400, 320));
    let px = doc.layers[0].surface().unwrap().pixel(200, 160);
    assert!((px[0] - 0.9).abs() < 0.02 && (px[2] - 0.1).abs() < 0.02, "the disc's centre is still orange: {px:?}");
    // A failing tile ends the task and leaves the document as it is.
    crate::jobs_ui::run(&mut app, "filter.ai.upscale", json!({"scale": "2"})).unwrap();
    let ticket = p.borrow().runs.last().unwrap().0;
    p.borrow_mut().events.push(AiEvent::Inferred { ticket, output: Ok(vec![0.0; 7]) });
    tick(&mut app, &ctx);
    assert!(app.ai.task.is_none() && app.session.active().unwrap().doc.size.width == 400);
    assert!(app.ui.notices.iter().any(|n| n.error && n.title.contains("Real-ESRGAN")));
}

#[test]
fn declining_the_upscale_model_resamples_and_other_offers_are_still_made() {
    let (mut app, p) = app();
    crate::jobs_ui::run(&mut app, "filter.ai.upscale", json!({"scale": "4"})).unwrap();
    answer_offer(&mut app, ButtonRole::Alternate);
    assert_eq!(app.session.active().unwrap().doc.size.width, 800, "enlarged with the built-in method");
    assert!(p.borrow().downloads.is_empty() && p.borrow().runs.is_empty());
    crate::jobs_ui::run(&mut app, "filter.ai.upscale", json!({"scale": "2"})).unwrap();
    assert!(app.ai.offer.is_none(), "not asked again for upscaling");
    crate::jobs_ui::run(&mut app, "select.subject", json!({})).unwrap();
    assert!(app.ai.offer.as_ref().is_some_and(|o| o.0.id == "u2netp"), "but still asked about the subject model");
    // The dialog has no live preview (a model can't run per slider change), and its choices are
    // the command's own.
    let spec = photocraft_engine::commands::find("filter.ai.upscale").unwrap().params;
    let params = crate::filter_dialog::parse_spec(spec);
    assert_eq!(params.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(), ["scale", "model"]);
    assert_eq!(params[1].kind, crate::filter_dialog::Kind::Choice { options: ["auto", "classical", "realesrgan"].map(String::from).to_vec(), default: None });
    assert!(crate::filter_dialog::has_dialog("filter.ai.upscale"));
    let dialog = crate::filter_dialog::open(&mut app, "filter.ai.upscale").unwrap();
    assert!(app.ui.dialogs.iter().find(|d| d.id == dialog).is_some_and(|d| !d.fields.contains_key("__preview")));
}

#[test]
fn the_remove_tool_never_interrupts_with_an_offer_and_uses_its_model_once_installed() {
    let (mut app, p) = app();
    let ctx = egui::Context::default();
    let stroke = json!({"points": [[90, 80], [110, 80]], "size": 10});
    let at = |app: &PhotocraftApp| app.session.active().unwrap().doc.layers[0].surface().unwrap().rgba(100, 80);
    // No model: the stroke fills from its surroundings straight away, with no dialog.
    let r = crate::jobs_ui::run(&mut app, "paint.remove", stroke.clone()).unwrap();
    assert!(r["model"] == "classical" && app.ai.offer.is_none() && p.borrow().downloads.is_empty() && p.borrow().runs.is_empty(), "{r}");
    app.session.execute("edit.undo", json!({})).unwrap();
    // Installed (from the AI Models dialog): the worker fills the stroke, in one undo step.
    p.borrow_mut().events.push(AiEvent::Installed(vec!["lama"]));
    tick(&mut app, &ctx);
    let (before, past) = (at(&app), app.session.active().unwrap().history.past_len());
    assert_eq!(crate::jobs_ui::run(&mut app, "paint.remove", stroke).unwrap()["pending"], "model");
    let (ticket, model, len) = p.borrow().runs[0];
    assert_eq!((model, len), ("lama", 4 * 512 * 512));
    assert!(matches!(app.ai.task.as_ref().map(|t| &t.work), Some(Work::Inpaint(_))) && at(&app) == before, "the document waits");
    // The progress window draws for this kind of work too.
    crate::PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::ALL[0]);
    ctx.run_ui(Default::default(), |ui| show(&mut app, ui.ctx())).textures_delta.clear();
    let green = (0..3 * 512 * 512).map(|i| if i / (512 * 512) == 1 { 255.0 } else { 0.0 }).collect();
    p.borrow_mut().events.push(AiEvent::Inferred { ticket, output: Ok(green) });
    tick(&mut app, &ctx);
    let px = at(&app);
    assert!(app.ai.task.is_none() && px[0] < 0.02 && px[1] > 0.98 && px[2] < 0.02, "{px:?}");
    assert_eq!(app.session.active().unwrap().history.past_len(), past + 1);
}
