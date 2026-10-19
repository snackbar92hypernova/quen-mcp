//! game_photo: photo mode through its own script interface, quen-w3 0.1.9's `photo.*` verbs (w3/README.md). modQuen
//! opens and closes photo mode by the game's own functions, never by keys (finding 30: one escape sometimes skipped the
//! exit dialog, and the confirm key meant for it landed in gameplay), and quen returns once its state shows the new
//! mode. The weather, time, env and grading are CR4PhotomodeEffects' own, as photo mode's menu sets them; every set
//! reads the whole photo state back, and a change it doesn't show fails.

use std::thread::sleep;
use std::time::{Duration, Instant};

use quen_core::bridge_log::{VALID, in_photo, names};
use quen_core::protocol::{self, GRADING, PhotoPart, Request, Verb};
use serde_json::{Map, Value, json};

use crate::channel::REPLY_MAX;
use crate::doctor;
use crate::game::{self, Ctx, Fields, ctx, schema, text};
use crate::session::{Call, Done, Session};

/// How long quen's state may take to show photo mode opened or closed after the game's reply: photo mode switches on
/// before its menu opens, and the menu's exit waits 1 s when the user spawned props (photomodeMenu.ws OnConfirmExit).
const MODE_WAIT: Duration = Duration::from_secs(10);
const MODE_POLL: Duration = Duration::from_millis(100);

/// The arguments the actions take, besides `token`.
const ARGS: [&str; 10] = [
    "action", "name", "path", "minute", "grading", "part", "tick_s", "cloth_s", "what", "filter",
];

pub fn tool() -> Value {
    let number = |what: &str| json!({"type": "number", "description": what});
    let string = |what: &str| json!({"type": "string", "description": what});
    let one_of =
        |of: &[&str], what: &str| json!({"type": "string", "enum": of, "description": what});
    // The ranges are photo mode's own sliders' (photomodeMenu.ws CreateEffectTab, CreateOverlayTab).
    let slider = [
        "-4 to 4 on the menu",
        "0.8 to 1.2 on the menu",
        "0.3 to 3 on the menu",
        "0 to 2 on the menu",
        "kelvin: the menu's 21 steps run 15000 to 1000, 6550 at the middle",
        "-0.7 to 0.7 on the menu",
        "0 to 0.1 on the menu",
        "0 to 20 on the menu",
    ];
    let sliders: Map<String, Value> = GRADING
        .iter()
        .zip(slider)
        .map(|(k, what)| (k.to_string(), number(what)))
        .collect();
    json!({
        "name": "game_photo",
        "description": "Photo mode through its own script interface (CR4PhotomodeEffects, as its menu calls it), \
            quen-w3 0.1.9 or later. action enter / exit: open or close photo mode by the game's own functions (exit: \
            the menu's own, no dialog; the game restores the weather), then wait up to 10 s for quen's state to show \
            it. get: photo mode's state as photo (weather, minute, env, grading, grading_at_open, rain, snow, \
            sky_clear, world_weather). weather (name), env (path), time (minute), grade (grading): set it, photo is \
            the read-back, failed when it doesn't show the change. reset (part): the world's weather, no env, or \
            the grading at open. tick (tick_s, cloth_s): one world tick and the cloth simulation; no read-back. list \
            (what, filter): names. All but enter need photo mode. env is journaled and taints later saves. \
            Refused, nothing sent: wrong token, a value out of range, no loaded game, not in photo mode (enter: in \
            it); with needs: quen-w3 0.1.8 or older. Failed: no reply within 5 s.",
        "inputSchema": schema(json!({
            "action": one_of(&["enter", "exit", "get", "weather", "env", "time", "grade", "reset", "tick", "list"], "what to do"),
            "name": string("weather: a name of the area's list (list weathers), e.g. WT_Clear"),
            "path": string("env: an environment definition's path (list envs), any case"),
            "minute": number("time: the value the menu's time slider passes, minutes of the day, 0 to below 1440"),
            "grading": {"type": "object", "properties": sliders, "additionalProperties": false,
                "description": "grade: the sliders to set, at least one; the others stay"},
            "part": one_of(&["weather", "env", "grading"], "reset: what goes back"),
            "tick_s": number("tick: seconds of one world tick, 0 to 10 (the menu passes 0.00001)"),
            "cloth_s": number("tick: seconds of cloth simulation, 0 to 10 (the menu passes 1)"),
            "what": one_of(&["weathers", "envs"], "list: the area's weather names or every env path"),
            "filter": string("list: only names holding this, any case"),
        }), &["action"]),
    })
}

