//! Image metrics over quen's own screenshots (finding 39): each encoding `game_screenshot` writes, decoded, a crop of
//! it, and the numbers agents wrote into measurement rows by hand: channel means and medians, luminance, the share of
//! pixels in a hue range, and the difference from a reference against a noise floor.
//!
//! Every number keeps its encoding's meaning. The 8-bit sRGB PNG (`srgb8`) has no absolute scale, so its light is
//! relative (linear sRGB, white = 1), never nits. The 16-bit PQ BT.2020 PNG (`pq_bt2020_u16`) and the scRGB `.npy`
//! (`scrgb_f16`) give nits, in BT.709 primaries both, so the two files of one HDR shot agree; hue and differences read
//! both as PQ BT.2020 code values, the `.npy` encoded as the PNG is.

use std::io::Cursor;

use half::f16;
use serde::Serialize;

use crate::hdr::{self, BT709_LUMA, BT2020_TO_BT709, Encoding, Region, mat3};
use crate::stats::summarise;

/// One number for a measurement row.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Row {
    pub metric: String,
    pub value: f64,
    pub unit: &'static str,
}

/// A decoded screenshot: R, G, B per pixel, row-major from the top, as its encoding stores them (srgb8 codes 0 to 255,
/// PQ code values 0 to 1, scRGB values); alpha dropped.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    width: u32,
    height: u32,
    encoding: Encoding,
    rgb: Vec<f32>,
}

const NO_PIXEL: &str = "the crop has no pixel with finite values";

impl Image {
    /// The file's bytes as `encoding` says they are: an 8-bit RGB PNG, a 16-bit RGB PNG with cICP BT.2020 PQ, a
    /// little-endian float16 `.npy` of shape (height, width, 4). Refuses any other file, and `pq_codes_f16` (a devkit
    /// EXR: no file quen takes).
    pub fn decode(bytes: &[u8], encoding: Encoding) -> Result<Image, String> {
        let (width, height, rgb) = match encoding {
            Encoding::Srgb8 => png(bytes, false)?,
            Encoding::PqBt2020U16 => png(bytes, true)?,
            Encoding::ScrgbF16 => npy(bytes)?,
            Encoding::PqCodesF16 => {
                return Err(
                    "pq_codes_f16 (a devkit EXR) isn't an encoding quen's screenshots use".into(),
                );
            }
        };
        if width == 0 || height == 0 {
            return Err(format!("a {width}x{height} image"));
        }
        Ok(Image {
            width,
            height,
            encoding,
            rgb,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// The crop `[x, y, width, height]` in whole pixels from the top-left, or with `fraction` in fractions of the
    /// frame's width and height (each edge rounded to the nearest pixel); None: the whole frame.
    pub fn region(&self, crop: Option<[f64; 4]>, fraction: bool) -> Result<Region, String> {
        let (w, h) = (self.width, self.height);
        let Some([x, y, cw, ch]) = crop else {
            return Ok(Region {
                x: 0,
                y: 0,
                width: w,
                height: h,
            });
        };
        let [x0, y0, x1, y1] = if fraction {
            if [x, y, cw, ch].iter().any(|v| !(0.0..=1.0).contains(v)) {
                return Err("a fraction crop is 4 numbers from 0 to 1".into());
            }
            let edge = |f: f64, n: u32| (f * f64::from(n)).round();
            [edge(x, w), edge(y, h), edge(x + cw, w), edge(y + ch, h)]
        } else {
            if [x, y, cw, ch].iter().any(|v| *v < 0.0 || v.fract() != 0.0) {
                return Err("a pixel crop is 4 whole numbers from 0".into());
            }
            [x, y, x + cw, y + ch]
        };
        if x1 <= x0 || y1 <= y0 || x1 > f64::from(w) || y1 > f64::from(h) {
            return Err(format!(
                "crop [{x}, {y}, {cw}, {ch}] is empty or leaves the {w}x{h} frame"
            ));
        }
        Ok(Region {
            x: x0 as u32,
            y: y0 as u32,
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
        })
    }

    /// Refuses a region that is empty or leaves the frame (one not from [`Image::region`]).
    fn inside(&self, r: Region) -> Result<(), String> {
        let fits =
            |at: u32, len: u32, max: u32| len > 0 && at.checked_add(len).is_some_and(|e| e <= max);
        match fits(r.x, r.width, self.width) && fits(r.y, r.height, self.height) {
            true => Ok(()),
            false => Err(format!(
                "region {}x{} at ({}, {}) is empty or leaves the {}x{} frame",
                r.width, r.height, r.x, r.y, self.width, self.height
            )),
        }
    }

    /// Every pixel of `r`, as stored.
    fn all(&self, r: Region) -> impl Iterator<Item = [f32; 3]> + '_ {
        (r.y..r.y + r.height).flat_map(move |y| {
            let start = (y as usize * self.width as usize + r.x as usize) * 3;
            self.rgb[start..start + r.width as usize * 3]
                .chunks_exact(3)
                .map(|p| [p[0], p[1], p[2]])
        })
    }

    /// The pixels of `r` whose three values are finite: every metric leaves the others out.
    fn pixels(&self, r: Region) -> impl Iterator<Item = [f32; 3]> + '_ {
        self.all(r).filter(|p| finite(*p))
    }

    /// How many pixels of `r` the metrics leave out: a NaN or infinity in the `.npy` (the PNGs hold none).
    pub fn non_finite(&self, r: Region) -> Result<usize, String> {
        self.inside(r)?;
        Ok(self.all(r).filter(|p| !finite(*p)).count())
    }

    /// The unit token of the stored values: `srgb8`, `pq` or `scrgb`.
    fn code_unit(&self) -> &'static str {
        match self.encoding {
            Encoding::Srgb8 => "srgb8",
            Encoding::PqBt2020U16 | Encoding::PqCodesF16 => "pq",
            Encoding::ScrgbF16 => "scrgb",
        }
    }

