//! Where and when a picture was taken, from its EXIF block, and the same of a
//! video from what ffmpeg says about it. The files come from cameras and from
//! anywhere else, so every offset is checked and a damaged block gives nothing.

use std::io::Read;
use std::path::Path;

/// The head of a JPEG that is read: its EXIF block stands near the start.
const JPEG_HEAD: u64 = 1024 * 1024;
/// How much of any other picture is looked through for the block.
const OTHER_HEAD: u64 = 32 * 1024 * 1024;
const ENTRIES_MAX: u16 = 512;

const IFD_EXIF: u16 = 0x8769;
const IFD_GPS: u16 = 0x8825;
const DATE_TIME: u16 = 0x0132;
const DATE_TIME_ORIGINAL: u16 = 0x9003;
const LAT_REF: u16 = 1;
const LAT: u16 = 2;
const LON_REF: u16 = 3;
const LON: u16 = 4;
const ALT_REF: u16 = 5;
const ALT: u16 = 6;

/// What a file says about where and when.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Taken {
    /// `(latitude, longitude)` in degrees.
    pub place: Option<(f64, f64)>,
    /// Metres above sea level.
    pub alt: Option<f64>,
    /// `2026-11-25 14:02:11`, as the camera's clock stood; a video's ends in ` UTC`.
    pub time: Option<String>,
}

/// A place on the earth. Cameras without a fix write zeros: that is no place.
fn place(lat: f64, lon: f64) -> Option<(f64, f64)> {
    let ok = lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0;
    (ok && !(lat == 0.0 && lon == 0.0)).then_some((lat, lon))
}

/// A TIFF structure, the inside of an EXIF block.
struct Tiff<'a> {
    data: &'a [u8],
    little: bool,
}

#[derive(Clone, Copy)]
struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    /// Where the four bytes of the value or of its offset stand.
    at: usize,
}

