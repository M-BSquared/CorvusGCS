//! Inter and JetBrains Mono, the web interface's fonts, decoded from its own
//! WOFF2 files at build time (build.rs).
//!
//! Both are variable fonts. Each weight the interface uses gets its own egui
//! family over the same file, pinned to that weight on the `wght` axis, the
//! way one `@font-face` with a weight range serves 400 to 700 in CSS. The
//! latin-ext file follows the latin one, as CSS's `unicode-range` split does.

use std::sync::Arc;

use eframe::egui::{self, epaint::text::VariationCoords, FontData, FontDefinitions, FontFamily, FontTweak};

static INTER_LATIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/Inter-latin.ttf"));
static INTER_LATIN_EXT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/Inter-latin-ext.ttf"));
static MONO_LATIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/JetBrainsMono-latin.ttf"));
static MONO_LATIN_EXT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/JetBrainsMono-latin-ext.ttf"));

const WEIGHTS: [u32; 4] = [400, 500, 600, 700];

fn weighted(bytes: &'static [u8], weight: u32) -> Arc<FontData> {
    let tweak = FontTweak {
        coords: VariationCoords::new([(b"wght", weight as f32)]),
        ..Default::default()
    };
    Arc::new(FontData::from_static(bytes).tweak(tweak))
}

/// Installs the fonts; the default proportional face is Inter 400 and the
/// monospace one JetBrains Mono 400, like `--sans` and `--mono`.
pub fn install(ctx: &egui::Context) {
    let mut defs = FontDefinitions::empty();
    for w in WEIGHTS {
        for (name, bytes) in [("inter", INTER_LATIN), ("inter-ext", INTER_LATIN_EXT)] {
            defs.font_data.insert(format!("{name}-{w}"), weighted(bytes, w));
        }
        for (name, bytes) in [("mono", MONO_LATIN), ("mono-ext", MONO_LATIN_EXT)] {
            defs.font_data.insert(format!("{name}-{w}"), weighted(bytes, w));
        }
        defs.families.insert(
            FontFamily::Name(format!("inter-{w}").into()),
            vec![format!("inter-{w}"), format!("inter-ext-{w}")],
        );
        defs.families.insert(
            FontFamily::Name(format!("mono-{w}").into()),
            vec![format!("mono-{w}"), format!("mono-ext-{w}")],
        );
    }
    defs.families.insert(
        FontFamily::Proportional,
        vec!["inter-400".into(), "inter-ext-400".into()],
    );
    defs.families
        .insert(FontFamily::Monospace, vec!["mono-400".into(), "mono-ext-400".into()]);
    ctx.set_fonts(defs);
}
