//! HDR frame encoder: the pure half of quen's own HDR capture, fed by the Win32 grab later.
//!
//! Input is what Windows.Graphics.Capture delivers in FP16 mode: RGBA half floats in scRGB (linear BT.709 primaries,
//! 1.0 = 80 nits, values may exceed 1 or go negative). Outputs: a 16-bit PQ BT.2020 PNG with cICP, a half RGBA linear
//! EXR, a float16 `.npy`. Plus the two numbers the A/B image checks use: a region mean in nits and an A~A diff
//! in PQ code units.
//!
//! Every frame carries its [`Encoding`], and linear-light operations refuse PQ code values until
//! [`Frame::decode_pq`] has turned them into light: earlier work twice treated PQ codes as linear.

use std::fmt;
use std::io::{Seek, Write};

use exr::prelude::{Image, SpecificChannels, Vec2, WritableImage};
use half::f16;
use serde::{Deserialize, Serialize};

/// Nits of scRGB 1.0.
pub const SCRGB_WHITE_NITS: f32 = 80.0;
/// Nits of PQ code value 1.0, the SMPTE ST 2084 reference peak.
pub const PQ_PEAK_NITS: f32 = 10_000.0;
/// cICP for the PNG (ITU-T H.273): BT.2020 primaries, PQ transfer, identity matrix (RGB), full range.
pub const CICP_PQ_BT2020: [u8; 4] = [9, 16, 0, 1];

// SMPTE ST 2084 constants.
const M1: f64 = 2610.0 / 16384.0;
const M2: f64 = 2523.0 / 4096.0 * 128.0;
const C1: f64 = 3424.0 / 4096.0;
const C2: f64 = 2413.0 / 4096.0 * 32.0;
const C3: f64 = 2392.0 / 4096.0 * 32.0;

/// Linear BT.709 RGB to linear BT.2020 RGB (ITU-R BT.2087).
const BT709_TO_BT2020: [[f64; 3]; 3] = [
    [
        0.627_403_895_934_699,
        0.329_283_038_377_884,
        0.043_313_065_687_417,
    ],
    [
        0.069_097_289_358_232,
        0.919_540_395_075_459,
        0.011_362_315_566_309,
    ],
    [
        0.016_391_438_875_150,
        0.088_013_307_877_226,
        0.895_595_253_247_624,
    ],
];
/// Its inverse, for decoding PQ BT.2020 back to scRGB.
pub(crate) const BT2020_TO_BT709: [[f64; 3]; 3] = [
    [
        1.660_491_002_108_434,
        -0.587_641_138_788_550,
        -0.072_849_863_319_885,
    ],
    [
        -0.124_550_474_521_591,
        1.132_899_897_125_961,
        -0.008_349_422_604_369,
    ],
    [
        -0.018_150_763_354_905,
        -0.100_578_898_008_007,
        1.118_729_661_362_913,
    ],
];
/// BT.709 luminance weights.
pub(crate) const BT709_LUMA: [f64; 3] = [0.2126, 0.7152, 0.0722];

/// scRGB value to absolute nits.
pub fn scrgb_to_nits(v: f32) -> f32 {
    v * SCRGB_WHITE_NITS
}

/// Linear BT.709 RGB to linear BT.2020 RGB, in any unit.
pub fn bt709_to_bt2020(rgb: [f32; 3]) -> [f32; 3] {
    mat3(&BT709_TO_BT2020, rgb)
}

/// Absolute nits to a PQ code value in 0..=1 (SMPTE ST 2084 inverse EOTF). Negatives and NaN clamp to 0, anything
/// above 10,000 nits to 1. 0 nits gives c1^m2 ≈ 7.3e-7, which is code 0 at 16 bits.
pub fn nits_to_pq(nits: f32) -> f32 {
    let y = f64::from(unit(nits / PQ_PEAK_NITS)).powf(M1);
    ((C1 + C2 * y) / (1.0 + C3 * y)).powf(M2) as f32
}

/// A PQ code value (clamped to 0..=1) to absolute nits (SMPTE ST 2084 EOTF).
pub fn pq_to_nits(code: f32) -> f32 {
    let e = f64::from(unit(code)).powf(1.0 / M2);
    ((e - C1).max(0.0) / (C2 - C3 * e)).powf(1.0 / M1) as f32 * PQ_PEAK_NITS
}

/// Clamps to 0..=1, NaN to 0.
fn unit(v: f32) -> f32 {
    if v > 0.0 { v.min(1.0) } else { 0.0 }
}

