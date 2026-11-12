//! The application icon: the logo fitted onto a transparent square.
//!
//! An integrator files an icon under `hicolor/<W>x<H>/apps` from its real size,
//! and the logo is not square: scaled with its aspect kept it came out 254 by
//! 256, a folder no icon theme looks in. So it is scaled to fit and centred on
//! a square instead.

use std::path::Path;

use image::imageops::FilterType;
use image::{ImageFormat, RgbaImage};

/// The logo at `input` fitted into a `size` by `size` RGBA PNG at `output`.
pub fn fit(input: &Path, output: &Path, size: u32) -> Result<(), String> {
    let logo = image::open(input)
        .map_err(|e| format!("{}: {e}", input.display()))?
        .to_rgba8();
    let square = fit_image(&logo, size);
    square
        .save_with_format(output, ImageFormat::Png)
        .map_err(|e| format!("{}: {e}", output.display()))
}

/// `img` scaled to fit `size` with its aspect kept and centred on a
/// transparent square.
pub fn fit_image(img: &RgbaImage, size: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let scale = f64::from(size) / f64::from(w.max(h).max(1));
    let (nw, nh) = (
        ((f64::from(w) * scale).round() as u32).clamp(1, size),
        ((f64::from(h) * scale).round() as u32).clamp(1, size),
    );
    let scaled = image::imageops::resize(img, nw, nh, FilterType::Lanczos3);
    let mut out = RgbaImage::new(size, size);
    image::imageops::overlay(
        &mut out,
        &scaled,
        i64::from((size - nw) / 2),
        i64::from((size - nh) / 2),
    );
    out
}

/// Width and height of a PNG, read from its header.
pub fn png_size(path: &Path) -> Result<(u32, u32), String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if data.len() < 24 || &data[..8] != b"\x89PNG\r\n\x1a\n" || &data[12..16] != b"IHDR" {
        return Err(format!("{} is not a PNG", path.display()));
    }
    let be = |i: usize| u32::from_be_bytes(data[i..i + 4].try_into().unwrap());
    Ok((be(16), be(20)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tall_logo_is_centred_on_a_transparent_square() {
        let logo = RgbaImage::from_pixel(753, 756, image::Rgba([200, 30, 30, 255]));
        let icon = fit_image(&logo, 256);
        assert_eq!(icon.dimensions(), (256, 256));
        assert_eq!(icon.get_pixel(128, 128)[3], 255, "the logo fills the middle");
        let wide = RgbaImage::from_pixel(400, 100, image::Rgba([0, 0, 0, 255]));
        let banner = fit_image(&wide, 256);
        assert_eq!(banner.get_pixel(128, 2)[3], 0, "above a wide logo is transparent");
        assert_eq!(banner.get_pixel(128, 128)[3], 255);
    }

    #[test]
    fn the_icon_of_the_repository_logo_is_256_square() {
        let dir = std::env::temp_dir().join(format!("corvus-icon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("icon.png");
        fit(&crate::repo_root().join("assets/CorvusGCS_logo.png"), &out, 256).unwrap();
        assert_eq!(png_size(&out).unwrap(), (256, 256));
        assert!(png_size(&crate::repo_root().join("VERSION")).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
