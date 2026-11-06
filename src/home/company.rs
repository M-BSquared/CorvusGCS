//! The operator's company logo at the far right of the top bar
//! (`.tb-company-logo` of `python/src/css/main.css`, `applyCompanyLogo` of
//! `python/src/js/topbar.js`). No logo ships by default; the Corvus mark keeps
//! the left end either way.

use std::sync::Arc;

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::ui::style;

/// The logo's file as the backend has it, ready to draw.
#[derive(Clone)]
pub struct Loaded {
    pub uri: String,
    pub bytes: Arc<[u8]>,
    pub size: (u32, u32),
}

/// Which logo is in force and where its bytes are.
pub struct CompanyLogo {
    server: Server,
    name: String,
    rev: u32,
    load: Option<Pending<Option<Vec<u8>>>>,
    loaded: Option<Loaded>,
}

impl CompanyLogo {
    pub fn new(server: Server) -> CompanyLogo {
        CompanyLogo {
            server,
            name: String::new(),
            rev: 0,
            load: None,
            loaded: None,
        }
    }

    /// The logo the config names; a new name or a replaced file is fetched
    /// again, an empty name drops it.
    pub fn set(&mut self, ctx: &egui::Context, name: &str, rev: u32) {
        if self.name == name && self.rev == rev {
            return;
        }
        self.name = name.to_string();
        self.rev = rev;
        if let Some(old) = self.loaded.take() {
            ctx.forget_image(&old.uri);
        }
        self.load = None;
        if name.is_empty() {
            return;
        }
        let server = self.server.clone();
        self.load = Some(Pending::spawn(ctx, move || {
            http::get_bytes(&server, &format!("/api/branding/logo?v={rev}"))
        }));
    }

    /// Takes the answer once it is there; a file that is not a PNG hides
    /// itself, as a failed image does on the web.
    pub fn poll(&mut self) {
        let Some(answer) = self.load.as_ref().and_then(Pending::take) else {
            return;
        };
        self.load = None;
        self.loaded = answer.and_then(|b| {
            let size = png_size(&b)?;
            Some(Loaded {
                uri: format!("bytes://company-logo-top-{}.png", self.rev),
                bytes: b.into(),
                size,
            })
        });
    }

    pub fn loaded(&self) -> Option<&Loaded> {
        self.loaded.as_ref()
    }
}

/// Width and height from a PNG's IHDR chunk.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 24 || bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// `.tb-company-logo { height: 32px; max-width: 160px }`, and under 601 px
/// wide `24px` and `90px`: the box the picture is drawn in.
fn picture(size: (u32, u32), narrow: bool) -> Vec2 {
    let (height, max_w) = if narrow { (24.0, 90.0) } else { (32.0, 160.0) };
    let natural = height * size.0 as f32 / size.1 as f32;
    Vec2::new(natural.min(max_w), height)
}

/// `margin-left`, the 1 px rule and `padding-left` before the picture.
fn lead(narrow: bool) -> f32 {
    if narrow {
        6.0 + 1.0 + 8.0
    } else {
        10.0 + 1.0 + 14.0
    }
}

/// What the logo takes of the bar's width.
pub fn width(logo: &Loaded, narrow: bool) -> f32 {
    lead(narrow) + picture(logo.size, narrow).x
}

/// Draws the logo with its left edge at `x`, centred on the bar.
pub fn paint(ui: &egui::Ui, bar: Rect, x: f32, logo: &Loaded, narrow: bool) {
    let pic = picture(logo.size, narrow);
    let top = bar.center().y - pic.y / 2.0;
    let rule_x = x + if narrow { 6.0 } else { 10.0 } + 0.5;
    ui.painter()
        .vline(rule_x, top..=top + pic.y, Stroke::new(1.0, style::border_soft()));
    let rect = Rect::from_min_size(Pos2::new(x + lead(narrow), top), pic);
    egui::Image::from_bytes(logo.uri.clone(), egui::load::Bytes::from(logo.bytes.clone()))
        .fit_to_exact_size(pic)
        .maintain_aspect_ratio(true)
        .paint_at(ui, rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b
    }

    #[test]
    fn the_size_is_read_from_the_png_header() {
        assert_eq!(png_size(&png(400, 100)), Some((400, 100)));
        assert_eq!(png_size(&png(0, 100)), None);
        assert_eq!(png_size(b"GIF89a not a png at all, long enough"), None);
        assert_eq!(png_size(&[]), None);
    }

    #[test]
    fn a_wide_logo_stops_at_its_maximum_width_and_a_narrow_one_keeps_its_shape() {
        assert_eq!(picture((1000, 100), false), Vec2::new(160.0, 32.0));
        assert_eq!(picture((64, 64), false), Vec2::new(32.0, 32.0));
        assert_eq!(picture((1000, 100), true), Vec2::new(90.0, 24.0));
    }

    #[test]
    fn the_bar_gives_it_its_margin_rule_and_padding() {
        let l = Loaded {
            uri: String::new(),
            bytes: Arc::from(Vec::new()),
            size: (64, 64),
        };
        assert_eq!(width(&l, false), 10.0 + 1.0 + 14.0 + 32.0);
        assert_eq!(width(&l, true), 6.0 + 1.0 + 8.0 + 24.0);
    }
}
