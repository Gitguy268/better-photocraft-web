//! Project links: the Help menu, the About dialog and the start screen all open these.
//!
//! This edition is built on the official PhotoCraft; `UPSTREAM` credits it. `GITHUB` and `ISSUES`
//! point at this edition's own repository (the owner's username is filled in at publish time,
//! see `PUBLISHING.md`), so its bugs don't land in upstream's tracker.

use serde_json::{Value, json};

use crate::PhotocraftApp;

/// The official PhotoCraft by the ArtCraft team, which this edition is built on.
pub const UPSTREAM: &str = "https://github.com/storytold/photocraft";
pub const GITHUB: &str = "https://github.com/TODO-USERNAME/Better-Photocraft-Web";
pub const ISSUES: &str = "https://github.com/TODO-USERNAME/Better-Photocraft-Web/issues";

/// The credit shown in About (and the README): plain text, as upstream's brand terms ask.
pub const CREDIT: &str = "Built on the official PhotoCraft by the ArtCraft team; this edition is not published or endorsed by them.";

/// Help-menu link commands: (id, url). Labels live in `menus::UI_COMMANDS`.
pub const COMMANDS: &[(&str, &str)] = &[("help.upstream", UPSTREAM), ("help.github", GITHUB), ("help.reportIssue", ISSUES)];

pub fn url_for(id: &str) -> Option<&'static str> {
    COMMANDS.iter().find(|c| c.0 == id).map(|c| c.1)
}

/// Open `url` in the system browser (a new tab on the web) and note it in the status bar. Prefers
/// the platform `open_url` service (reliable on Windows/macOS/Linux); falls back to `ctx.open_url`.
pub fn open(app: &mut PhotocraftApp, ctx: &egui::Context, url: &str) -> Value {
    let opened = app.services.open_url.as_ref().map(|f| f(url).is_ok()).unwrap_or(false);
    if !opened {
        ctx.open_url(egui::OpenUrl::new_tab(url));
    }
    let template = "Opened {url}";
    app.ui.status = crate::i18n::fmt(tl!(template), &[("url", url)]);
    json!({"url": url})
}

/// "Official PhotoCraft · GitHub" as links, centred. Clicks route through [`open`] (the platform
/// browser service) rather than `ui.hyperlink_to`, which uses the unreliable `ctx.open_url`.
pub fn link_row(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let links = [(tl!("Official PhotoCraft"), UPSTREAM), (tl!("GitHub"), GITHUB)];
    let font = egui::FontId::proportional(12.5);
    let sep = "  ·  ";
    let width: f32 = links.iter().map(|(l, _)| ui.painter().layout_no_wrap((*l).into(), font.clone(), t.text).size().x).sum::<f32>()
        + (links.len() - 1) as f32 * ui.painter().layout_no_wrap(sep.into(), font.clone(), t.text).size().x;
    let mut clicked: Option<&str> = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
        for (i, (label, url)) in links.iter().enumerate() {
            if i > 0 {
                ui.label(egui::RichText::new(sep).font(font.clone()).color(t.text_faint));
            }
            if ui.link(egui::RichText::new(*label).font(font.clone()).color(t.accent)).on_hover_text(*url).clicked() {
                clicked = Some(url);
            }
        }
    });
    if let Some(url) = clicked {
        open(app, ui.ctx(), url);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_open_through_the_platform_service() {
        // Issue #14: links (Help menu, start-page links) must open in the browser.
        // They route through the `open_url` service rather than the unreliable `ctx.open_url`.
        use std::sync::{Arc, Mutex};
        let opened: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let rec = opened.clone();
        let services = crate::Services {
            open_url: Some(Box::new(move |u: &str| {
                rec.lock().unwrap().push(u.to_string());
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        let ctx = egui::Context::default();
        // Every Help-menu link id reaches the service with its URL.
        for (id, url) in COMMANDS {
            crate::menus::invoke(&mut app, &ctx, id, json!({})).unwrap();
            assert_eq!(opened.lock().unwrap().last().map(String::as_str), Some(*url), "{id}");
        }
        // The direct open() helper (start-page links) also uses it.
        open(&mut app, &ctx, UPSTREAM);
        assert_eq!(opened.lock().unwrap().last().map(String::as_str), Some(UPSTREAM));
    }

    #[test]
    fn help_menu_lists_links_then_separator_then_system_info_and_about() {
        let app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let help: Vec<String> = crate::menus::menu_items(&app).into_iter().filter(|i| i.path == ["Help"]).map(|i| i.id).collect();
        assert_eq!(help, ["help.upstream", "help.github", "help.reportIssue", "help.checkForUpdates", "---", "help.systemInfo", "help.about"]);
        for (id, _) in COMMANDS {
            assert!(crate::menus::is_live(id) && crate::menus::is_enabled(&app, id), "{id}");
        }
    }

    #[test]
    fn link_commands_open_their_urls() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let ctx = egui::Context::default();
        for (id, url) in [("help.upstream", "https://github.com/storytold/photocraft"), ("help.github", GITHUB)] {
            let r = crate::menus::invoke(&mut app, &ctx, id, serde_json::json!({})).unwrap();
            assert_eq!(r["url"], url);
            assert_eq!(app.ui.status, format!("Opened {url}"));
        }
    }

    /// Upstream's brand terms: a modified version credits PhotoCraft in plain text and drops the
    /// ArtCraft community promotion.
    #[test]
    fn this_edition_credits_upstream_and_promotes_no_upstream_community() {
        assert!(CREDIT.contains("official PhotoCraft") && CREDIT.contains("not published or endorsed"));
        assert!(COMMANDS.iter().all(|(_, url)| !url.contains("discord") && !url.contains("getartcraft")));
        assert!(ISSUES.starts_with(GITHUB) && GITHUB != UPSTREAM);
    }
}