impl<'a> Tiff<'a> {
    fn bytes(&self, at: usize, len: usize) -> Option<&'a [u8]> {
        self.data.get(at..at.checked_add(len)?)
    }

    fn u16(&self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.bytes(at, 2)?.try_into().ok()?;
        Some(if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.bytes(at, 4)?.try_into().ok()?;
        Some(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    fn open(data: &'a [u8]) -> Option<Tiff<'a>> {
        let little = match data.get(..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let t = Tiff { data, little };
        (t.u16(2)? == 42).then_some(t)
    }

    fn directory(&self, offset: u32) -> Vec<Entry> {
        let start = offset as usize;
        let Some(count) = self.u16(start) else {
            return Vec::new();
        };
        (0..usize::from(count.min(ENTRIES_MAX)))
            .map_while(|i| {
                let at = start.checked_add(2 + i * 12)?;
                Some(Entry {
                    tag: self.u16(at)?,
                    kind: self.u16(at.checked_add(2)?)?,
                    count: self.u32(at.checked_add(4)?)?,
                    at: at.checked_add(8)?,
                })
            })
            .collect()
    }

    /// Where an entry's value lies: in its own four bytes, or where they point.
    fn value(&self, e: Entry, unit: usize) -> Option<&'a [u8]> {
        let len = unit.checked_mul(e.count as usize)?;
        if len <= 4 {
            self.bytes(e.at, len)
        } else {
            self.bytes(self.u32(e.at)? as usize, len)
        }
    }

    fn ascii(&self, e: Entry) -> Option<String> {
        if e.kind != 2 || e.count > 128 {
            return None;
        }
        let text = self.value(e, 1)?;
        let end = text.iter().position(|b| *b == 0).unwrap_or(text.len());
        Some(String::from_utf8_lossy(&text[..end]).trim().to_string())
    }

    /// The first `N` rationals of an entry.
    fn rationals<const N: usize>(&self, e: Entry) -> Option<[f64; N]> {
        if e.kind != 5 || (e.count as usize) < N || e.count > 16 {
            return None;
        }
        let start = self.u32(e.at)? as usize;
        let mut out = [0.0; N];
        for (i, v) in out.iter_mut().enumerate() {
            let at = start.checked_add(i * 8)?;
            let (num, den) = (self.u32(at)?, self.u32(at.checked_add(4)?)?);
            if den == 0 {
                return None;
            }
            *v = f64::from(num) / f64::from(den);
        }
        Some(out)
    }
}

/// `2026:11:25 14:02:11` as `2026-11-25 14:02:11`.
fn camera_time(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let shape = b"0000:00:00 00:00:00";
    let fits = b.len() == shape.len()
        && b.iter()
            .zip(shape)
            .all(|(c, s)| if *s == b'0' { c.is_ascii_digit() } else { c == s });
    // A camera whose clock was never set writes zeros.
    (fits && !text.starts_with("0000")).then(|| format!("{}-{}-{}", &text[..4], &text[5..7], &text[8..]))
}

/// What the TIFF structure `data` says.
fn of_tiff(data: &[u8]) -> Option<Taken> {
    let tiff = Tiff::open(data)?;
    let first = tiff.directory(tiff.u32(4)?);
    let pointed = |entries: &[Entry], tag: u16| {
        let e = entries.iter().find(|e| e.tag == tag)?;
        Some(tiff.directory(tiff.u32(e.at)?))
    };
    let text = |entries: &[Entry], tag: u16| entries.iter().find(|e| e.tag == tag).and_then(|e| tiff.ascii(*e));
    let mut taken = Taken {
        time: pointed(&first, IFD_EXIF)
            .and_then(|d| text(&d, DATE_TIME_ORIGINAL))
            .or_else(|| text(&first, DATE_TIME))
            .and_then(|t| camera_time(&t)),
        ..Taken::default()
    };
    if let Some(gps) = pointed(&first, IFD_GPS) {
        let find = |tag: u16| gps.iter().find(|e| e.tag == tag).copied();
        let angle = |tag: u16, reference: u16, negative: &str| {
            let [d, m, s] = tiff.rationals::<3>(find(tag)?)?;
            let value = d + m / 60.0 + s / 3600.0;
            let side = tiff.ascii(find(reference)?)?;
            Some(if side.eq_ignore_ascii_case(negative) {
                -value
            } else {
                value
            })
        };
        taken.place = angle(LAT, LAT_REF, "S")
            .zip(angle(LON, LON_REF, "W"))
            .and_then(|(lat, lon)| place(lat, lon));
        taken.alt = find(ALT).and_then(|e| tiff.rationals::<1>(e)).map(|[a]| {
            let below = find(ALT_REF).and_then(|e| tiff.value(e, 1)).and_then(|b| b.first()) == Some(&1);
            if below {
                -a
            } else {
                a
            }
        });
    }
    Some(taken)
}

/// The EXIF block of a JPEG: its APP1 segment that begins with `Exif`.
fn jpeg_block(data: &[u8]) -> Option<&[u8]> {
    if !data.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut at = 2usize;
    loop {
        let head = data.get(at..at.checked_add(4)?)?;
        // The picture itself begins: no block came before it.
        if head[0] != 0xff || head[1] == 0xda {
            return None;
        }
        let len = usize::from(u16::from_be_bytes([head[2], head[3]]));
        if len < 2 {
            return None;
        }
        let body = data.get(at + 4..(at + 2).checked_add(len)?)?;
        if head[1] == 0xe1 {
            if let Some(tiff) = body.strip_prefix(b"Exif\0\0") {
                return Some(tiff);
            }
        }
        at += 2 + len;
    }
}

/// What the picture whose first bytes are `data` says. A JPEG is read by its
/// segments; in other containers (HEIC, PNG, WebP) the block is found by its own
/// beginning, and a TIFF file is the structure itself.
pub fn of_picture(data: &[u8]) -> Taken {
    if data.starts_with(&[0xff, 0xd8]) {
        return jpeg_block(data).and_then(of_tiff).unwrap_or_default();
    }
    if let Some(taken) = of_tiff(data) {
        return taken;
    }
    let starts = |at: usize| matches!(data.get(at..at + 4), Some(b"II*\0" | b"MM\0*"));
    // `Exif\0\0` in HEIC and some WebP files; a bare structure after the name of
    // the chunk in PNG (`eXIf`) and WebP (`EXIF`).
    for (mark, skip) in [(&b"Exif\0\0"[..], 6), (&b"eXIf"[..], 4), (&b"EXIF"[..], 8)] {
        let mut from = 0;
        while let Some(i) = data
            .get(from..)
            .and_then(|rest| rest.windows(mark.len()).position(|w| w == mark))
        {
            let at = from + i + skip;
            if starts(at) {
                if let Some(taken) = data.get(at..).and_then(of_tiff) {
                    return taken;
                }
            }
            from += i + 1;
        }
    }
    Taken::default()
}

pub fn of_picture_file(path: &Path) -> std::io::Result<Taken> {
    let jpeg = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"));
    let mut head = Vec::new();
    std::fs::File::open(path)?
        .take(if jpeg { JPEG_HEAD } else { OTHER_HEAD })
        .read_to_end(&mut head)?;
    Ok(of_picture(&head))
}

/// `+48.0764+011.6428+520.000/` (ISO 6709 in decimal degrees).
fn iso6709(text: &str) -> Option<(f64, f64, Option<f64>)> {
    let text = text.trim().trim_end_matches('/');
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if (c == '+' || c == '-') && i > 0 {
            parts.push(&text[start..i]);
            start = i;
        }
    }
    parts.push(&text[start..]);
    let number = |s: &&str| {
        (s.starts_with('+') || s.starts_with('-'))
            .then(|| s.parse::<f64>().ok())
            .flatten()
    };
    let lat = parts.first().and_then(number)?;
    let lon = parts.get(1).and_then(number)?;
    Some((lat, lon, parts.get(2).and_then(number)))
}

/// What ffmpeg says about a video: the place a phone or a drone wrote into it, and
/// when it was made.
pub fn of_video(said: &str) -> Taken {
    let value = |names: &[&str]| {
        said.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            names.contains(&key.trim()).then(|| value.trim())
        })
    };
    let mut taken = Taken::default();
    if let Some((lat, lon, alt)) = value(&["location", "com.apple.quicktime.location.ISO6709"]).and_then(iso6709) {
        taken.place = place(lat, lon);
        taken.alt = alt.filter(|a| a.is_finite());
    }
    // `2026-11-25T13:02:11.000000Z`
    taken.time = value(&["creation_time"]).and_then(|t| {
        let t = t.get(..19)?;
        let day = t.get(..10)?;
        let clock = t.get(11..)?;
        (t.as_bytes()[10] == b'T' && !day.starts_with("19")).then(|| format!("{day} {clock} UTC"))
    });
    taken
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// An EXIF block as a camera writes it: a place, a height and the time. `little`
    /// is the byte order.
    pub fn block(little: bool, lat: (&str, [u32; 3]), lon: (&str, [u32; 3]), alt: u32, time: &str) -> Vec<u8> {
        let u16b = |v: u16| if little { v.to_le_bytes() } else { v.to_be_bytes() };
        let u32b = |v: u32| if little { v.to_le_bytes() } else { v.to_be_bytes() };
        let mut out: Vec<u8> = if little { b"II".to_vec() } else { b"MM".to_vec() };
        out.extend(u16b(42));
        out.extend(u32b(8));
        // IFD0 at 8: two pointers. 2 + 2*12 + 4 = 30 bytes, so Exif at 38, GPS at 56.
        let entry = |out: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: [u8; 4]| {
            out.extend(u16b(tag));
            out.extend(u16b(kind));
            out.extend(u32b(count));
            out.extend(value);
        };
        out.extend(u16b(2));
        entry(&mut out, IFD_EXIF, 4, 1, u32b(38));
        entry(&mut out, IFD_GPS, 4, 1, u32b(56));
        out.extend(u32b(0));
        // Exif IFD at 38: 2 + 12 + 4 = 18 bytes. Its text stands after the GPS data.
        let gps_len = 2 + 6 * 12 + 4;
        let rationals_at = 56 + gps_len as u32;
        let time_at = rationals_at + 3 * 8 + 3 * 8 + 8;
        out.extend(u16b(1));
        entry(&mut out, DATE_TIME_ORIGINAL, 2, 20, u32b(time_at));
        out.extend(u32b(0));
        out.extend(u16b(6));
        let side = |s: &str| [s.as_bytes()[0], 0, 0, 0];
        entry(&mut out, LAT_REF, 2, 2, side(lat.0));
        entry(&mut out, LAT, 5, 3, u32b(rationals_at));
        entry(&mut out, LON_REF, 2, 2, side(lon.0));
        entry(&mut out, LON, 5, 3, u32b(rationals_at + 24));
        entry(&mut out, ALT_REF, 1, 1, [0; 4]);
        entry(&mut out, ALT, 5, 1, u32b(rationals_at + 48));
        out.extend(u32b(0));
        for (i, v) in lat.1.iter().chain(&lon.1).enumerate() {
            out.extend(u32b(*v));
            // The seconds in thousandths.
            out.extend(u32b(if i % 3 == 2 { 1000 } else { 1 }));
        }
        out.extend(u32b(alt));
        out.extend(u32b(10));
        let mut text = time.as_bytes().to_vec();
        text.resize(20, 0);
        out.extend(text);
        out
    }

    /// A JPEG that carries `block`, with another segment before it.
    pub fn jpeg(block: &[u8]) -> Vec<u8> {
        let mut out = vec![0xff, 0xd8, 0xff, 0xe0, 0, 4, 1, 2, 0xff, 0xe1];
        out.extend(((block.len() + 8) as u16).to_be_bytes());
        out.extend(b"Exif\0\0");
        out.extend(block);
        out.extend([0xff, 0xda, 0, 2, 0xff, 0xd9]);
        out
    }

    fn munich(little: bool) -> Vec<u8> {
        block(
            little,
            ("N", [48, 4, 35040]),
            ("E", [11, 38, 34800]),
            5205,
            "2026:11:25 14:02:11",
        )
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn a_camera_s_place_height_and_time_are_read_in_both_byte_orders() {
        for little in [true, false] {
            let taken = of_picture(&jpeg(&munich(little)));
            let (lat, lon) = taken.place.unwrap_or_else(|| panic!("{taken:?}"));
            assert!(close(lat, 48.0764) && close(lon, 11.643), "{lat} {lon}");
            assert_eq!(taken.alt, Some(520.5));
            assert_eq!(taken.time.as_deref(), Some("2026-11-25 14:02:11"));
        }
    }

    #[test]
    fn south_and_west_are_negative() {
        let b = block(true, ("S", [33, 51, 36000]), ("W", [151, 12, 0]), 0, "x");
        let taken = of_picture(&jpeg(&b));
        let (lat, lon) = taken.place.unwrap();
        assert!(close(lat, -33.86) && close(lon, -151.2), "{lat} {lon}");
        assert_eq!(taken.time, None);
    }

    #[test]
    fn the_block_is_found_in_other_containers_too() {
        let b = munich(false);
        let wrap = |before: &[u8]| {
            let mut out = before.to_vec();
            out.extend(&b);
            out
        };
        for data in [
            b.clone(),
            wrap(b"....ftypheic........Exif\0\0"),
            wrap(b"\x89PNG\r\n\x1a\n\0\0\0\x80eXIf"),
            wrap(b"RIFF\0\0\0\0WEBPEXIF\x80\0\0\0"),
        ] {
            assert!(of_picture(&data).place.is_some(), "{:?}", &data[..12]);
        }
    }

    #[test]
    fn no_fix_is_no_place() {
        let b = block(true, ("N", [0, 0, 0]), ("E", [0, 0, 0]), 0, "0000:00:00 00:00:00");
        assert_eq!(of_picture(&jpeg(&b)).place, None);
        assert_eq!(of_picture(&jpeg(&b)).time, None);
        assert_eq!(of_picture(b"not a picture at all"), Taken::default());
        assert_eq!(of_picture(&[]), Taken::default());
    }

    #[test]
    fn a_damaged_file_gives_nothing_and_never_panics() {
        let whole = jpeg(&munich(true));
        for len in 0..whole.len() {
            let _ = of_picture(&whole[..len]);
        }
        // Every byte of the block turned into the largest value, one at a time.
        for i in 0..whole.len() {
            let mut broken = whole.clone();
            broken[i] = 0xff;
            let _ = of_picture(&broken);
        }
    }

    #[test]
    fn a_video_s_place_and_time_are_read_from_what_ffmpeg_says() {
        let said =
            "Input #0, mov,mp4, from 'a.mp4':\n  Metadata:\n    creation_time   : 2026-11-25T13:02:11.000000Z\n    \
                    location        : +48.0764+011.6430+520.500/\n  Duration: 00:00:12.00\n";
        assert_eq!(
            of_video(said),
            Taken {
                place: Some((48.0764, 11.643)),
                alt: Some(520.5),
                time: Some("2026-11-25 13:02:11 UTC".into()),
            }
        );
        let apple = "    com.apple.quicktime.location.ISO6709: -33.8600-151.2000/\n";
        assert_eq!(of_video(apple).place, Some((-33.86, -151.2)));
        for none in [
            "",
            "location: here",
            "location: +91.0+011.0/",
            "location: +00.0000+000.0000/",
            "creation_time: 1904",
        ] {
            assert_eq!(of_video(none), Taken::default(), "{none}");
        }
    }
}