/// game_photo's work: the action's verb, checked as the protocol checks it; quen's package at 0.1.9 or later, a loaded
/// game and (but to enter) photo mode in quen's state; the journal for env; then the verb, and what its reply
/// must show.
pub(crate) fn call(session: &Session, call: &mut Call, args: &Value) -> Result<Fields, Done> {
    let cx = ctx(session, args, &ARGS)?;
    let action = text(args, "action")?;
    let verb = verb(action, args)?;
    let request = Request {
        id: 1,
        ttl_ms: 0,
        verb: verb.clone(),
    };
    protocol::encode_request(&request).map_err(|e| e.to_string())?;
    doctor::installed(&[&doctor::PACKAGE], cx.config, "game_photo")?;
    doctor::package_at_least(cx.config, doctor::BEFORE_PHOTO, "game_photo", "0.1.9")?;
    let last = cx.last_flags();
    let enter = action == "enter";
    if !last.is_some_and(|f| f & VALID != 0 && (enter || in_photo(f))) {
        let need = if enter { "a loaded game" } else { "photo mode" };
        let why = format!(
            "quen's state shows {}: game_photo {action} needs {need}. Nothing sent",
            game::shown(last)
        );
        return Err(why.into());
    }
    let channel = cx.channel()?;
    call.lap("check");
    let mut fields = Map::new();
    // Photo mode's time and weather didn't outlast it (finding 33: world_get read the old hour and weather after the
    // exit; the game restores its weather in DisablePhotomode). Whether env does is untested: it stays journaled.
    let journal = match verb {
        Verb::PhotoEnv { .. } => {
            let what = format!(
                "channel {}",
                serde_json::to_string(&verb).unwrap_or_default()
            );
            Some(cx.journal(&what, None, true)?)
        }
        _ => None,
    };
    let reply = channel.call(verb.clone(), REPLY_MAX);
    call.lap("reply");
    if let Some((mut journal, id)) = journal {
        let ran = !matches!(reply, Err(crate::channel::Error::Refused(..)));
        let note = cx.settle(&mut journal, id, ran);
        fields.extend(note.map(|n| ("journal_note".to_string(), json!(n))));
    }
    let reply = reply.map_err(|e| e.done(fields.clone()))?;
    fields.insert("reply".into(), json!(reply.text));
    match verb {
        Verb::PhotoMode { on } => {
            let shown = wait_mode(&cx, on);
            call.lap("state");
            fields.insert("flags_after".into(), json!(shown.1.map(names)));
            if let Err(why) = shown.0 {
                return Err(Done::Failed(why, fields));
            }
        }
        Verb::PhotoTick { .. } => {}
        Verb::PhotoList { .. } => {
            fields.insert("names".into(), reply.data.unwrap_or(json!([])));
        }
        _ => {
            let photo = reply.data.unwrap_or_default();
            let unshown = unshown(&verb, &photo);
            fields.insert("photo".into(), photo);
            if let Some(why) = unshown {
                return Err(Done::Failed(why, fields));
            }
        }
    }
    Ok(fields)
}