pub(crate) fn mat3(m: &[[f64; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    let v = v.map(f64::from);
    m.map(|row| (row[0] * v[0] + row[1] * v[1] + row[2] * v[2]) as f32)
}

/// The first three samples of an scRGB pixel in absolute nits.
fn nits(p: &[f16]) -> [f32; 3] {
    [p[0], p[1], p[2]].map(|v| scrgb_to_nits(v.to_f32()))
}

/// What a capture's samples mean. The serde names are the ledger's `encoding` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    /// 8-bit sRGB codes: the SDR grab. It has no absolute scale, so it is never a [`Frame`].
    Srgb8,
    /// 16-bit PQ code values, BT.2020 primaries: the PNG written here.
    PqBt2020U16,
    /// Half floats holding PQ code values, not light: a devkit swapchain EXR.
    PqCodesF16,
    /// Half floats of linear light in scRGB: what Windows.Graphics.Capture delivers in FP16 mode.
    ScrgbF16,
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Encoding::Srgb8 => "srgb8",
            Encoding::PqBt2020U16 => "pq_bt2020_u16",
            Encoding::PqCodesF16 => "pq_codes_f16",
            Encoding::ScrgbF16 => "scrgb_f16",
        })
    }
}

/// A frame's samples: RGBA, row-major from the top row, stored as their encoding names them.
#[derive(Clone, Debug, PartialEq)]
pub enum Pixels {
    PqBt2020U16(Vec<u16>),
    PqCodesF16(Vec<f16>),
    ScrgbF16(Vec<f16>),
}

/// A rectangle of a frame, in pixels from its top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The A~A / A/B image metric in PQ code units (1.0 spans 0 to 10,000 nits; a 10-bit step is 1/1023), from
/// each pixel's largest |a − b| over R, G and B.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AbDiff {
    /// Mean of the per-pixel largest |a − b|.
    pub mean_abs: f64,
    /// Percentage of pixels whose largest |a − b| is above the threshold.
    pub pct_above: f64,
}

/// Why an operation here refused or failed.
#[derive(Debug)]
pub enum HdrError {
    /// A zero dimension, or a buffer that isn't width × height × 4 samples.
    Shape {
        width: u32,
        height: u32,
        samples: usize,
    },
    /// The operation can't take this encoding, e.g. linear light asked of PQ code values.
    Encoding {
        op: &'static str,
        found: Encoding,
        needs: &'static str,
    },
    /// The region is empty or leaves the frame.
    Region {
        region: Region,
        width: u32,
        height: u32,
    },
    /// The frames of a comparison differ in size or encoding, so they didn't come down the same capture path.
    Mismatch {
        a: (u32, u32, Encoding),
        b: (u32, u32, Encoding),
    },
    Io(std::io::Error),
    Png(png::EncodingError),
    Exr(exr::error::Error),
}

impl fmt::Display for HdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HdrError::Shape {
                width,
                height,
                samples,
            } => write!(
                f,
                "a {width}x{height} RGBA frame needs {width}x{height}x4 samples, not {samples}"
            ),
            HdrError::Encoding { op, found, needs } => {
                write!(
                    f,
                    "{op} refused: it needs {needs}, and this frame is {found}"
                )
            }
            HdrError::Region {
                region: r,
                width,
                height,
            } => write!(
                f,
                "region {}x{} at ({}, {}) is empty or leaves the {width}x{height} frame",
                r.width, r.height, r.x, r.y
            ),
            HdrError::Mismatch { a, b } => write!(
                f,
                "comparison refused: {}x{} {} vs {}x{} {}",
                a.0, a.1, a.2, b.0, b.1, b.2
            ),
            HdrError::Io(e) => write!(f, "write failed: {e}"),
            HdrError::Png(e) => write!(f, "PNG write failed: {e}"),
            HdrError::Exr(e) => write!(f, "EXR write failed: {e}"),
        }
    }
}

impl std::error::Error for HdrError {}

impl From<std::io::Error> for HdrError {
    fn from(e: std::io::Error) -> Self {
        HdrError::Io(e)
    }
}

impl From<png::EncodingError> for HdrError {
    fn from(e: png::EncodingError) -> Self {
        HdrError::Png(e)
    }
}

impl From<exr::error::Error> for HdrError {
    fn from(e: exr::error::Error) -> Self {
        HdrError::Exr(e)
    }
}

/// One captured frame and its encoding.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    width: u32,
    height: u32,
    pixels: Pixels,
}

