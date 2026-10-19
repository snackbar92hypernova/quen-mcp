//! The command channel between quen and an in-game end, as JSON lines (docs/intent.md, "The in-game package").
//!
//! Game-neutral: the Witcher 3 end is `W3Quen.asi` + `modQuen` (quen's package, `w3/`), and CET, BG3SE or UE4SS ends
//! can reuse this side.
//! One message per line, newline-terminated. A request is one flat object, the verb's fields beside `id` and `ttl_ms`:
//! `{"id":7,"ttl_ms":2000,"verb":"load","save":"..."}`. Verbs are typed; there is no raw `exec`. Unknown verbs and
//! fields, oversize lines and non-finite floats are refused both ways.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fmt;

use crate::sweep::{self, Ease, Keyframe, Mode, Path, Speed, is_default};

/// The longest request line, without its newline. The in-game end reads requests into a buffer of this size.
pub const MAX_REQUEST_BYTES: usize = 4096;
/// The longest reply line, without its newline. Larger than a request: `listsaves` returns every save name.
pub const MAX_REPLY_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    /// The in-game end drops a request older than this instead of running it late.
    pub ttl_ms: u32,
    #[serde(flatten)]
    pub verb: Verb,
}

/// `pos` and `rot` pass through unchanged, in whatever units the in-game end's scripts take. Verbs without fields are
/// empty structs, not unit variants: serde lets a unit variant of a tagged enum ignore stray fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verb", deny_unknown_fields)]
pub enum Verb {
    /// Load the save whose name matches, case-insensitively, with or without `.sav`.
    #[serde(rename = "load")]
    Load { save: String },
    #[serde(rename = "loadlast")]
    LoadLast {},
    #[serde(rename = "listsaves")]
    ListSaves {},
    #[serde(rename = "cam.get")]
    CamGet {},
    #[serde(rename = "cam.set")]
    CamSet {
        pos: [f32; 3],
        rot: [f32; 3],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fov: Option<f32>,
    },
    /// Place the player (it taints later saves).
    #[serde(rename = "teleport")]
    Teleport { pos: [f32; 3], rot: [f32; 3] },
    #[serde(rename = "settime")]
    SetTime { day: u32, hour: u32 },
    #[serde(rename = "weather")]
    Weather { name: String },
    #[serde(rename = "pause")]
    Pause { on: bool },
    /// Load a camera path on the photo-mode camera, replacing a loaded one: `realtime` plays it from the
    /// next tick by local time, `step` (with `fps`) sets frame 0 and holds. The game interpolates the keyframes as
    /// `sweep::Path` does, so they must be canonical (`Path::keys`). At most `sweep::MAX_KEYFRAMES`.
    #[serde(rename = "cam.path")]
    CamPath {
        mode: Mode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fps: Option<u32>,
        #[serde(default, skip_serializing_if = "is_default")]
        speed: Speed,
        #[serde(default, skip_serializing_if = "is_default")]
        ease: Ease,
        keys: Vec<Keyframe>,
    },
    /// Set frame `n` of a loaded step path, its time `n / fps`; the reply's data is the read-back.
    #[serde(rename = "cam.frame")]
    CamFrame { n: u32 },
    /// Unload the path; the camera stays where it is.
    #[serde(rename = "cam.stop")]
    CamStop {},
    /// Write a new manual save, as the menu's "Empty save slot" does. No fields: a request can't name a slot, a
    /// save type or an existing save, so it can't ask for an overwrite or a quicksave.
    #[serde(rename = "save")]
    Save {},
    /// Read the player's position and heading, the game time (day and hour) and the weather's name; changes nothing.
    #[serde(rename = "world.get")]
    WorldGet {},
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    /// The id of the request this answers.
    pub id: u64,
    pub ok: bool,
    /// What happened, or why not.
    pub text: String,
    /// Read-back, such as a camera pose or the save list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The line is longer than the cap for its direction.
    TooLong { len: usize, max: usize },
    /// Not one JSON object of the expected shape: bad JSON, a line break inside, an unknown verb or field, a wrong type.
    Malformed(String),
    /// Well-formed, but a value the game must not get: a non-finite float, an hour past 23, an empty name.
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TooLong { len, max } => write!(f, "message of {len} bytes, the limit is {max}"),
            Error::Malformed(msg) => write!(f, "malformed message: {msg}"),
            Error::Invalid(msg) => write!(f, "invalid message: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl Verb {
    fn check(&self) -> Result<(), Error> {
        let finite = |v: &[f32]| v.iter().all(|x| x.is_finite());
        match self {
            Verb::CamSet { pos, rot, fov }
                if !finite(pos) || !finite(rot) || fov.is_some_and(|f| !f.is_finite()) =>
            {
                Err(Error::Invalid("cam.set with a non-finite number".into()))
            }
            Verb::Teleport { pos, rot } if !finite(pos) || !finite(rot) => {
                Err(Error::Invalid("teleport with a non-finite number".into()))
            }
            Verb::SetTime { hour, .. } if *hour > 23 => {
                Err(Error::Invalid(format!("settime hour {hour}, past 23")))
            }
            Verb::Load { save: name } | Verb::Weather { name } if name.is_empty() => {
                Err(Error::Invalid("an empty name".into()))
            }
            Verb::CamPath {
                mode,
                fps,
                speed,
                ease,
                keys,
            } => sweep::check_playback(*mode, *fps)
                .and_then(|()| Path::new(keys, *speed, *ease))
                .and_then(|path| {
                    if path.keys() == keys.as_slice() {
                        Ok(())
                    } else {
                        Err(sweep::SweepError(
                            "keyframes not canonical: send Path::keys".into(),
                        ))
                    }
                })
                .map_err(|e| Error::Invalid(format!("cam.path: {e}"))),
            _ => Ok(()),
        }
    }
}

/// One request line, newline included.
pub fn encode_request(request: &Request) -> Result<String, Error> {
    request.verb.check()?;
    encode(request, MAX_REQUEST_BYTES)
}

/// A request from one line, with or without its newline.
pub fn decode_request(line: &str) -> Result<Request, Error> {
    let request: Request = decode(line, MAX_REQUEST_BYTES)?;
    request.verb.check()?;
    Ok(request)
}

/// One reply line, newline included.
pub fn encode_reply(reply: &Reply) -> Result<String, Error> {
    encode(reply, MAX_REPLY_BYTES)
}

/// A reply from one line, with or without its newline.
pub fn decode_reply(line: &str) -> Result<Reply, Error> {
    decode(line, MAX_REPLY_BYTES)
}

fn encode<T: Serialize>(message: &T, max: usize) -> Result<String, Error> {
    let mut line = serde_json::to_string(message).map_err(|e| Error::Malformed(e.to_string()))?;
    if line.len() > max {
        return Err(Error::TooLong {
            len: line.len(),
            max,
        });
    }
    line.push('\n');
    Ok(line)
}

fn decode<T: DeserializeOwned>(line: &str, max: usize) -> Result<T, Error> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.len() > max {
        return Err(Error::TooLong {
            len: line.len(),
            max,
        });
    }
    if line.contains('\n') {
        return Err(Error::Malformed("a line break inside the message".into()));
    }
    serde_json::from_str(line).map_err(|e| Error::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(verb: Verb) -> Request {
        Request {
            id: 7,
            ttl_ms: 2000,
            verb,
        }
    }

    fn every_verb() -> Vec<Verb> {
        vec![
            Verb::Load {
                save: "test save 1".into(),
            },
            Verb::LoadLast {},
            Verb::ListSaves {},
            Verb::CamGet {},
            Verb::CamSet {
                pos: [1.5, -2.25, 30.0],
                rot: [0.0, -10.5, 359.0],
                fov: Some(60.0),
            },
            Verb::CamSet {
                pos: [0.0; 3],
                rot: [0.0; 3],
                fov: None,
            },
            Verb::Teleport {
                pos: [-100.0, 200.5, 3.0],
                rot: [0.0, 0.0, 90.0],
            },
            Verb::SetTime { day: 3, hour: 23 },
            Verb::Weather {
                name: "WT_Clear".into(),
            },
            Verb::Pause { on: true },
            Verb::CamPath {
                mode: Mode::Realtime,
                fps: None,
                speed: Speed::Keys,
                ease: Ease::Linear,
                keys: path_keys(),
            },
            Verb::CamPath {
                mode: Mode::Step,
                fps: Some(30),
                speed: Speed::Constant,
                ease: Ease::EaseInOut,
                keys: path_keys(),
            },
            Verb::CamFrame { n: 4_000_000 },
            Verb::CamStop {},
            Verb::Save {},
            Verb::WorldGet {},
        ]
    }

    /// Two canonical keyframes.
    fn path_keys() -> Vec<Keyframe> {
        let key = |t: f64, x: f64, yaw: f64| Keyframe {
            t,
            pos: [x, 2.5, 3.0],
            rot: [-10.5, yaw, 0.0],
            fov: 60.0,
            ease: Ease::Linear,
        };
        vec![key(0.0, 1.0, 170.0), key(2.5, 4.25, -170.0)]
    }

    fn cam_path(mode: Mode, fps: Option<u32>, keys: Vec<Keyframe>) -> Request {
        request(Verb::CamPath {
            mode,
            fps,
            speed: Speed::Keys,
            ease: Ease::Linear,
            keys,
        })
    }

    #[test]
    fn every_verb_round_trips_as_one_line() {
        for verb in every_verb() {
            let req = request(verb);
            let line = encode_request(&req).unwrap();
            assert!(
                line.ends_with('\n') && line.matches('\n').count() == 1,
                "{line:?}"
            );
            assert_eq!(decode_request(&line).unwrap(), req);
        }
    }

    #[test]
    fn wire_shape_is_flat() {
        let line = encode_request(&request(Verb::Load { save: "s".into() })).unwrap();
        assert_eq!(
            line,
            "{\"id\":7,\"ttl_ms\":2000,\"verb\":\"load\",\"save\":\"s\"}\n"
        );
        let line = encode_request(&request(Verb::CamGet {})).unwrap();
        assert_eq!(line, "{\"id\":7,\"ttl_ms\":2000,\"verb\":\"cam.get\"}\n");
        let line = encode_request(&request(Verb::CamSet {
            pos: [1.0, 2.0, 3.0],
            rot: [0.0; 3],
            fov: None,
        }))
        .unwrap();
        assert_eq!(
            line,
            "{\"id\":7,\"ttl_ms\":2000,\"verb\":\"cam.set\",\"pos\":[1.0,2.0,3.0],\"rot\":[0.0,0.0,0.0]}\n"
        );
        // Defaults (keys speed, linear eases) are left out.
        let line = encode_request(&cam_path(Mode::Realtime, None, path_keys())).unwrap();
        assert_eq!(
            line,
            concat!(
                r#"{"id":7,"ttl_ms":2000,"verb":"cam.path","mode":"realtime","keys":["#,
                r#"{"t":0.0,"pos":[1.0,2.5,3.0],"rot":[-10.5,170.0,0.0],"fov":60.0},"#,
                r#"{"t":2.5,"pos":[4.25,2.5,3.0],"rot":[-10.5,-170.0,0.0],"fov":60.0}]}"#,
                "\n"
            )
        );
        let line = encode_request(&request(Verb::CamFrame { n: 12 })).unwrap();
        assert_eq!(
            line,
            "{\"id\":7,\"ttl_ms\":2000,\"verb\":\"cam.frame\",\"n\":12}\n"
        );
    }

    #[test]
    fn cam_path_refuses_what_the_game_must_not_play() {
        let invalid = |req: Request, needle: &str| {
            for result in [
                encode_request(&req).map(|_| ()),
                decode_request(&serde_json::to_string(&req).unwrap()).map(|_| ()),
            ] {
                match result {
                    Err(Error::Invalid(why)) => {
                        assert!(why.contains(needle), "{why:?} lacks {needle:?}")
                    }
                    other => panic!("expected an invalid cam.path about {needle:?}, got {other:?}"),
                }
            }
        };
        invalid(
            cam_path(Mode::Step, None, path_keys()),
            "step with fps None",
        );
        invalid(
            cam_path(Mode::Step, Some(0), path_keys()),
            "step with fps Some(0)",
        );
        invalid(cam_path(Mode::Step, Some(241), path_keys()), "1 to 240");
        invalid(
            cam_path(Mode::Realtime, Some(30), path_keys()),
            "realtime with fps",
        );
        invalid(
            cam_path(Mode::Realtime, None, path_keys()[..1].to_vec()),
            "1 keyframes",
        );
        let mut keys = path_keys();
        keys[1].rot[1] = 190.0;
        invalid(cam_path(Mode::Realtime, None, keys), "not canonical");
        let mut keys = path_keys();
        keys[0].pos[0] = 1.00001;
        invalid(cam_path(Mode::Realtime, None, keys), "not canonical");
        let mut keys = path_keys();
        keys[1].rot[0] = 90.0;
        invalid(cam_path(Mode::Realtime, None, keys), "pitch 90");

        let ok = encode_request(&cam_path(Mode::Realtime, None, path_keys())).unwrap();
        for bad in [
            ok.replace("\"realtime\"", "\"slowmo\""),
            ok.replace("\"fov\":60.0}", "\"fov\":60.0,\"roll\":0}"),
            ok.replace("\"keys\"", "\"frames\""),
            ok.replace("\"mode\":\"realtime\",", ""),
        ] {
            assert!(
                matches!(decode_request(&bad), Err(Error::Malformed(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_longest_cam_path_fits_a_request() {
        // Every number at its longest canonical spelling; eases on all keyframes but the last, as keys speed allows.
        let keys: Vec<Keyframe> = (0..sweep::MAX_KEYFRAMES)
            .map(|i| Keyframe {
                t: if i == 0 {
                    0.0
                } else {
                    3599.9976 + 0.0001 * i as f64
                },
                pos: [-99999.9999; 3],
                rot: [-88.9999, -179.9999, -179.9999],
                fov: 178.9999,
                ease: if i + 1 < sweep::MAX_KEYFRAMES {
                    Ease::EaseInOut
                } else {
                    Ease::Linear
                },
            })
            .collect();
        let keys = Path::new(&keys, Speed::Keys, Ease::Linear)
            .unwrap()
            .keys()
            .to_vec();
        let longest = Request {
            id: u64::MAX,
            ttl_ms: u32::MAX,
            verb: Verb::CamPath {
                mode: Mode::Step,
                fps: Some(sweep::MAX_FPS),
                speed: Speed::Keys,
                ease: Ease::Linear,
                keys,
            },
        };
        let line = encode_request(&longest).unwrap();
        assert_eq!(decode_request(&line).unwrap(), longest);
        assert_eq!(
            line.len() - 1,
            3261,
            "the sweep::MAX_KEYFRAMES doc quotes this"
        );
    }

    #[test]
    fn decodes_what_another_end_writes() {
        let req = decode_request(r#"{"verb":"cam.set","rot":[0,0,90],"pos":[1,2,3],"ttl_ms":500,"id":18446744073709551615}"#)
            .unwrap();
        assert_eq!(req.id, u64::MAX);
        assert_eq!(
            req.verb,
            Verb::CamSet {
                pos: [1.0, 2.0, 3.0],
                rot: [0.0, 0.0, 90.0],
                fov: None
            }
        );
        let req =
            decode_request("{\"id\":1,\"ttl_ms\":0,\"verb\":\"pause\",\"on\":false}\r\n").unwrap();
        assert_eq!(req.verb, Verb::Pause { on: false });
    }

    #[test]
    fn refuses_unknown_verbs_fields_and_shapes() {
        for line in [
            r#"{"id":1,"ttl_ms":1,"verb":"exec","line":"quit"}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"LOAD","save":"s"}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"load","save":"s","extra":1}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"cam.set","pos":[0,0,0],"rot":[0,0,0],"FOV":40}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"loadlast","save":"s"}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"save","slot":0}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"save","save":"ManualSave_1"}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"world.get","x":0}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"cam.set","pos":[0,0],"rot":[0,0,0]}"#,
            r#"{"id":1,"ttl_ms":1,"verb":"load"}"#,
            r#"{"id":1,"verb":"loadlast"}"#,
            r#"{"id":-1,"ttl_ms":1,"verb":"loadlast"}"#,
            r#"{"id":1,"ttl_ms":4294967296,"verb":"loadlast"}"#,
            r#"{"id":1,"ttl_ms":1}"#,
            r#"[1,2]"#,
            "{\"id\":1,\n\"ttl_ms\":1,\"verb\":\"loadlast\"}",
            "",
        ] {
            assert!(
                matches!(decode_request(line), Err(Error::Malformed(_))),
                "{line:?}"
            );
        }
        let err = decode_request(r#"{"id":1,"ttl_ms":1,"verb":"exec"}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown variant `exec`"), "{err}");
    }

    #[test]
    fn refuses_non_finite_and_out_of_range_values() {
        let nan = request(Verb::CamSet {
            pos: [f32::NAN, 0.0, 0.0],
            rot: [0.0; 3],
            fov: None,
        });
        assert!(matches!(encode_request(&nan), Err(Error::Invalid(_))));
        let inf_fov = request(Verb::CamSet {
            pos: [0.0; 3],
            rot: [0.0; 3],
            fov: Some(f32::INFINITY),
        });
        assert!(matches!(encode_request(&inf_fov), Err(Error::Invalid(_))));
        let inf = request(Verb::Teleport {
            pos: [0.0; 3],
            rot: [0.0, f32::NEG_INFINITY, 0.0],
        });
        assert!(matches!(encode_request(&inf), Err(Error::Invalid(_))));
        // 1e39 is a finite f64 but overflows f32 to infinity.
        let line = r#"{"id":1,"ttl_ms":1,"verb":"teleport","pos":[1e39,0,0],"rot":[0,0,0]}"#;
        assert!(matches!(decode_request(line), Err(Error::Invalid(_))));
        let line = r#"{"id":1,"ttl_ms":1,"verb":"settime","day":1,"hour":24}"#;
        assert!(matches!(decode_request(line), Err(Error::Invalid(_))));
        assert!(matches!(
            encode_request(&request(Verb::Weather {
                name: String::new()
            })),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            encode_request(&request(Verb::Load {
                save: String::new()
            })),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn caps_line_length_per_direction() {
        let at_cap = |extra: usize| {
            let empty = encode_request(&request(Verb::Load { save: "x".into() }))
                .unwrap()
                .len()
                - 1;
            request(Verb::Load {
                save: "x".repeat(MAX_REQUEST_BYTES - empty + 1 + extra),
            })
        };
        let line = encode_request(&at_cap(0)).unwrap();
        assert_eq!(line.len(), MAX_REQUEST_BYTES + 1);
        assert_eq!(decode_request(&line).unwrap(), at_cap(0));
        assert_eq!(
            encode_request(&at_cap(1)),
            Err(Error::TooLong {
                len: MAX_REQUEST_BYTES + 1,
                max: MAX_REQUEST_BYTES
            })
        );
        let long = format!(
            "{{\"id\":1,\"ttl_ms\":1,\"verb\":\"load\",\"save\":\"{}\"}}",
            "x".repeat(MAX_REQUEST_BYTES)
        );
        assert!(matches!(decode_request(&long), Err(Error::TooLong { .. })));

        let reply = Reply {
            id: 1,
            ok: true,
            text: "x".repeat(MAX_REQUEST_BYTES * 2),
            data: None,
        };
        assert_eq!(decode_reply(&encode_reply(&reply).unwrap()).unwrap(), reply);
        let reply = Reply {
            id: 1,
            ok: true,
            text: "x".repeat(MAX_REPLY_BYTES),
            data: None,
        };
        assert!(matches!(encode_reply(&reply), Err(Error::TooLong { .. })));
    }

    #[test]
    fn replies_round_trip() {
        let saves = Reply {
            id: 9,
            ok: true,
            text: "2 saves".into(),
            data: Some(json!(["save one", "save two"])),
        };
        let pose = Reply {
            id: 10,
            ok: true,
            text: String::new(),
            data: Some(json!({"pos": [1.5, 2.0, 3.0], "rot": [0.0, 0.0, 90.0], "fov": 60.0})),
        };
        let refused = Reply {
            id: 11,
            ok: false,
            text: "no save matches".into(),
            data: None,
        };
        for reply in [saves, pose, refused] {
            let line = encode_reply(&reply).unwrap();
            assert!(
                line.ends_with('\n') && line.matches('\n').count() == 1,
                "{line:?}"
            );
            assert_eq!(decode_reply(&line).unwrap(), reply);
        }
        assert_eq!(
            encode_reply(&Reply {
                id: 11,
                ok: false,
                text: "no".into(),
                data: None
            })
            .unwrap(),
            "{\"id\":11,\"ok\":false,\"text\":\"no\"}\n"
        );
        assert!(matches!(
            decode_reply(r#"{"id":1,"ok":true,"text":"","extra":0}"#),
            Err(Error::Malformed(_))
        ));
        assert!(matches!(
            decode_reply(r#"{"id":1,"ok":true}"#),
            Err(Error::Malformed(_))
        ));
    }
}
