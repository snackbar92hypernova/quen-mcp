//! `measure_image` (the runner design's phase 3, finding 39): numbers from a screenshot quen took, over a crop, as
//! measurement rows with its `capture_id`, family `screenshot`. quen-core's image module decodes and measures; here the
//! captures rows are found, each file checked against its sha256, and the rows written in one transaction. It reads
//! the files only, and runs on the session thread, so never inside another call's window.

use std::fs;
use std::time::Duration;

use quen_core::hdr::Encoding;
use quen_core::image::{Image, Row};
use quen_core::journal::sha256_hex;
use quen_core::ledger::{Ledger, Measurement, Tag};
use serde_json::{Map, Value, json};

use crate::config::Config;
use crate::game::Fields;
use crate::session::{self, Call, Done, Session};

const METRICS: [&str; 4] = ["channels", "luminance", "hue_share", "diff"];
const KEYS: [&str; 14] = [
    "token",
    "capture",
    "metrics",
    "crop",
    "crop_unit",
    "hue",
    "s_min",
    "v_min",
    "reference",
    "noise_ref",
    "name",
    "state_label",
    "run_id",
    "note",
];

pub fn tool() -> Value {
    let capture = |what: &str| {
        json!({"type": "string", "description": format!("{what}: its captures id, or its path as captures.path has \
            it (<session>/<tag>.png)")})
    };
    let unit = json!({"type": "number", "minimum": 0, "maximum": 1, "description": "a fraction"});
    json!({
        "name": "measure_image",
        "description": "Numbers from a screenshot quen took (game_screenshot, game_frame, sweep_play) over a crop, \
            written as measurement rows: method_family screenshot, tag M, its capture_id and boot (quen_guide: \
            measure_image). metrics: channels (per R, G, B the mean and median of the file's values, srgb8 0-255, pq \
            0-1 or scrgb, and the mean of linear light), luminance (BT.709 Y of linear light: mean, median, p95, max; \
            nits from the HDR files, relative from srgb8: sRGB white = 1), hue_share (the share of pixels in an HSV hue \
            range), diff (against reference, and noise_ref as the floor). Reads the files only, each checked against \
            its sha256, on the session thread: never during a window. Refused, nothing written: not quen's screenshot \
            or not an image (an .rdc, a CSV, Nsight's final.png: its encoding is unknown), the file removed or changed, an argument a metric doesn't take, a crop \
            off the frame, a reference of another method, encoding, size or display state.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "token": {"type": "string", "description": "optional: the live session's token (run_plan passes it)"},
                "capture": capture("the screenshot"),
                "metrics": {"type": "array", "minItems": 1, "items": {"type": "string", "enum": METRICS, "description": "a metric"}, "description": "which numbers"},
                "crop": {"type": "array", "minItems": 4, "maxItems": 4, "items": {"type": "number", "description": "x, y, width or height"}, "description": "[x, y, width, height] from the top-left; default the whole frame"},
                "crop_unit": {"type": "string", "enum": ["px", "fraction"], "description": "px (default): whole pixels. fraction: of the frame's width and height, each edge rounded to a pixel"},
                "hue": {"type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "integer", "minimum": 0, "maximum": 360, "description": "degrees"}, "description": "hue_share: [from, to] degrees, both included; from above to wraps through 0"},
                "s_min": unit.clone(),
                "v_min": unit,
                "reference": capture("diff: the capture to compare with"),
                "noise_ref": capture("diff: a second capture of the screenshot's own state, the noise floor"),
                "name": {"type": "string", "pattern": "^[a-z][a-z0-9_]{0,39}$", "description": "prefixes each metric's name (geralt: geralt_r_lin_mean)"},
                "state_label": {"type": "string", "description": "the rows' A/B label"},
                "run_id": {"type": "string", "description": "the rows' run id"},
                "note": {"type": "string", "description": "added to the rows' note"},
            },
            "required": ["capture", "metrics"],
            "additionalProperties": false,
        },
    })
}

/// A screenshot's captures row, its file read and decoded.
struct Shot {
    id: i64,
    path: String,
    method: String,
    boot: Option<i64>,
    display: Value,
    image: Image,
}