    /// The unit of linear light: `relative` for srgb8 (sRGB white = 1), else `nits`.
    fn light_unit(&self) -> &'static str {
        match self.encoding {
            Encoding::Srgb8 => "relative",
            _ => "nits",
        }
    }

    /// Linear light in BT.709 primaries: relative for srgb8, nits for the HDR files.
    fn light(&self, p: [f32; 3]) -> [f32; 3] {
        match self.encoding {
            Encoding::Srgb8 => p.map(|c| srgb_to_linear(c / 255.0)),
            Encoding::PqBt2020U16 | Encoding::PqCodesF16 => {
                mat3(&BT2020_TO_BT709, p.map(hdr::pq_to_nits))
            }
            Encoding::ScrgbF16 => p.map(hdr::scrgb_to_nits),
        }
    }

    /// What hue and differences read: srgb8 codes as stored, PQ BT.2020 code values for the HDR files (scRGB encoded
    /// as the PNG is: negatives and above 10,000 nits clamp).
    fn coded(&self, p: [f32; 3]) -> [f32; 3] {
        match self.encoding {
            Encoding::ScrgbF16 => {
                hdr::bt709_to_bt2020(p.map(hdr::scrgb_to_nits)).map(hdr::nits_to_pq)
            }
            _ => p,
        }
    }

    /// Per channel R, G, B: the mean and median of the stored values (`<c>_<srgb8|pq|scrgb>_mean`, `_median`), and the
    /// mean of linear light (`<c>_lin_mean`, relative or nits).
    pub fn channels(&self, r: Region) -> Result<Vec<Row>, String> {
        self.inside(r)?;
        let mut lin = [0.0; 3];
        for p in self.pixels(r) {
            for (sum, v) in lin.iter_mut().zip(self.light(p)) {
                *sum += f64::from(v);
            }
        }
        let mut rows = Vec::new();
        for (c, name) in ["r", "g", "b"].into_iter().enumerate() {
            let s = summarise(self.pixels(r).map(|p| f64::from(p[c])).collect()).ok_or(NO_PIXEL)?;
            let u = self.code_unit();
            rows.extend([
                row(format!("{name}_{u}_mean"), s.mean, u),
                row(format!("{name}_{u}_median"), s.median, u),
                row(
                    format!("{name}_lin_mean"),
                    lin[c] / s.count as f64,
                    self.light_unit(),
                ),
            ]);
        }
        Ok(rows)
    }

    /// BT.709 luminance of linear light: mean, median, p95 and max (`luminance_<nits|relative>_<stat>`).
    pub fn luminance(&self, r: Region) -> Result<Vec<Row>, String> {
        self.inside(r)?;
        let y: Vec<f64> = self.pixels(r).map(|p| luma(self.light(p))).collect();
        let max = y.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let s = summarise(y).ok_or(NO_PIXEL)?;
        let u = self.light_unit();
        Ok([
            ("mean", s.mean),
            ("median", s.median),
            ("p95", s.p95),
            ("max", max),
        ]
        .into_iter()
        .map(|(stat, v)| row(format!("luminance_{u}_{stat}"), v, u))
        .collect())
    }

    /// The share of pixels whose HSV hue lies from `hue[0]` to `hue[1]` degrees (both included; from above to wraps
    /// through 0) with saturation at least `s_min` and value at least `v_min`, over srgb8 codes / 255 or PQ BT.2020 code
    /// values. A gray pixel has no hue. `hue_<from>_<to>_share`.
    pub fn hue_share(
        &self,
        r: Region,
        hue: [u32; 2],
        s_min: f64,
        v_min: f64,
    ) -> Result<Row, String> {
        self.inside(r)?;
        let scale = if self.encoding == Encoding::Srgb8 {
            255.0
        } else {
            1.0
        };
        let [from, to] = hue.map(f64::from);
        let (mut n, mut hits) = (0u64, 0u64);
        for p in self.pixels(r) {
            n += 1;
            // Equal stored channels are gray, whatever the conversion to PQ codes rounds them to.
            if p[0] == p[1] && p[1] == p[2] {
                continue;
            }
            let [red, green, blue] = self.coded(p).map(|v| f64::from(v) / scale);
            let max = red.max(green).max(blue);
            let d = max - red.min(green).min(blue);
            if d <= 0.0 || max < v_min || d / max < s_min {
                continue;
            }
            let h = if max == red {
                60.0 * ((green - blue) / d).rem_euclid(6.0)
            } else if max == green {
                60.0 * ((blue - red) / d + 2.0)
            } else {
                60.0 * ((red - green) / d + 4.0)
            };
            hits += u64::from(if from <= to {
                (from..=to).contains(&h)
            } else {
                h >= from || h <= to
            });
        }
        if n == 0 {
            return Err(NO_PIXEL.into());
        }
        Ok(row(
            format!("hue_{}_{}_share", hue[0], hue[1]),
            hits as f64 / n as f64,
            "share",
        ))
    }

    /// The difference from `reference` over `r`: the mean of each pixel's largest |a − b| over R, G and B, in srgb8
    /// levels or PQ code units (`diff_<srgb8|pq>_mean`). With `noise`, a second capture of this one's state: the same
    /// against it (`floor_<u>_mean`) and the ratio (`diff_over_floor`, left out when the floor is 0).
    pub fn diff_rows(
        &self,
        reference: &Image,
        noise: Option<&Image>,
        r: Region,
    ) -> Result<Vec<Row>, String> {
        let u = if self.encoding == Encoding::Srgb8 {
            "srgb8"
        } else {
            "pq"
        };
        let diff = self.diff(reference, r)?;
        let mut rows = vec![row(format!("diff_{u}_mean"), diff, u)];
        if let Some(noise) = noise {
            let floor = self.diff(noise, r)?;
            rows.push(row(format!("floor_{u}_mean"), floor, u));
            if floor > 0.0 {
                rows.push(row("diff_over_floor".into(), diff / floor, "ratio"));
            }
        }
        Ok(rows)
    }

    /// Refuses images of another size or encoding: within a comparison the capture path stays the same.
    fn diff(&self, other: &Image, r: Region) -> Result<f64, String> {
        self.inside(r)?;
        let (a, b) = (
            (self.width, self.height, self.encoding),
            (other.width, other.height, other.encoding),
        );
        if a != b {
            return Err(format!(
                "comparison refused: {}x{} {} vs {}x{} {}",
                a.0, a.1, a.2, b.0, b.1, b.2
            ));
        }
        let (mut sum, mut n) = (0.0, 0u64);
        for (pa, pb) in self
            .all(r)
            .zip(other.all(r))
            .filter(|(a, b)| finite(*a) && finite(*b))
        {
            let (pa, pb) = (self.coded(pa), other.coded(pb));
            sum += f64::from((0..3).map(|c| (pa[c] - pb[c]).abs()).fold(0.0, f32::max));
            n += 1;
        }
        match n {
            0 => Err(NO_PIXEL.into()),
            n => Ok(sum / n as f64),
        }
    }
}