/// The verb of `action`, from its arguments. Refused when one is missing or of the wrong type; the protocol checks
/// the values.
fn verb(action: &str, args: &Value) -> Result<Verb, String> {
    let number = |key: &str| -> Result<Option<f32>, String> {
        match args.get(key) {
            None => Ok(None),
            Some(v) => v
                .as_f64()
                .map(|x| Some(x as f32))
                .ok_or(format!("{key} must be a number")),
        }
    };
    let pick = |key: &str| -> Result<Value, String> {
        args.get(key)
            .cloned()
            .ok_or(format!("game_photo {action} needs {key}"))
    };
    let typed = |key: &str| -> Result<Value, String> {
        let v = pick(key)?;
        Ok(json!({ key: v }))
    };
    // The protocol's own types read the enums and the grading, so a wrong value is refused in its words.
    let from = |fields: Value| -> Result<Verb, String> {
        let mut line = json!({"verb": format!("photo.{action}")});
        line.as_object_mut()
            .unwrap()
            .extend(fields.as_object().cloned().unwrap_or_default());
        serde_json::from_value(line).map_err(|e| format!("game_photo {action}: {e}"))
    };
    Ok(match action {
        "enter" | "exit" => Verb::PhotoMode {
            on: action == "enter",
        },
        "get" => Verb::PhotoGet {},
        "weather" => Verb::PhotoWeather {
            name: text(args, "name")?.into(),
        },
        "env" => Verb::PhotoEnv {
            path: text(args, "path")?.into(),
        },
        "time" => Verb::PhotoTime {
            minute: number("minute")?.ok_or("game_photo time needs minute (a number)")?,
        },
        "grade" => from(pick("grading")?)?,
        "reset" => from(typed("part")?)?,
        "tick" => Verb::PhotoTick {
            tick_s: number("tick_s")?,
            cloth_s: number("cloth_s")?,
        },
        "list" => {
            let mut fields = typed("what")?;
            if let Some(filter) = args.get("filter") {
                fields["filter"] = filter.clone();
            }
            from(fields)?
        }
        _ => return Err(format!("unknown action {action:?}")),
    })
}

/// Waits up to [`MODE_WAIT`] for quen's state to show photo mode `on` or off. The last flags read either way; Err with
/// why when they never showed it.
fn wait_mode(cx: &Ctx, on: bool) -> (Result<(), String>, Option<i64>) {
    let start = Instant::now();
    loop {
        let last = cx.last_flags();
        if last.is_some_and(|f| in_photo(f) == on) {
            return (Ok(()), last);
        }
        if start.elapsed() >= MODE_WAIT {
            let why = format!(
                "the game answered, but quen's state still shows {} {} s later: photo mode not {}",
                game::shown(last),
                MODE_WAIT.as_secs(),
                if on { "open" } else { "closed" }
            );
            return (Err(why), last);
        }
        sleep(MODE_POLL);
    }
}