/// The screenshot `v` (an id, as a number or text, or a path) names, for argument `key`: one of quen's own
/// (capture.rs's methods), an image encoding, on disk and unchanged since its row.
fn shot(ledger: &Ledger, config: &Config, key: &str, v: &Value) -> Result<Shot, String> {
    let by_id = v
        .as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()));
    let (column, param) = match (by_id, v.as_str()) {
        (Some(id), _) => ("id = ?1", json!(id)),
        (None, Some(path)) if !path.is_empty() => {
            ("path = ?1 COLLATE NOCASE", json!(path.replace('\\', "/")))
        }
        _ => return Err(format!("{key} is a captures id or path")),
    };
    let sql = format!(
        "SELECT id, path, sha256, method, encoding, boot_id, removed_at, display_state FROM captures WHERE {column}"
    );
    let found = ledger.query(&sql, &[param], Duration::from_secs(5));
    let rows = found.map_err(session::ledger_error)?.rows;
    let [row] = &rows[..] else {
        return Err(format!("{key}: no capture {v}"));
    };
    let id = row[0].as_i64().unwrap_or_default();
    let text = |i: usize| row[i].as_str().unwrap_or_default().to_string();
    let (path, method) = (text(1), text(3));
    if method == "ngfx_extract" && path.ends_with("/final.png") {
        return Err(format!(
            "{key}: capture {id} ({path}) is Nsight's final image (ngfx_extract's screenshot op): Nsight makes it from \
             the presented back buffer by a conversion it doesn't name (boot 259's: an 8-bit RGBA PNG with no \
             colour-space chunk, from a 10-bit back buffer on an HDR display), so its encoding is unknown and \
             measure_image doesn't read it"
        ));
    }
    if !crate::capture::IMAGE_METHODS.contains(&method.as_str()) {
        return Err(format!(
            "{key}: capture {id} ({path}) was taken by {method:?}, not by quen's screenshot (methods {}): its numbers \
             aren't family screenshot",
            crate::capture::IMAGE_METHODS.join(", ")
        ));
    }
    let encoding: Encoding = serde_json::from_value(row[4].clone()).map_err(|_| {
        format!(
            "{key}: capture {id} ({path}) has encoding {}: measure_image reads srgb8, pq_bt2020_u16 and scrgb_f16",
            row[4]
        )
    })?;
    if !row[6].is_null() {
        return Err(format!(
            "{key}: capture {id} ({path}) was removed at {}",
            row[6]
        ));
    }
    let file = config.capture_root().join(&path);
    let bytes = fs::read(&file).map_err(|e| format!("{key}: {}: {e}", file.display()))?;
    if sha256_hex(&bytes) != text(2) {
        return Err(format!(
            "{key}: {}: changed since quen wrote it (sha256)",
            file.display()
        ));
    }
    let image = Image::decode(&bytes, encoding)
        .map_err(|why| format!("{key}: capture {id} ({path}), {encoding}: {why}"))?;
    Ok(Shot {
        id,
        path,
        method,
        boot: row[5].as_i64(),
        display: row[7].clone(),
        image,
    })
}