impl Frame {
    /// Refuses a zero dimension or a buffer that isn't width × height × 4 samples.
    pub fn new(width: u32, height: u32, pixels: Pixels) -> Result<Frame, HdrError> {
        let samples = match &pixels {
            Pixels::PqBt2020U16(s) => s.len(),
            Pixels::PqCodesF16(s) | Pixels::ScrgbF16(s) => s.len(),
        };
        let wanted = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4));
        if width == 0 || height == 0 || wanted != Some(samples) {
            return Err(HdrError::Shape {
                width,
                height,
                samples,
            });
        }
        Ok(Frame {
            width,
            height,
            pixels,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &Pixels {
        &self.pixels
    }

    fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    pub fn encoding(&self) -> Encoding {
        match self.pixels {
            Pixels::PqBt2020U16(_) => Encoding::PqBt2020U16,
            Pixels::PqCodesF16(_) => Encoding::PqCodesF16,
            Pixels::ScrgbF16(_) => Encoding::ScrgbF16,
        }
    }

    /// The scRGB samples, or the refusal for `op`: linear-light operations don't take PQ code values.
    fn linear(&self, op: &'static str) -> Result<&[f16], HdrError> {
        match &self.pixels {
            Pixels::ScrgbF16(s) => Ok(s),
            _ => Err(HdrError::Encoding {
                op,
                found: self.encoding(),
                needs: "linear light (decode PQ code values first with Frame::decode_pq)",
            }),
        }
    }

    /// Absolute nits per pixel, RGB in BT.709 primaries. Refuses PQ code values: decode them first.
    pub fn linear_nits(&self) -> Result<impl Iterator<Item = [f32; 3]> + '_, HdrError> {
        Ok(self.linear("linear nits")?.chunks_exact(4).map(nits))
    }

    /// PQ code values per pixel, RGB in BT.2020 primaries, clamped to 0..=1: linear light is encoded (negatives
    /// clamp to 0), PQ code values pass through.
    pub fn pq_codes(&self) -> impl Iterator<Item = [f32; 3]> + '_ {
        (0..self.pixel_count()).map(move |i| {
            let p = i * 4;
            match &self.pixels {
                Pixels::PqBt2020U16(s) => {
                    [s[p], s[p + 1], s[p + 2]].map(|v| f32::from(v) / 65535.0)
                }
                Pixels::PqCodesF16(s) => [s[p], s[p + 1], s[p + 2]].map(|v| unit(v.to_f32())),
                Pixels::ScrgbF16(s) => bt709_to_bt2020(nits(&s[p..])).map(nits_to_pq),
            }
        })
    }

    /// The scRGB frame that PQ BT.2020 code values encode, alpha kept. Refuses a frame that is already linear light.
    /// Decoding an encoded frame gives back the original, less whatever lay outside BT.2020 (clamped to 0).
    pub fn decode_pq(&self) -> Result<Frame, HdrError> {
        if let Pixels::ScrgbF16(_) = self.pixels {
            return Err(HdrError::Encoding {
                op: "PQ decode",
                found: self.encoding(),
                needs: "PQ code values",
            });
        }
        let mut out = Vec::with_capacity(self.pixel_count() * 4);
        for (i, code) in self.pq_codes().enumerate() {
            let rgb = mat3(&BT2020_TO_BT709, code.map(pq_to_nits));
            out.extend(rgb.map(|n| f16::from_f32(n / SCRGB_WHITE_NITS)));
            out.push(match &self.pixels {
                Pixels::PqBt2020U16(s) => f16::from_f32(f32::from(s[i * 4 + 3]) / 65535.0),
                Pixels::PqCodesF16(s) | Pixels::ScrgbF16(s) => s[i * 4 + 3],
            });
        }
        Frame::new(self.width, self.height, Pixels::ScrgbF16(out))
    }

    /// Mean luminance (BT.709 Y) of the region in absolute nits. Refuses PQ code values and a region that is empty or
    /// leaves the frame.
    pub fn region_mean_nits(&self, region: Region) -> Result<f64, HdrError> {
        let s = self.linear("region mean")?;
        let inside = |at: u32, len: u32, max: u32| {
            len > 0 && at.checked_add(len).is_some_and(|end| end <= max)
        };
        if !inside(region.x, region.width, self.width)
            || !inside(region.y, region.height, self.height)
        {
            return Err(HdrError::Region {
                region,
                width: self.width,
                height: self.height,
            });
        }
        let mut sum = 0.0;
        for y in region.y..region.y + region.height {
            let start = (y as usize * self.width as usize + region.x as usize) * 4;
            for p in s[start..start + region.width as usize * 4].chunks_exact(4) {
                sum += nits(p)
                    .iter()
                    .zip(BT709_LUMA)
                    .map(|(&n, w)| f64::from(n) * w)
                    .sum::<f64>();
            }
        }
        Ok(sum / (f64::from(region.width) * f64::from(region.height)))
    }

    /// The A~A / A/B diff against `other` in PQ code units; `threshold` is in the same units. Refuses frames that
    /// differ in size or encoding: within a comparison the capture path stays the same.
    pub fn ab_diff(&self, other: &Frame, threshold: f32) -> Result<AbDiff, HdrError> {
        let (a, b) = (
            (self.width, self.height, self.encoding()),
            (other.width, other.height, other.encoding()),
        );
        if a != b {
            return Err(HdrError::Mismatch { a, b });
        }
        let (mut sum, mut above) = (0.0, 0u64);
        for (pa, pb) in self.pq_codes().zip(other.pq_codes()) {
            let d = (0..3).map(|c| (pa[c] - pb[c]).abs()).fold(0.0, f32::max);
            sum += f64::from(d);
            above += u64::from(d > threshold);
        }
        let n = f64::from(self.width) * f64::from(self.height);
        Ok(AbDiff {
            mean_abs: sum / n,
            pct_above: 100.0 * above as f64 / n,
        })
    }

    /// A 16-bit RGB PNG of PQ BT.2020 code values with a cICP chunk before IDAT. Alpha is dropped.
    pub fn write_png_pq(&self, w: impl Write) -> Result<(), HdrError> {
        let mut data = Vec::with_capacity(self.pixel_count() * 6);
        for code in self.pq_codes() {
            for c in code {
                data.extend_from_slice(&((c * 65535.0).round() as u16).to_be_bytes());
            }
        }
        let mut encoder = png::Encoder::new(w, self.width, self.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Sixteen);
        // Live boot 1: the default deflate took most of 5.65 s on a real 4K frame.
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header()?;
        // png 0.18 reads cICP but never writes it, so the raw chunk goes in here: after IHDR, before IDAT.
        writer.write_chunk(png::chunk::cICP, &CICP_PQ_BT2020)?;
        writer.write_image_data(&data)?;
        writer.finish()?;
        Ok(())
    }

    /// A half RGBA EXR of linear scRGB, uncompressed scanlines in increasing order (one line per block), the layout the
    /// devkit writes and the harness's EXR reader parses: it reads raw lines and silently misreads tiles or compression.
    /// Refuses PQ code values: an EXR of those is the devkit's trap. Pass a buffered writer.
    pub fn write_exr(&self, w: impl Write + Seek) -> Result<(), HdrError> {
        let s = self.linear("EXR")?;
        let width = self.width as usize;
        let channels = SpecificChannels::rgba(|Vec2(x, y)| {
            let p = (y * width + x) * 4;
            (s[p], s[p + 1], s[p + 2], s[p + 3])
        });
        let encoding = exr::prelude::Encoding::UNCOMPRESSED;
        Image::from_encoded_channels((width, self.height as usize), encoding, channels)
            .write()
            .to_buffered(w)?;
        Ok(())
    }

    /// The half floats as they are, in a `.npy` (version 1.0, little-endian float16, shape [height, width, 4]).
    /// Refuses 16-bit integer codes.
    pub fn write_npy(&self, mut w: impl Write) -> Result<(), HdrError> {
        let s = match &self.pixels {
            Pixels::PqCodesF16(s) | Pixels::ScrgbF16(s) => s,
            Pixels::PqBt2020U16(_) => {
                return Err(HdrError::Encoding {
                    op: "npy",
                    found: self.encoding(),
                    needs: "half floats",
                });
            }
        };
        let dict = format!(
            "{{'descr': '<f2', 'fortran_order': False, 'shape': ({}, {}, 4), }}",
            self.height, self.width
        );
        // Magic, version and header length take 10 bytes; spaces pad the header so the data starts at a multiple of 64.
        let header = format!("{dict}{}\n", " ".repeat(63 - (10 + dict.len()) % 64));
        w.write_all(b"\x93NUMPY\x01\x00")?;
        w.write_all(&(header.len() as u16).to_le_bytes())?;
        w.write_all(header.as_bytes())?;
        let mut row = Vec::with_capacity(self.width as usize * 8);
        for line in s.chunks_exact(self.width as usize * 4) {
            row.clear();
            row.extend(line.iter().flat_map(|v| v.to_le_bytes()));
            w.write_all(&row)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exr::prelude::{ReadChannels, ReadLayers};
    use std::io::Cursor;

    fn scrgb(width: u32, height: u32, px: &[[f32; 4]]) -> Frame {
        let s = px.iter().flatten().map(|&v| f16::from_f32(v)).collect();
        Frame::new(width, height, Pixels::ScrgbF16(s)).unwrap()
    }

    fn gray(nits: f32) -> [f32; 4] {
        let v = nits / SCRGB_WHITE_NITS;
        [v, v, v, 1.0]
    }

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn pq_known_values() {
        assert!(nits_to_pq(0.0).abs() < 1e-6);
        assert_eq!((nits_to_pq(0.0) * 65535.0).round(), 0.0);
        assert!(close(nits_to_pq(10_000.0), 1.0, 1e-6));
        assert!(close(nits_to_pq(100.0), 0.5081, 1e-4));
        assert!(close(nits_to_pq(203.0), 0.5807, 1e-4));
        assert!(close(nits_to_pq(1000.0), 0.7518, 1e-4));
        assert_eq!(pq_to_nits(0.0), 0.0);
        assert!(close(pq_to_nits(1.0), 10_000.0, 1e-2));
    }

    #[test]
    fn pq_round_trips() {
        for n in [0.01, 0.1, 1.0, 80.0, 100.0, 203.0, 1000.0, 4000.0, 10_000.0] {
            let back = pq_to_nits(nits_to_pq(n));
            assert!(close(back, n, n * 1e-4), "{n} nits came back as {back}");
        }
        for i in 0..=100 {
            let code = i as f32 / 100.0;
            assert!(
                close(nits_to_pq(pq_to_nits(code)), code, 1e-5),
                "code {code}"
            );
        }
    }

    #[test]
    fn pq_clamps_to_its_range() {
        assert_eq!(nits_to_pq(-5.0), nits_to_pq(0.0));
        assert_eq!(nits_to_pq(f32::NAN), nits_to_pq(0.0));
        assert_eq!(nits_to_pq(20_000.0), nits_to_pq(10_000.0));
        assert_eq!(pq_to_nits(-0.5), 0.0);
        assert_eq!(pq_to_nits(1.5), pq_to_nits(1.0));
    }

    #[test]
    fn scrgb_one_is_80_nits() {
        assert_eq!(scrgb_to_nits(1.0), 80.0);
        assert_eq!(scrgb_to_nits(-0.5), -40.0);
    }

    #[test]
    fn bt709_to_bt2020_keeps_white_and_inverts() {
        for (out, want) in bt709_to_bt2020([1.0; 3]).into_iter().zip([1.0; 3]) {
            assert!(close(out, want, 1e-6));
        }
        let red = bt709_to_bt2020([1.0, 0.0, 0.0]);
        assert!(
            close(red[0], 0.6274, 1e-4)
                && close(red[1], 0.0691, 1e-4)
                && close(red[2], 0.0164, 1e-4)
        );
        let product = BT709_TO_BT2020.map(|row| {
            [0, 1, 2].map(|j| (0..3).map(|k| row[k] * BT2020_TO_BT709[k][j]).sum::<f64>())
        });
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        for (got, want) in product.iter().flatten().zip(identity.iter().flatten()) {
            assert!((got - want).abs() < 1e-12, "M·M⁻¹ = {product:?}");
        }
    }

    #[test]
    fn frame_refuses_bad_shapes() {
        for (w, h, len) in [(0, 1, 0), (1, 0, 0), (2, 2, 15), (2, 2, 17)] {
            let err = Frame::new(w, h, Pixels::ScrgbF16(vec![f16::ZERO; len])).unwrap_err();
            assert!(matches!(err, HdrError::Shape { .. }), "{w}x{h} with {len}");
        }
    }

    #[test]
    fn encoding_names_match_the_spec() {
        for (e, name) in [
            (Encoding::Srgb8, "srgb8"),
            (Encoding::PqBt2020U16, "pq_bt2020_u16"),
            (Encoding::PqCodesF16, "pq_codes_f16"),
            (Encoding::ScrgbF16, "scrgb_f16"),
        ] {
            assert_eq!(e.to_string(), name);
            assert_eq!(serde_json::to_string(&e).unwrap(), format!("\"{name}\""));
            assert_eq!(
                serde_json::from_str::<Encoding>(&format!("\"{name}\"")).unwrap(),
                e
            );
        }
    }

    #[test]
    fn linear_light_refuses_pq_codes_until_decoded() {
        let codes = [nits_to_pq(100.0), nits_to_pq(100.0), nits_to_pq(100.0), 1.0];
        let f16_codes =
            Frame::new(1, 1, Pixels::PqCodesF16(codes.map(f16::from_f32).to_vec())).unwrap();
        let u16_codes = Frame::new(
            1,
            1,
            Pixels::PqBt2020U16(codes.map(|c| (c * 65535.0).round() as u16).to_vec()),
        )
        .unwrap();
        let all = Region {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        for frame in [&f16_codes, &u16_codes] {
            let refused = |r: Result<(), HdrError>| matches!(r, Err(HdrError::Encoding { found, .. }) if found == frame.encoding());
            assert!(refused(frame.linear_nits().map(|_| ())));
            assert!(refused(frame.region_mean_nits(all).map(|_| ())));
            let mut out = Cursor::new(Vec::new());
            assert!(refused(frame.write_exr(&mut out)));
            assert!(out.get_ref().is_empty(), "a refusal wrote nothing");

            let decoded = frame.decode_pq().unwrap();
            assert_eq!(decoded.encoding(), Encoding::ScrgbF16);
            let [r, g, b] = decoded.linear_nits().unwrap().next().unwrap();
            // Half-float PQ codes are coarse: a step near 100 nits is about 0.2 %.
            for n in [r, g, b] {
                assert!(close(n, 100.0, 0.5), "decoded to {n} nits");
            }
            assert!(close(
                decoded.region_mean_nits(all).unwrap() as f32,
                100.0,
                0.5
            ));
        }
        assert!(matches!(
            scrgb(1, 1, &[gray(100.0)]).decode_pq(),
            Err(HdrError::Encoding {
                found: Encoding::ScrgbF16,
                ..
            })
        ));
    }

    #[test]
    fn decode_pq_inverts_the_encoder() {
        let px = [
            gray(0.0),
            gray(100.0),
            [2.0, 0.5, 0.1, 1.0],
            [12.5, 0.0, 0.0, 0.5],
        ];
        let frame = scrgb(2, 2, &px);
        let u16_codes: Vec<u16> = frame
            .pq_codes()
            .zip(px)
            .flat_map(|(c, p)| {
                let [r, g, b] = c.map(|v| (v * 65535.0).round() as u16);
                [r, g, b, (p[3] * 65535.0) as u16]
            })
            .collect();
        let decoded = Frame::new(2, 2, Pixels::PqBt2020U16(u16_codes))
            .unwrap()
            .decode_pq()
            .unwrap();
        for (got, want) in decoded
            .linear_nits()
            .unwrap()
            .zip(frame.linear_nits().unwrap())
        {
            for c in 0..3 {
                assert!(
                    close(got[c], want[c], want[c] * 2e-3 + 0.05),
                    "{got:?} vs {want:?}"
                );
            }
        }
        let Pixels::ScrgbF16(s) = decoded.pixels() else {
            unreachable!()
        };
        assert_eq!(s[15].to_f32(), 0.5, "alpha kept");
    }

    #[test]
    fn region_mean_nits_is_mean_luminance() {
        // Left column 100-nit gray, right column 400-nit gray, bottom-right pure red at 80 nits a channel.
        let frame = scrgb(
            2,
            3,
            &[
                gray(100.0),
                gray(400.0),
                gray(100.0),
                gray(400.0),
                gray(100.0),
                [1.0, 0.0, 0.0, 1.0],
            ],
        );
        let mean = |x, y, width, height| {
            frame.region_mean_nits(Region {
                x,
                y,
                width,
                height,
            })
        };
        assert!((mean(0, 0, 1, 3).unwrap() - 100.0).abs() < 1e-3);
        assert!((mean(1, 0, 1, 2).unwrap() - 400.0).abs() < 1e-3);
        assert!((mean(1, 2, 1, 1).unwrap() - 0.2126 * 80.0).abs() < 1e-3);
        assert!((mean(0, 0, 2, 2).unwrap() - 250.0).abs() < 1e-3);
        for (x, y, w, h) in [
            (0, 0, 0, 1),
            (0, 0, 3, 1),
            (1, 2, 1, 2),
            (u32::MAX, 0, 2, 1),
        ] {
            assert!(
                matches!(mean(x, y, w, h), Err(HdrError::Region { .. })),
                "{x},{y} {w}x{h}"
            );
        }
    }

    #[test]
    fn ab_diff_in_pq_code_units() {
        let a = scrgb(2, 2, &[gray(100.0); 4]);
        assert_eq!(
            a.ab_diff(&a.clone(), 0.0).unwrap(),
            AbDiff {
                mean_abs: 0.0,
                pct_above: 0.0
            }
        );

        // One pixel goes from 100 to 1000 nits in green only: the largest channel diff is PQ(1000) − PQ(100) in
        // BT.2020 green, a quarter of it on average, 25 % of the pixels above a small threshold.
        let mut px = [gray(100.0); 4];
        px[3][1] = 1000.0 / SCRGB_WHITE_NITS;
        let b = scrgb(2, 2, &px);
        let codes = |f: &Frame| f.pq_codes().nth(3).unwrap();
        let d = (0..3)
            .map(|c| (codes(&a)[c] - codes(&b)[c]).abs())
            .fold(0.0, f32::max);
        assert!(d > 0.2, "a big step, {d}");
        let diff = a.ab_diff(&b, 0.01).unwrap();
        assert!((diff.mean_abs - f64::from(d) / 4.0).abs() < 1e-7);
        assert_eq!(diff.pct_above, 25.0);
        assert_eq!(a.ab_diff(&b, d).unwrap().pct_above, 0.0, "strictly above");

        let small = scrgb(1, 1, &[gray(100.0)]);
        let codes = Frame::new(2, 2, Pixels::PqCodesF16(vec![f16::ZERO; 16])).unwrap();
        for other in [&small, &codes] {
            assert!(matches!(
                a.ab_diff(other, 0.01),
                Err(HdrError::Mismatch { .. })
            ));
        }
    }

    #[test]
    fn png_has_cicp_before_idat_and_pq_pixels() {
        let px = [
            gray(0.0),
            gray(100.0),
            gray(200.0),
            [-0.5, 1.0, 0.0, 1.0],
            gray(20_000.0),
            [1.0, 0.0, 0.0, 1.0],
        ];
        let frame = scrgb(3, 2, &px);
        let mut bytes = Vec::new();
        frame.write_png_pq(&mut bytes).unwrap();

        let at = |tag: &[u8]| bytes.windows(4).position(|w| w == tag).unwrap();
        assert!(at(b"IHDR") < at(b"cICP") && at(b"cICP") < at(b"IDAT"));
        assert_eq!(
            bytes[at(b"cICP") - 4..at(b"cICP")],
            4u32.to_be_bytes(),
            "cICP holds 4 bytes"
        );
        assert_eq!(bytes[at(b"cICP") + 4..at(b"cICP") + 8], CICP_PQ_BT2020);

        let mut reader = png::Decoder::new(Cursor::new(&bytes)).read_info().unwrap();
        let cicp = reader.info().coding_independent_code_points.unwrap();
        assert_eq!(
            [
                cicp.color_primaries,
                cicp.transfer_function,
                cicp.matrix_coefficients
            ],
            [9, 16, 0]
        );
        assert!(cicp.is_video_full_range_image);
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (3, 2));
        assert_eq!(
            (info.color_type, info.bit_depth),
            (png::ColorType::Rgb, png::BitDepth::Sixteen)
        );
        let got: Vec<u16> = buf
            .chunks_exact(2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(got.len(), 18);

        let code = |nits: f32| (nits_to_pq(nits) * 65535.0).round() as u16;
        for (i, p) in px.iter().enumerate() {
            let want = bt709_to_bt2020([p[0], p[1], p[2]].map(scrgb_to_nits)).map(code);
            assert_eq!(got[i * 3..i * 3 + 3], want, "pixel {i}");
        }
        assert_eq!(got[0..3], [0; 3], "black");
        assert!(
            got[3].abs_diff((0.5081f32 * 65535.0) as u16) <= 7,
            "100 nits is PQ 0.5081"
        );
        assert_eq!(got[12..15], [65535; 3], "above 10,000 nits clips to 1.0");
        // scRGB (-0.5, 1, 0) is outside BT.709 but inside BT.2020: red ends up positive.
        assert!(got[9] > 0);
    }

    #[test]
    fn png_passes_u16_codes_through() {
        let codes: Vec<u16> = (0..4u16)
            .flat_map(|i| [i * 1000 + 1, 65535 - i, 30_001 + i, 65535])
            .collect();
        let frame = Frame::new(2, 2, Pixels::PqBt2020U16(codes.clone())).unwrap();
        let mut bytes = Vec::new();
        frame.write_png_pq(&mut bytes).unwrap();
        let mut reader = png::Decoder::new(Cursor::new(&bytes)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut buf).unwrap();
        let got: Vec<u16> = buf
            .chunks_exact(2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .collect();
        let want: Vec<u16> = codes
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn exr_reads_back_as_half_rgba_scrgb() {
        let px = [
            gray(100.0),
            [-0.25, 12.5, 0.0, 1.0],
            [0.0, 0.0, 0.0, 0.0],
            [1.0, 2.0, 3.0, 0.5],
            gray(0.5),
            gray(4000.0),
        ];
        let frame = scrgb(3, 2, &px);
        let mut out = Cursor::new(Vec::new());
        frame.write_exr(&mut out).unwrap();

        type Rgba = (f16, f16, f16, f16);
        let image = exr::prelude::read()
            .no_deep_data()
            .largest_resolution_level()
            .rgba_channels(
                |size, _| {
                    (
                        size,
                        vec![(f16::ZERO, f16::ZERO, f16::ZERO, f16::ZERO); size.area()],
                    )
                },
                |(size, px): &mut (Vec2<usize>, Vec<Rgba>), at: Vec2<usize>, p: Rgba| {
                    px[at.y() * size.width() + at.x()] = p
                },
            )
            .first_valid_layer()
            .all_attributes()
            .from_buffered(Cursor::new(out.into_inner()))
            .unwrap();
        let channels = &image.layer_data.channel_data;
        let (r, _, _, a) = &channels.channels;
        assert_eq!(r.sample_type, exr::prelude::SampleType::F16);
        assert_eq!(
            a.as_ref().unwrap().sample_type,
            exr::prelude::SampleType::F16
        );
        let (size, got) = &channels.pixels;
        assert_eq!((size.width(), size.height()), (3, 2));
        let Pixels::ScrgbF16(s) = frame.pixels() else {
            unreachable!()
        };
        let want: Vec<Rgba> = s
            .chunks_exact(4)
            .map(|p| (p[0], p[1], p[2], p[3]))
            .collect();
        assert_eq!(*got, want);
    }

    /// The harness's EXR reader takes the header's attributes, then one offset per line, and reads each line's halves
    /// raw at offset + 8, channel by channel (A, B, G, R): tiles or compression would be misread without an error. So
    /// the header must say uncompressed scanlines, and every line must read back as written, the way it reads them.
    #[test]
    fn exr_is_uncompressed_scanlines_as_the_harness_reads_them() {
        let px = [gray(100.0), [1.0, 2.0, 3.0, 0.5], [-0.25, 12.5, 0.0, 1.0]];
        let px: Vec<[f32; 4]> = px.iter().cycle().take(5 * 3).copied().collect();
        let frame = scrgb(5, 3, &px);
        let mut out = Cursor::new(Vec::new());
        frame.write_exr(&mut out).unwrap();
        let d = out.into_inner();

        let meta = exr::meta::MetaData::read_from_buffered(Cursor::new(&d), true).unwrap();
        let header = &meta.headers[0];
        assert_eq!(header.blocks, exr::meta::BlockDescription::ScanLines);
        assert_eq!(
            header.compression,
            exr::compression::Compression::Uncompressed
        );
        assert_eq!(
            header.line_order,
            exr::meta::attribute::LineOrder::Increasing
        );

        // read_exr_row.py, step by step: name\0 type\0 size data ... \0, then the line offsets.
        let le = |at: usize, n: usize| {
            d[at..at + n]
                .iter()
                .rev()
                .fold(0u64, |v, &b| v << 8 | u64::from(b))
        };
        let mut i = 8;
        while d[i] != 0 {
            i += d[i..].iter().position(|&b| b == 0).unwrap() + 1;
            i += d[i..].iter().position(|&b| b == 0).unwrap() + 1;
            i += 4 + le(i, 4) as usize;
        }
        i += 1;
        let Pixels::ScrgbF16(s) = frame.pixels() else {
            unreachable!()
        };
        for y in 0..3 {
            let line = le(i + y * 8, 8) as usize + 8;
            for (k, channel) in [3, 2, 1, 0].into_iter().enumerate() {
                for x in 0..5 {
                    let at = line + (k * 5 + x) * 2;
                    let got = f16::from_bits(le(at, 2) as u16);
                    assert_eq!(
                        got,
                        s[(y * 5 + x) * 4 + channel],
                        "line {y}, channel {channel}, x {x}"
                    );
                }
            }
        }
    }

    #[test]
    fn npy_header_and_data() {
        let px = [
            gray(80.0),
            [-1.0, 0.5, 2.0, 1.0],
            gray(0.0),
            gray(160.0),
            gray(240.0),
            [0.25, 0.0, 0.0, 0.0],
        ];
        let frame = scrgb(3, 2, &px);
        let mut bytes = Vec::new();
        frame.write_npy(&mut bytes).unwrap();

        let dict = b"{'descr': '<f2', 'fortran_order': False, 'shape': (2, 3, 4), }";
        assert_eq!(&bytes[..8], b"\x93NUMPY\x01\x00");
        assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 118);
        assert_eq!(&bytes[10..10 + dict.len()], dict);
        assert!(bytes[10 + dict.len()..127].iter().all(|&b| b == b' '));
        assert_eq!(bytes[127], b'\n');
        assert_eq!(bytes.len(), 128 + 3 * 2 * 4 * 2);
        assert_eq!(bytes[128..130], f16::ONE.to_le_bytes());
        let data: Vec<f32> = bytes[128..]
            .chunks_exact(2)
            .map(|b| f16::from_le_bytes([b[0], b[1]]).to_f32())
            .collect();
        assert_eq!(data, px.iter().flatten().copied().collect::<Vec<_>>());

        let codes = Frame::new(1, 1, Pixels::PqBt2020U16(vec![0; 4])).unwrap();
        let mut out = Vec::new();
        assert!(matches!(
            codes.write_npy(&mut out),
            Err(HdrError::Encoding {
                found: Encoding::PqBt2020U16,
                ..
            })
        ));
        assert!(out.is_empty());
    }
}