/// Why the read-back `photo` doesn't show what `verb` set, if it doesn't. A reset of the weather isn't compared: what
/// GetWeather reads after RestoreWeather is untested.
fn unshown(verb: &Verb, photo: &Value) -> Option<String> {
    let num = |v: &Value| v.as_f64().unwrap_or(f64::NAN);
    // The script reads back 4 decimals of an f32.
    let near = |got: f64, want: f64| (got - want).abs() <= 1e-4 + 1e-6 * want.abs();
    let off = |what: &str, got: &Value, want: Value| {
        Some(format!(
            "photo mode's read-back doesn't show the change: {what} is {got}, asked {want} (photo is the read-back)"
        ))
    };
    match verb {
        Verb::PhotoWeather { name } if photo["weather"] != json!(name) => {
            off("weather", &photo["weather"], json!(name))
        }
        Verb::PhotoEnv { path }
            if !photo["env"]
                .as_str()
                .is_some_and(|e| e.eq_ignore_ascii_case(path)) =>
        {
            off("env", &photo["env"], json!(path))
        }
        Verb::PhotoReset {
            part: PhotoPart::Env,
        } if photo["env"] != json!("") => off("env", &photo["env"], json!("")),
        Verb::PhotoTime { minute }
            if photo["minute"]
                .as_f64()
                .is_none_or(|m| (m - f64::from(*minute)).abs() > 0.5) =>
        {
            off("minute", &photo["minute"], json!(minute))
        }
        Verb::PhotoGrade { .. } => {
            GRADING
                .iter()
                .zip(verb.photo_values())
                .find_map(|(key, want)| {
                    let (got, want) = (&photo["grading"][key], want?);
                    (!near(num(got), f64::from(want)))
                        .then(|| off(&format!("grading.{key}"), got, json!(want)))?
                })
        }
        Verb::PhotoReset {
            part: PhotoPart::Grading,
        } => GRADING.iter().find_map(|key| {
            let (got, want) = (&photo["grading"][key], &photo["grading_at_open"][key]);
            (!near(num(got), num(want)))
                .then(|| off(&format!("grading.{key}"), got, want.clone()))?
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::tests::{fake_game, reply, served, token};
    use crate::game::tests::Fake;
    use quen_core::protocol::PhotoListed;
    use std::fs;
    use std::io::Write as _;

    /// A photo read-back as W3Quen.asi turns modQuen's into JSON.
    fn state(weather: &str, minute: f64, exposure: f64, env: &str) -> Value {
        let grading = |exposure: f64| {
            json!({"exposure": exposure, "contrast": 1, "highlights": 1, "saturation": 1, "temperature": 6550,
                   "vignette": 0, "grain": 0, "chromatic_aberration": 0})
        };
        json!({"weather": weather, "minute": minute, "env": env, "grading": grading(exposure),
               "grading_at_open": grading(0.0), "rain": 0, "snow": 0, "sky_clear": true, "world_weather": "WT_Clear"})
    }

    fn state_log(fx: &Fake) -> std::path::PathBuf {
        fx.dir.join("game/bin/x64_dx12/W3QuenState.log")
    }

    /// In photo mode (flags 2177 [valid, photo, menu], as the live check read it).
    fn in_photo_mode(test: &str) -> Fake {
        let fx = served(test);
        fs::write(state_log(&fx), "push flags=2177\n").unwrap();
        fx
    }

    /// Every set action sends its verb and answers the read-back as photo; env is journaled, the rest not (finding 33).
    #[test]
    fn the_sets_send_their_verbs_and_read_the_change_back() {
        let fx = in_photo_mode("photo-sets");
        let server = fake_game(
            &token(&fx),
            6,
            Box::new(|r| {
                let data = match &r.verb {
                    Verb::PhotoWeather { name } => state(name, 720.0, 0.0, ""),
                    Verb::PhotoEnv { .. } => state("WT_Clear", 720.0, 0.0, r"Env\X.env"),
                    Verb::PhotoTime { minute } => state("WT_Clear", f64::from(*minute), 0.0, ""),
                    Verb::PhotoGrade { exposure, .. } => {
                        state("WT_Clear", 720.0, f64::from(exposure.unwrap()), "")
                    }
                    Verb::PhotoReset { .. } | Verb::PhotoGet {} => {
                        state("WT_Clear", 720.0, 0.0, "")
                    }
                    _ => unreachable!(),
                };
                vec![reply(r.id, true, "set; data is the read-back", Some(data))]
            }),
        );
        let run = |args: Value| fx.run("game_photo", args);
        let weather = run(json!({"token": "tok", "action": "weather", "name": "WT_Snow"}));
        assert_eq!(weather["ok"], true, "{weather}");
        assert_eq!(weather["photo"]["weather"], "WT_Snow", "{weather}");
        let env = run(json!({"token": "tok", "action": "env", "path": r"env\x.env"}));
        assert_eq!(env["ok"], true, "{env}");
        let time = run(json!({"token": "tok", "action": "time", "minute": 1080}));
        assert_eq!(time["photo"]["minute"], 1080.0, "{time}");
        let grade = json!({"token": "tok", "action": "grade", "grading": {"exposure": 0.5}});
        assert_eq!(run(grade)["ok"], true);
        let reset = run(json!({"token": "tok", "action": "reset", "part": "grading"}));
        assert_eq!(reset["ok"], true, "{reset}");
        let get = run(json!({"token": "tok", "action": "get"}));
        assert_eq!(
            get["photo"]["grading_at_open"]["temperature"], 6550,
            "{get}"
        );
        let verbs: Vec<Verb> = server.join().unwrap().into_iter().map(|r| r.verb).collect();
        assert_eq!(
            verbs[1],
            Verb::PhotoEnv {
                path: r"env\x.env".into()
            }
        );
        assert_eq!(verbs[3].photo_values()[0], Some(0.5));
        assert_eq!(
            verbs[4],
            Verb::PhotoReset {
                part: PhotoPart::Grading
            }
        );
        let config = fx.shared.config().as_ref().clone().unwrap();
        let journal = fs::read_to_string(crate::session::journal_path(&config)).unwrap();
        assert_eq!(journal.matches("game_memory").count(), 1, "{journal}");
        assert!(
            !journal.contains("photo.time") && journal.contains("photo.env"),
            "{journal}"
        );
    }

    /// A set the read-back doesn't show fails, the change sent, with the read-back as photo: a slider the game kept at
    /// another value, a weather it didn't take, a reset that left the grading off its values at open, a time it kept.
    #[test]
    fn a_change_the_read_back_doesnt_show_fails() {
        let fx = in_photo_mode("photo-unshown");
        let server = fake_game(
            &token(&fx),
            4,
            Box::new(|r| {
                vec![reply(
                    r.id,
                    true,
                    "set",
                    Some(state("WT_Clear", 720.0, 0.25, "")),
                )]
            }),
        );
        for (args, needle) in [
            (
                json!({"token": "tok", "action": "grade", "grading": {"exposure": 0.5}}),
                "grading.exposure is 0.25, asked 0.5",
            ),
            (
                json!({"token": "tok", "action": "weather", "name": "WT_Snow"}),
                "weather is \"WT_Clear\"",
            ),
            (
                json!({"token": "tok", "action": "reset", "part": "grading"}),
                "grading.exposure is 0.25, asked 0.0",
            ),
            (
                json!({"token": "tok", "action": "time", "minute": 1080}),
                "minute is 720.0, asked 1080.0",
            ),
        ] {
            let out = fx.run("game_photo", args);
            assert_eq!(
                (&out["ok"], &out["refused"]),
                (&json!(false), &json!(false)),
                "{out}"
            );
            assert!(out["reason"].as_str().unwrap().contains(needle), "{out}");
            assert_eq!(out["photo"]["grading"]["exposure"], 0.25, "{out}");
        }
        assert_eq!(server.join().unwrap().len(), 4);
    }

    /// enter and exit return once quen's state shows the new mode, and fail when it never does; tick and list answer
    /// without a read-back; with the user's hands only get and list run.
    #[test]
    fn enter_and_exit_wait_for_quen_s_state() {
        let fx = served("photo-mode");
        let log = state_log(&fx);
        let answered = std::sync::atomic::AtomicUsize::new(0);
        let server = fake_game(
            &token(&fx),
            6,
            Box::new(move |r| {
                let n = answered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut state = fs::OpenOptions::new().append(true).open(&log).unwrap();
                match (&r.verb, n) {
                    (Verb::PhotoMode { on: true }, _) => state
                        .write_all(b"push flags=129\npush flags=2177\n")
                        .unwrap(),
                    // The first exit closes photo mode; the second leaves it showing.
                    (Verb::PhotoMode { on: false }, 1) => {
                        state.write_all(b"push flags=1\n").unwrap()
                    }
                    (Verb::PhotoMode { .. }, _) => {}
                    (Verb::PhotoList { what, filter }, _) => {
                        assert_eq!((*what, filter.as_deref()), (PhotoListed::Envs, Some("x")));
                        return vec![reply(r.id, true, "0 of 9 envs", Some(json!([])))];
                    }
                    _ => {}
                }
                vec![reply(r.id, true, "done", None)]
            }),
        );
        let run = |args: Value| fx.run("game_photo", args);
        let enter = run(json!({"token": "tok", "action": "enter"}));
        assert_eq!(enter["ok"], true, "{enter}");
        assert_eq!(
            enter["flags_after"],
            json!(["valid", "photo", "menu"]),
            "{enter}"
        );
        let exit = run(json!({"token": "tok", "action": "exit"}));
        assert_eq!(exit["flags_after"], json!(["valid"]), "{exit}");
        // Outside photo mode only enter goes out.
        let get = run(json!({"token": "tok", "action": "get"}));
        assert!(
            get["reason"].as_str().unwrap().contains("needs photo mode"),
            "{get}"
        );
        assert_eq!(run(json!({"token": "tok", "action": "enter"}))["ok"], true);
        let list = run(json!({"token": "tok", "action": "list", "what": "envs", "filter": "x"}));
        assert_eq!(list["names"], json!([]), "{list}");
        let tick = run(json!({"token": "tok", "action": "tick", "tick_s": 0.00001}));
        assert_eq!(tick["ok"], true, "{tick}");
        assert!(tick.get("photo").is_none(), "{tick}");
        let start = Instant::now();
        let stuck = run(json!({"token": "tok", "action": "exit"}));
        assert!(start.elapsed() >= MODE_WAIT, "{:?}", start.elapsed());
        assert_eq!(
            (&stuck["ok"], &stuck["refused"]),
            (&json!(false), &json!(false)),
            "{stuck}"
        );
        assert!(
            stuck["reason"]
                .as_str()
                .unwrap()
                .contains("photo mode not closed"),
            "{stuck}"
        );
        assert_eq!(server.join().unwrap().len(), 6);
        assert!(crate::hands::user_may(
            "game_photo",
            &json!({"action": "get"})
        ));
        assert!(crate::hands::user_may(
            "game_photo",
            &json!({"action": "list"})
        ));
        assert!(!crate::hands::user_may(
            "game_photo",
            &json!({"action": "exit"})
        ));
    }

    #[test]
    fn game_photo_refuses_before_sending() {
        let fx = in_photo_mode("photo-refuse");
        let server = fake_game(&token(&fx), 0, Box::new(|_| Vec::new()));
        for args in [
            json!({"token": "tok"}),
            json!({"token": "tok", "action": "zoom"}),
            json!({"token": "tok", "action": "get", "extra": 1}),
            json!({"token": "tok", "action": "weather"}),
            json!({"token": "tok", "action": "weather", "name": ""}),
            json!({"token": "tok", "action": "time", "minute": 1440}),
            json!({"token": "tok", "action": "time", "minute": "noon"}),
            json!({"token": "tok", "action": "grade"}),
            json!({"token": "tok", "action": "grade", "grading": {}}),
            json!({"token": "tok", "action": "grade", "grading": {"dof": 1}}),
            json!({"token": "tok", "action": "reset", "part": "camera"}),
            json!({"token": "tok", "action": "tick"}),
            json!({"token": "tok", "action": "tick", "cloth_s": 11}),
            json!({"token": "tok", "action": "list", "what": "props"}),
            json!({"token": "tok", "action": "list", "what": "envs", "filter": ""}),
        ] {
            let out = fx.run("game_photo", args.clone());
            assert_eq!(out["refused"], true, "{args}: {out}");
        }
        assert!(server.join().unwrap().is_empty(), "nothing was sent");
    }
}