pub(crate) fn call(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let map = args
        .as_object()
        .ok_or("arguments must be an object".to_string())?;
    if let Some(key) = map.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("unknown argument {key:?}").into());
    }
    if let Some(token) = args.get("token") {
        let live = session
            .live
            .as_ref()
            .ok_or("a token, but this quen has no live session".to_string())?;
        if token.as_str() != Some(live.token.as_str()) {
            return Err(
                "wrong token: only the agent that launched the session passes one"
                    .to_string()
                    .into(),
            );
        }
    }
    let metrics: Vec<&str> = args["metrics"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| m.as_str().filter(|m| METRICS.contains(m)))
        .collect::<Option<_>>()
        .filter(|m: &Vec<&str>| !m.is_empty())
        .ok_or(format!("metrics is a list of {}", METRICS.join(", ")))?;
    if (1..metrics.len()).any(|i| metrics[..i].contains(&metrics[i])) {
        return Err("metrics names a metric twice".to_string().into());
    }
    let wants = |m: &str| metrics.contains(&m);
    for (key, metric) in [
        ("hue", "hue_share"),
        ("s_min", "hue_share"),
        ("v_min", "hue_share"),
        ("reference", "diff"),
        ("noise_ref", "diff"),
    ] {
        if map.contains_key(key) && !wants(metric) {
            return Err(format!("{key} is for {metric}, which metrics doesn't list").into());
        }
    }
    let hue = match (wants("hue_share"), args["hue"].as_array()) {
        (false, _) => None,
        (true, Some(h)) => match h
            .iter()
            .map(|d| d.as_u64().filter(|d| *d <= 360))
            .collect::<Option<Vec<_>>>()
        {
            Some(d) if d.len() == 2 => Some([d[0] as u32, d[1] as u32]),
            _ => {
                return Err("hue is [from, to], whole degrees from 0 to 360"
                    .to_string()
                    .into());
            }
        },
        (true, None) => return Err("hue_share needs hue: [from, to] degrees".to_string().into()),
    };
    let fraction_of = |key: &str| match args.get(key) {
        None => Ok(0.0),
        Some(v) => v
            .as_f64()
            .filter(|f| (0.0..=1.0).contains(f))
            .ok_or(format!("{key} is a number from 0 to 1")),
    };
    let (s_min, v_min) = (fraction_of("s_min")?, fraction_of("v_min")?);
    if wants("diff") && !map.contains_key("reference") {
        return Err("diff needs reference: the capture to compare with"
            .to_string()
            .into());
    }
    let crop = match args.get("crop") {
        None => None,
        Some(v) => Some(
            v.as_array()
                .filter(|a| a.len() == 4)
                .and_then(|a| a.iter().map(Value::as_f64).collect::<Option<Vec<_>>>())
                .map(|c| [c[0], c[1], c[2], c[3]])
                .ok_or("crop is [x, y, width, height]".to_string())?,
        ),
    };
    let fraction = match (args.get("crop_unit").map(Value::as_str), crop) {
        (None | Some(Some("px")), _) => false,
        (Some(Some("fraction")), Some(_)) => true,
        (Some(Some("fraction")), None) => return Err("crop_unit without crop".to_string().into()),
        _ => return Err("crop_unit is px or fraction".to_string().into()),
    };
    let text = |key: &str| match args.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_str()
            .map(|s| Some(s.to_string()))
            .ok_or(format!("{key} must be a string")),
    };
    let (name, label, run, note) = (
        text("name")?,
        text("state_label")?,
        text("run_id")?,
        text("note")?,
    );
    if let Some(n) = &name {
        let ok = n.len() <= 40
            && n.starts_with(|c: char| c.is_ascii_lowercase())
            && n.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !ok {
            return Err(format!("name {n:?}: 1-40 of a-z, 0-9 and _, from a letter").into());
        }
    }
    let config = session.config()?;
    let mut ledger = session::open_ledger(config)?;
    let main = shot(&ledger, config, "capture", &args["capture"])?;
    call.lap("decode");
    let region = main.image.region(crop, fraction)?;
    let mut rows: Vec<Row> = Vec::new();
    let mut about = vec![format!(
        "measure_image {} {}x{} crop {},{} {}x{} px",
        main.image.encoding(),
        main.image.width(),
        main.image.height(),
        region.x,
        region.y,
        region.width,
        region.height
    )];
    let mut compared = Map::new();
    for metric in &metrics {
        match *metric {
            "channels" => rows.extend(main.image.channels(region)?),
            "luminance" => rows.extend(main.image.luminance(region)?),
            "hue_share" => {
                let [from, to] = hue.unwrap_or_default();
                rows.push(main.image.hue_share(region, [from, to], s_min, v_min)?);
                about.push(format!("hue {from}-{to} s>={s_min} v>={v_min}"));
            }
            _ => {
                let mut against = Vec::new();
                for key in ["reference", "noise_ref"] {
                    let Some(v) = args.get(key) else { continue };
                    let other = shot(&ledger, config, key, v)?;
                    if (&other.method, &other.display) != (&main.method, &main.display) {
                        return Err(format!(
                            "{key}: capture {} was taken by {} with display state {}, the screenshot by {} with {}: \
                             compare only captures of one method, encoding and display state",
                            other.id, other.method, other.display, main.method, main.display
                        )
                        .into());
                    }
                    about.push(format!("{key} capture {}", other.id));
                    compared.insert(key.into(), json!({"id": other.id, "path": other.path}));
                    against.push(other.image);
                }
                rows.extend(main.image.diff_rows(&against[0], against.get(1), region)?);
            }
        }
    }
    let non_finite = main.image.non_finite(region)?;
    if non_finite > 0 {
        about.push(format!(
            "{non_finite} pixels with a NaN or infinity left out"
        ));
    }
    about.extend(note);
    call.lap("metrics");
    let note = about.join("; ");
    if let Some(n) = &name {
        for r in &mut rows {
            r.metric = format!("{n}_{}", r.metric);
        }
    }
    let ids = ledger
        .atomic(|l| {
            rows.iter()
                .map(|r| {
                    l.add_measurement(&Measurement {
                        boot_id: main.boot,
                        capture_id: Some(main.id),
                        metric: r.metric.clone(),
                        value: Some(r.value),
                        unit: Some(r.unit.into()),
                        state_label: label.clone(),
                        method_family: "screenshot".into(),
                        tag: Tag::M,
                        run_id: run.clone(),
                        note: Some(note.clone()),
                        extraction_id: None,
                        world: None,
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(session::ledger_error)?;
    for id in ids {
        call.row("measurements", id);
    }
    call.lap("ledger");
    let mut fields = Map::new();
    fields.insert(
        "capture".into(),
        json!({"id": main.id, "path": main.path, "method": main.method, "encoding": main.image.encoding(),
               "size": [main.image.width(), main.image.height()]}),
    );
    fields.insert(
        "crop".into(),
        json!([region.x, region.y, region.width, region.height]),
    );
    fields.extend(compared);
    fields.insert("values".into(), json!(rows));
    if non_finite > 0 {
        fields.insert("non_finite".into(), json!(non_finite));
    }
    Ok(fields)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::game::tests::Fake;
    use half::f16;
    use quen_core::hdr::{Frame, Pixels};
    use quen_core::ledger::Capture;

    /// Writes `bytes` as `<capture root>/s1/<name>` with a captures row on boot 1 (the Fake's): its id.
    pub(crate) fn stored(
        fx: &Fake,
        name: &str,
        bytes: &[u8],
        method: &str,
        encoding: Option<Encoding>,
    ) -> i64 {
        let dir = fx.dir.join("data/captures/s1");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), bytes).unwrap();
        let mut ledger = Ledger::open(fx.dir.join("data/state/quen.db")).unwrap();
        let row = Capture {
            boot_id: Some(1),
            path: format!("s1/{name}"),
            sha256: sha256_hex(bytes),
            bytes: bytes.len() as i64,
            kind: name.rsplit('.').next().unwrap().into(),
            method: Some(method.into()),
            encoding,
            display_state: json!({"color_mode": "hdr"}).as_object().cloned(),
            t: "2026-10-17T13:16:00Z".into(),
            ..Capture::default()
        };
        ledger.add_capture(&row).unwrap()
    }

    /// An 8-bit RGB PNG of `px`, as capture.rs encodes the SDR grab.
    pub(crate) fn srgb8(width: u32, height: u32, px: &[[u8; 3]]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&px.concat()).unwrap();
        writer.finish().unwrap();
        out
    }

    fn db(fx: &Fake, sql: &str) -> Vec<Vec<Value>> {
        let l = Ledger::open_readonly(fx.dir.join("data/state/quen.db")).unwrap();
        l.query(sql, &[], Duration::from_secs(5)).unwrap().rows
    }

    /// The design's accept: hand-computed values from each encoding quen's screenshots use, written as rows with the
    /// capture's id and boot, family screenshot; ids and paths both name a capture.
    #[test]
    fn rows_carry_the_capture_id_and_hand_computed_values() {
        let fx = Fake::new("image-rows");
        let px = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [128, 128, 128]];
        let a = stored(
            &fx,
            "A1.png",
            &srgb8(2, 2, &px),
            "printwindow",
            Some(Encoding::Srgb8),
        );
        let mut moved = px;
        moved[3] = [128, 148, 128];
        let b = stored(
            &fx,
            "B1.png",
            &srgb8(2, 2, &moved),
            "printwindow",
            Some(Encoding::Srgb8),
        );
        let mut noisy = px;
        noisy[3] = [130, 128, 128];
        let n = stored(
            &fx,
            "A2.png",
            &srgb8(2, 2, &noisy),
            "printwindow",
            Some(Encoding::Srgb8),
        );
        let out = fx.run(
            "measure_image",
            json!({"token": "tok", "capture": a, "metrics": ["channels", "luminance", "hue_share", "diff"],
                   "hue": [200, 280], "reference": "S1\\b1.PNG", "noise_ref": n.to_string(), "name": "geralt",
                   "state_label": "A", "run_id": "s1-img", "note": "a test"}),
        );
        assert_eq!(out["ok"], true, "{out}");
        let value = |metric: &str| {
            let v = out["values"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["metric"] == metric);
            v.unwrap_or_else(|| panic!("no {metric}: {out}"))["value"]
                .as_f64()
                .unwrap()
        };
        assert_eq!(value("geralt_r_srgb8_mean"), 95.75);
        assert_eq!(value("geralt_r_srgb8_median"), 64.0);
        assert_eq!(value("geralt_hue_200_280_share"), 0.25);
        // 20 levels in one pixel of four against B1, 2 against A2.
        assert_eq!(value("geralt_diff_srgb8_mean"), 5.0);
        assert_eq!(value("geralt_floor_srgb8_mean"), 0.5);
        assert_eq!(value("geralt_diff_over_floor"), 10.0);
        assert_eq!(
            (
                out["reference"]["id"].clone(),
                out["noise_ref"]["id"].clone()
            ),
            (json!(b), json!(n))
        );
        let ids = out["ledger"]["measurements"].as_array().unwrap();
        assert_eq!(ids.len(), 9 + 4 + 1 + 3, "{out}");
        let rows = db(
            &fx,
            "SELECT DISTINCT method_family, tag, capture_id, boot_id, state_label, run_id, note \
                            FROM measurements",
        );
        let note = format!(
            "measure_image srgb8 2x2 crop 0,0 2x2 px; hue 200-280 s>=0 v>=0; reference capture {b}; \
                            noise_ref capture {n}; a test"
        );
        assert_eq!(
            rows,
            [[
                json!("screenshot"),
                json!("M"),
                json!(a),
                json!(1),
                json!("A"),
                json!("s1-img"),
                json!(note)
            ]]
        );

        // HDR: the .npy and the PQ PNG of one shot, 100 and 400 nits gray, give the same luminance in nits.
        let s: Vec<f16> = [100.0f32, 400.0]
            .iter()
            .flat_map(|n| [n / 80.0, n / 80.0, n / 80.0, 1.0])
            .map(f16::from_f32)
            .collect();
        let frame = Frame::new(2, 1, Pixels::ScrgbF16(s)).unwrap();
        let (mut npy, mut png) = (Vec::new(), Vec::new());
        frame.write_npy(&mut npy).unwrap();
        frame.write_png_pq(&mut png).unwrap();
        let ids = [
            stored(&fx, "H1.npy", &npy, "wgc_fp16", Some(Encoding::ScrgbF16)),
            stored(&fx, "H1.png", &png, "wgc_fp16", Some(Encoding::PqBt2020U16)),
        ];
        for id in ids {
            let out = fx.run(
                "measure_image",
                json!({"capture": id, "metrics": ["luminance"], "crop": [0.5, 0, 0.5, 1], "crop_unit": "fraction"}),
            );
            assert_eq!(out["crop"], json!([1, 0, 1, 1]), "{out}");
            let mean = &out["values"][0];
            assert_eq!(mean["metric"], "luminance_nits_mean", "{out}");
            assert!(
                (mean["value"].as_f64().unwrap() - 400.0).abs() < 0.2,
                "{out}"
            );
        }
    }

    /// Refused before anything is written: not quen's screenshot, not an image, gone or changed, arguments a metric
    /// doesn't take, a crop off the frame, a reference of another display state, a wrong token.
    #[test]
    fn refuses_and_writes_nothing() {
        let fx = Fake::new("image-refuse");
        let png = srgb8(2, 1, &[[1, 2, 3], [4, 5, 6]]);
        let shot = stored(&fx, "A1.png", &png, "printwindow", Some(Encoding::Srgb8));
        let rdc = stored(&fx, "R1.rdc", b"synthetic", "renderdoc", None);
        let csv = stored(&fx, "t-trace.csv", b"t_s\n", "actor_trace", None);
        let exr = stored(
            &fx,
            "D1.exr",
            b"synthetic",
            "renodx_devkit",
            Some(Encoding::PqCodesF16),
        );
        let old = stored(&fx, "O1.png", &png, "printwindow", None);
        let nsight = stored(&fx, "final.png", &png, "ngfx_extract", None);
        let changed = stored(&fx, "C1.png", &png, "printwindow", Some(Encoding::Srgb8));
        fs::write(
            fx.dir.join("data/captures/s1/C1.png"),
            srgb8(2, 1, &[[9, 9, 9], [4, 5, 6]]),
        )
        .unwrap();
        let gone = stored(&fx, "G1.png", &png, "printwindow", Some(Encoding::Srgb8));
        Ledger::open(fx.dir.join("data/state/quen.db"))
            .unwrap()
            .mark_removed(gone, "2026-10-17T13:17:00Z", "recycle_bin")
            .unwrap();
        let other = stored(&fx, "B1.png", &png, "printwindow", Some(Encoding::Srgb8));
        let ch = json!(["channels"]);
        for (args, why) in [
            (json!({"capture": rdc, "metrics": ch}), "\"renderdoc\""),
            (json!({"capture": csv, "metrics": ch}), "\"actor_trace\""),
            (json!({"capture": exr, "metrics": ch}), "\"renodx_devkit\""),
            (json!({"capture": old, "metrics": ch}), "encoding null"),
            (
                json!({"capture": nsight, "metrics": ch}),
                "is Nsight's final image (ngfx_extract's screenshot op)",
            ),
            (
                json!({"capture": changed, "metrics": ch}),
                "changed since quen wrote it",
            ),
            (json!({"capture": gone, "metrics": ch}), "was removed"),
            (json!({"capture": 999, "metrics": ch}), "no capture 999"),
            (
                json!({"capture": "s1/none.png", "metrics": ch}),
                "no capture",
            ),
            (
                json!({"capture": shot, "metrics": ["channels", "hue_share"]}),
                "needs hue",
            ),
            (
                json!({"capture": shot, "metrics": ch, "hue": [1, 2]}),
                "hue is for hue_share",
            ),
            (
                json!({"capture": shot, "metrics": ["hue_share"], "hue": [1, 400]}),
                "whole degrees",
            ),
            (
                json!({"capture": shot, "metrics": ["diff"]}),
                "diff needs reference",
            ),
            (
                json!({"capture": shot, "metrics": ch, "noise_ref": other}),
                "noise_ref is for diff",
            ),
            (
                json!({"capture": shot, "metrics": ["sharpness"]}),
                "metrics is a list",
            ),
            (
                json!({"capture": shot, "metrics": ["luminance", "luminance"]}),
                "twice",
            ),
            (
                json!({"capture": shot, "metrics": ch, "crop": [0, 0, 3, 1]}),
                "leaves the 2x1 frame",
            ),
            (
                json!({"capture": shot, "metrics": ch, "crop_unit": "fraction"}),
                "crop_unit without crop",
            ),
            (
                json!({"capture": shot, "metrics": ch, "name": "Geralt"}),
                "name",
            ),
            (
                json!({"capture": shot, "metrics": ch, "token": "nope"}),
                "wrong token",
            ),
            (
                json!({"capture": shot, "metrics": ch, "other": 1}),
                "unknown argument",
            ),
            (
                json!({"capture": shot, "metrics": ["luminance", "diff"], "reference": rdc}),
                "\"renderdoc\"",
            ),
        ] {
            let out = fx.run("measure_image", args.clone());
            let reason = out["reason"].as_str().unwrap_or_default();
            assert!(
                out["refused"] == true && reason.contains(why),
                "{args}: {out}"
            );
        }
        // Another method (or display state): compare only captures of one method, encoding and display state.
        let wgc = stored(&fx, "W1.png", &png, "wgc_fp16", Some(Encoding::Srgb8));
        let out = fx.run(
            "measure_image",
            json!({"capture": shot, "metrics": ["diff"], "reference": wgc}),
        );
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("compare only captures of one method"),
            "{out}"
        );
        assert_eq!(db(&fx, "SELECT count(*) FROM measurements"), [[json!(0)]]);
    }
}