fn row(metric: String, value: f64, unit: &'static str) -> Row {
    Row {
        metric,
        value,
        unit,
    }
}

fn finite(p: [f32; 3]) -> bool {
    p.iter().all(|v| v.is_finite())
}

fn luma(rgb: [f32; 3]) -> f64 {
    rgb.iter()
        .zip(BT709_LUMA)
        .map(|(&v, w)| f64::from(v) * w)
        .sum()
}

/// The sRGB EOTF (IEC 61966-2-1), a code in 0..=1 to linear light.
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// An RGB or RGBA PNG: 8-bit (`pq` false), or 16-bit with cICP BT.2020 PQ (`pq`), as capture.rs and
/// [`hdr::Frame::write_png_pq`] write them. Width, height, and R, G, B per pixel (16-bit codes / 65535).
fn png(bytes: &[u8], pq: bool) -> Result<(u32, u32, Vec<f32>), String> {
    // A 4K 16-bit frame is about 50 MB of pixels: above the decoder's default 64 MiB for anything larger.
    let limits = png::Limits { bytes: 1 << 30 };
    let mut reader = png::Decoder::new_with_limits(Cursor::new(bytes), limits)
        .read_info()
        .map_err(|e| format!("PNG: {e}"))?;
    let cicp = reader
        .info()
        .coding_independent_code_points
        .map(|c| (c.color_primaries, c.transfer_function));
    if pq && cicp != Some((9, 16)) {
        return Err(format!(
            "a pq_bt2020_u16 PNG carries cICP BT.2020 PQ (9, 16); this one {cicp:?}"
        ));
    }
    let size = reader
        .output_buffer_size()
        .ok_or("PNG: too large to decode")?;
    let mut buf = vec![0; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("PNG: {e}"))?;
    let channels = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        other => return Err(format!("a {other:?} PNG, not RGB")),
    };
    let (depth, bytes_per) = if pq {
        (png::BitDepth::Sixteen, 2)
    } else {
        (png::BitDepth::Eight, 1)
    };
    if info.bit_depth != depth {
        return Err(format!(
            "a {}-bit PNG where its encoding needs {} bits",
            info.bit_depth as u8, depth as u8
        ));
    }
    let (w, h) = (info.width as usize, info.height as usize);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for line in buf.chunks_exact(info.line_size).take(h) {
        for px in line[..w * channels * bytes_per].chunks_exact(channels * bytes_per) {
            for c in 0..3 {
                rgb.push(match pq {
                    true => f32::from(u16::from_be_bytes([px[c * 2], px[c * 2 + 1]])) / 65535.0,
                    false => f32::from(px[c]),
                });
            }
        }
    }
    Ok((info.width, info.height, rgb))
}

/// A `.npy` as [`hdr::Frame::write_npy`] writes it: version 1.0, `<f2`, C order, shape (height, width, 4).
fn npy(bytes: &[u8]) -> Result<(u32, u32, Vec<f32>), String> {
    let bad = |why: &str| format!("not a float16 RGBA .npy as quen writes it: {why}");
    let rest = bytes
        .strip_prefix(b"\x93NUMPY\x01\x00")
        .ok_or_else(|| bad("no version 1.0 NUMPY magic"))?;
    let len = rest.get(..2).ok_or_else(|| bad("truncated"))?;
    let len = usize::from(u16::from_le_bytes([len[0], len[1]]));
    let header = rest
        .get(2..2 + len)
        .and_then(|h| std::str::from_utf8(h).ok())
        .ok_or_else(|| bad("truncated header"))?;
    if !header.contains("'descr': '<f2'") || !header.contains("'fortran_order': False") {
        return Err(bad(header.trim()));
    }
    let shape = header
        .split("'shape': (")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .ok_or_else(|| bad("no shape"))?;
    let dims: Vec<usize> = shape
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| bad(shape))?;
    let [h, w, 4] = dims[..] else {
        return Err(bad(&format!("shape ({shape})")));
    };
    let data = &rest[2 + len..];
    if Some(data.len()) != h.checked_mul(w).and_then(|n| n.checked_mul(8)) {
        return Err(bad(&format!(
            "{} data bytes for shape ({shape})",
            data.len()
        )));
    }
    let size = |n: usize| u32::try_from(n).map_err(|_| bad("too large"));
    let rgb = data
        .chunks_exact(8)
        .flat_map(|px| (0..3).map(move |c| f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32()))
        .collect();
    Ok((size(w)?, size(h)?, rgb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::{Frame, Pixels, SCRGB_WHITE_NITS};

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    /// `rows` as (metric, value) pairs, for lookups.
    fn value(rows: &[Row], metric: &str) -> f64 {
        rows.iter()
            .find(|r| r.metric == metric)
            .unwrap_or_else(|| panic!("no {metric} in {rows:?}"))
            .value
    }

    /// An 8-bit RGB PNG of `px`, as capture.rs encodes the SDR grab.
    fn srgb8(width: u32, height: u32, px: &[[u8; 3]]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&px.concat()).unwrap();
        writer.finish().unwrap();
        out
    }

    /// An scRGB frame of `px` (RGB in scRGB units, alpha 1), and quen's two HDR files of it: the .npy and the PQ PNG.
    fn hdr_files(width: u32, height: u32, px: &[[f32; 3]]) -> (Vec<u8>, Vec<u8>) {
        let s = px
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 1.0])
            .map(f16::from_f32)
            .collect();
        let frame = Frame::new(width, height, Pixels::ScrgbF16(s)).unwrap();
        let (mut npy, mut png) = (Vec::new(), Vec::new());
        frame.write_npy(&mut npy).unwrap();
        frame.write_png_pq(&mut png).unwrap();
        (npy, png)
    }

    fn nits(n: f32) -> [f32; 3] {
        [n / SCRGB_WHITE_NITS; 3]
    }

    /// srgb8, hand-computed: red, green, blue and gray 128 in a 2x2 frame.
    #[test]
    fn srgb8_metrics_match_hand_values() {
        let file = srgb8(
            2,
            2,
            &[[255, 0, 0], [0, 255, 0], [0, 0, 255], [128, 128, 128]],
        );
        let img = Image::decode(&file, Encoding::Srgb8).unwrap();
        let all = img.region(None, false).unwrap();
        let ch = img.channels(all).unwrap();
        // R: 255, 0, 0, 128: mean 95.75, median between 0 and 128.
        assert_eq!(value(&ch, "r_srgb8_mean"), 95.75);
        assert_eq!(value(&ch, "r_srgb8_median"), 64.0);
        let gray = ((128.0 / 255.0 + 0.055) / 1.055f64).powf(2.4);
        assert!(
            close(value(&ch, "r_lin_mean"), (1.0 + gray) / 4.0, 1e-6),
            "{ch:?}"
        );
        assert!(ch.iter().all(|r| r.unit
            == if r.metric.contains("lin") {
                "relative"
            } else {
                "srgb8"
            }));

        let y = img.luminance(all).unwrap();
        // Y: 0.2126, 0.7152, 0.0722 and the gray's linear value; sorted 0.0722, 0.2126, gray, 0.7152.
        assert!(close(
            value(&y, "luminance_relative_mean"),
            (0.2126 + 0.7152 + 0.0722 + gray) / 4.0,
            1e-6
        ));
        assert!(close(
            value(&y, "luminance_relative_median"),
            (0.2126 + gray) / 2.0,
            1e-6
        ));
        assert!(close(value(&y, "luminance_relative_max"), 0.7152, 1e-6));
        // p95: rank 2.85, between gray and 0.7152.
        assert!(close(
            value(&y, "luminance_relative_p95"),
            gray + 0.85 * (0.7152 - gray),
            1e-6
        ));

        // Hues 0, 120 and 240; gray has none.
        let share = |hue, s, v| img.hue_share(all, hue, s, v).unwrap().value;
        assert_eq!(share([200, 280], 0.0, 0.0), 0.25);
        assert_eq!(share([350, 10], 0.0, 0.0), 0.25, "wraps through 0");
        assert_eq!(share([0, 360], 0.0, 0.0), 0.75);
        assert_eq!(share([0, 360], 0.0, 1.0), 0.75, "v 1 is the full code");
        assert_eq!(
            img.hue_share(all, [200, 280], 0.0, 0.0).unwrap().metric,
            "hue_200_280_share"
        );

        // A crop: the right column, in pixels and as fractions.
        let right = img.region(Some([1.0, 0.0, 1.0, 2.0]), false).unwrap();
        assert_eq!(img.region(Some([0.5, 0.0, 0.5, 1.0]), true).unwrap(), right);
        assert_eq!(value(&img.channels(right).unwrap(), "g_srgb8_mean"), 191.5);
    }

    /// The PQ PNG and the .npy of one HDR shot, hand-computed in nits: black and 10,000 nits are exact in PQ; 80 and
    /// 400 nits, and red at 80 nits a channel, within a 16-bit PQ step.
    #[test]
    fn hdr_files_give_nits_and_agree() {
        let px = [
            nits(0.0),
            nits(10_000.0),
            nits(80.0),
            nits(400.0),
            [1.0, 0.0, 0.0],
            nits(400.0),
        ];
        let (npy, png) = hdr_files(3, 2, &px);
        let scrgb = Image::decode(&npy, Encoding::ScrgbF16).unwrap();
        let pq = Image::decode(&png, Encoding::PqBt2020U16).unwrap();
        assert_eq!(
            (scrgb.width(), scrgb.height(), pq.width(), pq.height()),
            (3, 2, 3, 2)
        );

        let all = scrgb.region(None, false).unwrap();
        let red = 0.2126 * 80.0;
        // Y: 0, 10000, 80, 400, 17.008, 400: sorted 0, 17.008, 80, 400, 400, 10000.
        let mean = (10_000.0 + 80.0 + 400.0 + red + 400.0) / 6.0;
        for (img, tol) in [(&scrgb, 0.05), (&pq, 0.5)] {
            let y = img.luminance(all).unwrap();
            assert!(
                close(value(&y, "luminance_nits_mean"), mean, tol * 6.0),
                "{:?}: {y:?}",
                img.encoding()
            );
            assert!(
                close(value(&y, "luminance_nits_median"), 240.0, tol * 2.0),
                "{y:?}"
            );
            assert!(
                close(value(&y, "luminance_nits_max"), 10_000.0, 1.0),
                "{y:?}"
            );
            // p95: rank 4.75, between 400 and 10,000.
            assert!(
                close(value(&y, "luminance_nits_p95"), 400.0 + 0.75 * 9600.0, 2.0),
                "{y:?}"
            );
            assert!(y.iter().all(|r| r.unit == "nits"));
            let ch = img.channels(all).unwrap();
            // Linear R in BT.709: 0, 10000, 80, 400, 80, 400.
            assert!(
                close(value(&ch, "r_lin_mean"), 10_960.0 / 6.0, tol * 6.0),
                "{ch:?}"
            );
            assert!(
                close(value(&ch, "g_lin_mean"), 10_880.0 / 6.0, tol * 6.0),
                "{ch:?}"
            );
        }
        // Stored values: scRGB as is (R: 0, 125, 1, 5, 1, 5), PQ as codes 0 to 1 (black 0, 10,000 nits 1).
        let ch = scrgb.channels(all).unwrap();
        assert!(close(value(&ch, "r_scrgb_mean"), 137.0 / 6.0, 1e-3));
        assert_eq!(value(&ch, "r_scrgb_median"), 3.0);
        let corner = pq.region(Some([0.0, 0.0, 2.0, 1.0]), false).unwrap();
        let ch = pq.channels(corner).unwrap();
        assert_eq!(value(&ch, "g_pq_mean"), 0.5);
        assert!(ch.iter().any(|r| r.unit == "pq"));

        // Hue over PQ BT.2020 codes: only the red pixel has one, and both files agree.
        for img in [&scrgb, &pq] {
            assert!(close(
                img.hue_share(all, [350, 30], 0.1, 0.0).unwrap().value,
                1.0 / 6.0,
                1e-9
            ));
            assert_eq!(img.hue_share(all, [60, 300], 0.0, 0.0).unwrap().value, 0.0);
        }
    }

    /// diff: srgb8 levels and PQ code units, the floor and the ratio; a size or encoding mismatch refuses.
    #[test]
    fn diff_against_a_reference_and_a_floor() {
        let base = [[100u8, 100, 100]; 4];
        let mut moved = base;
        moved[3] = [100, 120, 90];
        let mut noisy = base;
        noisy[0] = [102, 100, 100];
        let img = |px: &[[u8; 3]]| Image::decode(&srgb8(2, 2, px), Encoding::Srgb8).unwrap();
        let (a, b, n) = (img(&base), img(&moved), img(&noisy));
        let all = a.region(None, false).unwrap();
        let rows = a.diff_rows(&b, Some(&n), all).unwrap();
        // The largest |a - b| is 20 levels in one pixel of four, the floor 2 in one.
        assert_eq!(value(&rows, "diff_srgb8_mean"), 5.0);
        assert_eq!(value(&rows, "floor_srgb8_mean"), 0.5);
        assert_eq!(value(&rows, "diff_over_floor"), 10.0);
        let same = a.diff_rows(&a, Some(&a), all).unwrap();
        assert_eq!(same.len(), 2, "no ratio over a zero floor: {same:?}");

        // HDR: one pixel from 100 to 1000 nits in green: PQ(1000) - PQ(100) in BT.2020 green, a quarter on average.
        let mut px = [nits(100.0); 4];
        let (npy_a, png_a) = hdr_files(2, 2, &px);
        px[3][1] = 1000.0 / SCRGB_WHITE_NITS;
        let (npy_b, _) = hdr_files(2, 2, &px);
        let (sa, sb) = (
            Image::decode(&npy_a, Encoding::ScrgbF16).unwrap(),
            Image::decode(&npy_b, Encoding::ScrgbF16).unwrap(),
        );
        let step = |n: f32| hdr::nits_to_pq(hdr::bt709_to_bt2020([100.0, n, 100.0])[1]);
        let want = f64::from(step(1000.0) - step(100.0)) / 4.0;
        let rows = sa.diff_rows(&sb, None, all).unwrap();
        assert!(
            close(value(&rows, "diff_pq_mean"), want, 1e-6),
            "{rows:?} vs {want}"
        );
        let pq = Image::decode(&png_a, Encoding::PqBt2020U16).unwrap();
        let tall = Image::decode(&srgb8(1, 4, &base), Encoding::Srgb8).unwrap();
        for (x, y) in [(&sa, &pq), (&a, &sa), (&a, &tall)] {
            assert!(
                x.diff_rows(y, None, all)
                    .unwrap_err()
                    .contains("comparison refused")
            );
        }
    }

    /// Refusals: a file that isn't what its encoding says, a crop outside the frame, a region not from `region`.
    #[test]
    fn refuses_what_it_cannot_read() {
        let sdr = srgb8(2, 1, &[[1, 2, 3], [4, 5, 6]]);
        let (npy, png) = hdr_files(2, 1, &[nits(1.0), nits(2.0)]);
        let short = npy[..npy.len() - 2].to_vec();
        for (bytes, enc, why) in [
            (&sdr, Encoding::PqBt2020U16, "cICP"),
            (&png, Encoding::Srgb8, "16-bit PNG"),
            (&npy, Encoding::Srgb8, "PNG"),
            (&sdr, Encoding::ScrgbF16, "NUMPY"),
            (&short, Encoding::ScrgbF16, "data bytes"),
            (&npy, Encoding::PqCodesF16, "devkit EXR"),
        ] {
            let err = Image::decode(bytes, enc).unwrap_err();
            assert!(err.contains(why), "{enc}: {err}");
        }
        let mut f4 = npy.clone();
        let at = f4.windows(4).position(|w| w == b"<f2'").unwrap();
        f4[at + 2] = b'4';
        assert!(Image::decode(&f4, Encoding::ScrgbF16).is_err());

        let img = Image::decode(&sdr, Encoding::Srgb8).unwrap();
        for (crop, fraction) in [
            ([0.0, 0.0, 3.0, 1.0], false),
            ([1.0, 0.0, 0.0, 1.0], false),
            ([0.5, 0.0, 1.0, 1.0], false),
            ([0.0, 0.0, 1.0, 1.5], true),
            ([0.0, 0.0, 0.1, 1.0], true),
        ] {
            assert!(
                img.region(Some(crop), fraction).is_err(),
                "{crop:?} {fraction}"
            );
        }
        let outside = Region {
            x: 1,
            y: 0,
            width: 2,
            height: 1,
        };
        assert!(img.channels(outside).is_err() && img.luminance(outside).is_err());
    }

    /// A NaN pixel in the .npy is left out of every metric and counted.
    #[test]
    fn non_finite_pixels_are_left_out() {
        let (mut npy, _) = hdr_files(2, 1, &[nits(80.0), nits(160.0)]);
        let n = npy.len();
        npy[n - 8..n - 6].copy_from_slice(&f16::NAN.to_le_bytes());
        let img = Image::decode(&npy, Encoding::ScrgbF16).unwrap();
        let all = img.region(None, false).unwrap();
        assert_eq!(img.non_finite(all).unwrap(), 1);
        assert!(close(
            value(&img.luminance(all).unwrap(), "luminance_nits_mean"),
            80.0,
            0.05
        ));
        let only = img.region(Some([1.0, 0.0, 1.0, 1.0]), false).unwrap();
        assert!(img.channels(only).unwrap_err().contains("finite"));
    }
}
