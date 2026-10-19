//! The game-build pin (docs/intent.md, "The game build"). quen finds a game update itself, never by a mod refusing to
//! load. The exe's sha256 is a build's identity, its PE header's TimeDateStamp the label people read, and Steam's
//! `buildid` ([`crate::detect::steam_buildid`]) a second, independent sign. Before a launch changes anything, the exe
//! is checked against the known builds ([`gate`]). The RE knowledge base's tables name the build they were made on
//! ([`Build::of_kb`]), so names, RVAs and seeds from one build are never applied to another.
//!
//! The Windows layer reads the config and passes the platform in; nothing here knows a game.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::str::FromStr;

use serde::{Serialize, Serializer};

use crate::journal::sha256_hex;

/// What a PE file header says of its build: the linker's TimeDateStamp, and the machine the image is for
/// (`IMAGE_FILE_MACHINE_*`, 0x8664 for x64).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pe {
    pub machine: u16,
    pub stamp: Stamp,
}

impl Pe {
    /// The file header of an image's bytes: a DOS header (`MZ`) whose `e_lfanew` points at the PE signature and the
    /// file header. Anything else is refused, saying what is missing.
    pub fn parse(bytes: &[u8]) -> Result<Pe, String> {
        if !bytes.starts_with(b"MZ") {
            return Err("no MZ header: not an exe".into());
        }
        let e_lfanew = bytes.get(0x3C..0x40).ok_or("the DOS header is cut off")?;
        let pe = u32::from_le_bytes(e_lfanew.try_into().expect("4 bytes")) as usize;
        if bytes.get(pe..pe.saturating_add(4)) != Some(b"PE\0\0") {
            return Err(format!(
                "no PE signature at {pe:#x}: a DOS program, or not an exe"
            ));
        }
        // After the signature: Machine, NumberOfSections, TimeDateStamp, and 12 more bytes of file header.
        let header = bytes
            .get(pe + 4..pe + 24)
            .ok_or("the PE file header is cut off")?;
        Ok(Pe {
            machine: u16::from_le_bytes([header[0], header[1]]),
            stamp: Stamp(u32::from_le_bytes(
                header[4..8].try_into().expect("4 bytes"),
            )),
        })
    }
}

/// A PE TimeDateStamp, a build's label: written `0x` and 8 uppercase hex digits, as the KB writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Stamp(pub u32);

impl fmt::Display for Stamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:08X}", self.0)
    }
}

/// `0x` and exactly 8 hex digits, of either case: a digit short is another stamp, not this one.
impl FromStr for Stamp {
    type Err = String;

    fn from_str(s: &str) -> Result<Stamp, String> {
        let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"));
        hex.filter(|h| h.len() == 8 && h.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|h| u32::from_str_radix(h, 16).ok())
            .map(Stamp)
            .ok_or_else(|| format!("stamp {s:?} is not 0x and 8 hex digits"))
    }
}

impl Serialize for Stamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// A game build: its exe's sha256 (lowercase hex), the identity, and its PE stamp, the label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Build {
    pub sha256: String,
    pub stamp: Stamp,
}

impl fmt::Display for Build {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256 {} (PE stamp {})", self.sha256, self.stamp)
    }
}

impl Build {
    /// The build of an exe's bytes, refused unless they are a PE image ([`Pe::parse`]).
    pub fn of_exe(bytes: &[u8]) -> Result<Build, String> {
        let pe = Pe::parse(bytes)?;
        Ok(Build {
            sha256: sha256_hex(bytes),
            stamp: pe.stamp,
        })
    }

    /// The build of the exe at `path`. Errors name the path; one that isn't a PE image is `InvalidData`.
    pub fn read_exe(path: &Path) -> io::Result<Build> {
        let at = |kind, why: &dyn fmt::Display| {
            io::Error::new(kind, format!("{}: {why}", path.display()))
        };
        let bytes = fs::read(path).map_err(|e| at(e.kind(), &e))?;
        Build::of_exe(&bytes).map_err(|why| at(io::ErrorKind::InvalidData, &why))
    }

    /// The build a KB table (`names.tsv`, `hooks.tsv`) was made on: its one `# build: sha256=<64 hex> stamp=0x<8 hex>`
    /// line, a comment to the table parsers, anywhere in the file. A table without one is refused, since nothing then
    /// says which build its RVAs are for. The error is the reason, with its line number when it has one.
    pub fn of_kb(text: &str) -> Result<Build, String> {
        let mut found: Option<(usize, Build)> = None;
        for (i, line) in text.lines().enumerate() {
            let comment = line.trim().strip_prefix('#').map(str::trim_start);
            let Some(fields) = comment.and_then(|c| c.strip_prefix("build:")) else {
                continue;
            };
            let at = |why: String| format!("line {}: {why}", i + 1);
            if let Some((first, _)) = &found {
                return Err(at(format!(
                    "a second build line (the first is line {first})"
                )));
            }
            let (mut sha, mut stamp) = (None, None);
            for field in fields.split_whitespace() {
                match field.split_once('=') {
                    Some(("sha256", v)) if sha.is_none() => sha = Some(sha256(v).map_err(at)?),
                    Some(("stamp", v)) if stamp.is_none() => stamp = Some(v.parse().map_err(at)?),
                    _ => return Err(at(format!("{field:?} is not one sha256= and one stamp="))),
                }
            }
            let (Some(sha256), Some(stamp)) = (sha, stamp) else {
                return Err(at("the build line needs both sha256= and stamp=".into()));
            };
            found = Some((i + 1, Build { sha256, stamp }));
        }
        found.map(|(_, build)| build).ok_or_else(|| {
            "no build line (# build: sha256=<64 hex> stamp=0x<8 hex>): which build are its RVAs for?".into()
        })
    }

    /// Refuses unless `exe` is this build, a KB table's: its names, RVAs and seeds are this build's only.
    pub fn check_exe(&self, exe: &Build) -> Result<(), String> {
        let why = match (self.sha256 == exe.sha256, self.stamp == exe.stamp) {
            (true, true) => return Ok(()),
            (true, false) => {
                "the same sha256 with another stamp: the KB's build line has its stamp wrong"
            }
            (false, true) => "the same PE stamp, other bytes: a patched or damaged exe",
            (false, false) => "another build: a game update, or another store's exe",
        };
        Err(format!("the KB is for {self}, but the exe is {exe}: {why}"))
    }
}

/// A sha256 cell: 64 hex digits of either case, as lowercase (the form [`sha256_hex`] and the ledger use).
fn sha256(cell: &str) -> Result<String, String> {
    if cell.len() == 64 && cell.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(cell.to_ascii_lowercase())
    } else {
        Err(format!("sha256 {cell:?} is not 64 hex digits"))
    }
}

/// One build quen knows: its exe's sha256, the platform it was verified on, and what people call it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Known {
    pub platform: &'static str,
    pub sha256: String,
    pub label: String,
}

/// The builds quen knows. The local config's value (key `known_builds`, read by the Windows layer) lists them:
///
/// ```text
/// known_builds = 'steam <sha256> 5.01 (Steam build <buildid>); gog <sha256> 5.01 (GOG)'
/// ```
///
/// Each entry is `<platform> <sha256> <label>`, with `;` between entries: the platform the build was verified on
/// (`steam` or `gog`, as [`crate::detect`] names them), the exe's sha256 (64 hex digits), and a label for people (the
/// rest of the entry). Blank entries are skipped, so `''` lists none. The adapter's own list and the config's are
/// parsed as one value, the adapter's first: the same build listed twice is kept once, and refused with two labels.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnownBuilds(pub Vec<Known>);

impl KnownBuilds {
    pub fn parse(value: &str) -> Result<KnownBuilds, String> {
        let mut known: Vec<Known> = Vec::new();
        for (i, entry) in value.split(';').map(str::trim).enumerate() {
            if entry.is_empty() {
                continue;
            }
            let bad = |why: String| format!("known_builds entry {} ({entry:?}): {why}", i + 1);
            let (platform, rest) = entry.split_once(char::is_whitespace).unwrap_or((entry, ""));
            let rest = rest.trim_start();
            let (sha, label) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let platform = match platform {
                "steam" => "steam",
                "gog" => "gog",
                other => return Err(bad(format!("platform {other:?} is not steam or gog"))),
            };
            let sha256 = sha256(sha).map_err(bad)?;
            let label = label.trim();
            if label.is_empty() {
                return Err(bad(
                    "no label: entries are <platform> <sha256> <label>".into()
                ));
            }
            match known
                .iter()
                .find(|k| k.platform == platform && k.sha256 == sha256)
            {
                Some(k) if k.label == label => {}
                Some(k) => {
                    return Err(bad(format!("listed before as {:?}: which is it?", k.label)));
                }
                None => known.push(Known {
                    platform,
                    sha256,
                    label: label.into(),
                }),
            }
        }
        Ok(KnownBuilds(known))
    }
}

/// The build gate's decision on an exe, made before a launch changes anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Gate {
    /// A build known on the install's platform.
    Known { label: String },
    /// Not known: the launch is refused, nothing changed. `why` names the sha256 and the stamp.
    Unknown { why: String },
    /// Not known, but the launch passed `allow_unknown_build`: it goes ahead, and the boot row records that.
    AllowedUnknown { why: String },
}

impl Gate {
    /// The refusal, if the gate refuses: an unknown build without the override.
    pub fn refusal(&self) -> Option<&str> {
        match self {
            Gate::Unknown { why } => Some(why),
            _ => None,
        }
    }
}

/// Whether `exe` may launch from an install on `platform` (as [`crate::detect`] names it, `None` for a folder no store
/// lists): known if `known` lists its sha256 for that platform. A GOG install is matched by `gog` entries only, so its
/// exe is an unknown build until someone verifies it there, never known by Steam's hashes, even for the same
/// bytes: GOG's other files may differ. A folder no store lists (a copied install) is matched by Steam's entries, the
/// platform quen is verified on. An unknown build is refused unless `allow_unknown`.
pub fn gate(exe: &Build, platform: Option<&str>, known: &KnownBuilds, allow_unknown: bool) -> Gate {
    let on = platform.unwrap_or("steam");
    let entry = |platform: &str| {
        known
            .0
            .iter()
            .find(|k| k.platform == platform && k.sha256 == exe.sha256)
    };
    if let Some(k) = entry(on) {
        return Gate::Known {
            label: k.label.clone(),
        };
    }
    let why = match (on, entry("steam")) {
        ("gog", Some(steam)) => format!(
            "the same bytes as Steam's {:?}, but GOG is untested: its other files may differ, so Steam's known \
             builds don't count for it",
            steam.label
        ),
        ("gog", None) => {
            "a GOG install's exe is an unknown build until it is verified there: GOG's exe and files may differ \
             from Steam's"
                .into()
        }
        _ => "not a build quen knows: a game update, or another store's exe".into(),
    };
    let place = match platform {
        Some(p) => format!("on {p}"),
        None => "in a folder no store lists".into(),
    };
    let why = format!(
        "the game's exe {place} is {exe}: {why}. Once it is verified, add `{on} {} <label>` to known_builds in \
         quen's config, or launch with allow_unknown_build (recorded on the boot row)",
        exe.sha256
    );
    if allow_unknown {
        Gate::AllowedUnknown { why }
    } else {
        Gate::Unknown { why }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A synthetic PE image, not from any real binary: a DOS header pointing at the PE header at 0x80, `machine` and
    /// `stamp` in its file header, then `body`.
    fn exe(machine: u16, stamp: u32, body: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 0x98];
        b[..2].copy_from_slice(b"MZ");
        b[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        b[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        b[0x86..0x88].copy_from_slice(&1u16.to_le_bytes()); // NumberOfSections
        b[0x88..0x8C].copy_from_slice(&stamp.to_le_bytes());
        b.extend_from_slice(body);
        b
    }

    fn build(stamp: u32, body: &[u8]) -> Build {
        Build::of_exe(&exe(0x8664, stamp, body)).unwrap()
    }

    #[test]
    fn a_pe_header_gives_the_stamp_and_the_machine() {
        let pe = Pe::parse(&exe(0x8664, 0x1234_ABCD, b"code")).unwrap();
        assert_eq!(
            pe,
            Pe {
                machine: 0x8664,
                stamp: Stamp(0x1234_ABCD)
            }
        );
        assert_eq!(pe.stamp.to_string(), "0x1234ABCD");
        let x86 = Pe::parse(&exe(0x014C, 0xFF, b"")).unwrap();
        assert_eq!(
            (x86.machine, x86.stamp.to_string()),
            (0x014C, "0x000000FF".into())
        );

        let good = exe(0x8664, 1, b"");
        let mut ne = good.clone();
        ne[0x80..0x82].copy_from_slice(b"NE");
        let mut far = good.clone();
        far[0x3C..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
        let cases: [(&[u8], &str); 8] = [
            (b"", "no MZ header"),
            (b"\x7fELF\x02\x01\x01\0", "no MZ header"),
            (b"ZM", "no MZ header"),
            (b"MZ", "the DOS header is cut off"),
            (&good[..0x3E], "the DOS header is cut off"),
            (&ne, "no PE signature at 0x80: a DOS program"),
            (&far, "no PE signature at 0xffffffff"),
            (&good[..0x97], "the PE file header is cut off"),
        ];
        for (bytes, why) in cases {
            let e = Pe::parse(bytes).unwrap_err();
            assert!(e.contains(why), "{why}: {e}");
        }
    }

    #[test]
    fn stamps_are_written_as_the_kb_writes_them() {
        assert_eq!("0x0BADF00D".parse(), Ok(Stamp(0x0BAD_F00D)));
        assert_eq!("0X0badf00d".parse(), Ok(Stamp(0x0BAD_F00D)));
        assert_eq!(Stamp(10).to_string(), "0x0000000A");
        assert_eq!(serde_json::to_value(Stamp(10)).unwrap(), "0x0000000A");
        for bad in [
            "0BADF00D",
            "0xBADF00D",
            "0x00BADF00D",
            "0x0BADF00G",
            "0x+BADF00D",
            "0x",
            "",
        ] {
            let e = bad.parse::<Stamp>().unwrap_err();
            assert!(e.contains("not 0x and 8 hex digits"), "{bad}: {e}");
        }
    }

    #[test]
    fn an_exe_s_build_is_its_sha256_and_its_stamp() {
        let bytes = exe(0x8664, 0x1111_2222, b"synthetic build A");
        let a = Build::of_exe(&bytes).unwrap();
        assert_eq!(
            a,
            Build {
                sha256: sha256_hex(&bytes),
                stamp: Stamp(0x1111_2222)
            }
        );
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            json!({"sha256": sha256_hex(&bytes), "stamp": "0x11112222"})
        );
        assert_eq!(
            a.to_string(),
            format!("sha256 {} (PE stamp 0x11112222)", a.sha256)
        );
        // The same stamp over other bytes is another build.
        let b = build(0x1111_2222, b"synthetic build B");
        assert_ne!(a.sha256, b.sha256);
        assert_eq!(
            Build::of_exe(b"not an exe"),
            Err("no MZ header: not an exe".into())
        );

        let dir = std::env::temp_dir().join(format!("quen-build-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (good, bad, missing) = (
            dir.join("game.exe"),
            dir.join("notes.exe"),
            dir.join("none.exe"),
        );
        fs::write(&good, &bytes).unwrap();
        fs::write(&bad, "MZ").unwrap();
        let read = (
            Build::read_exe(&good),
            Build::read_exe(&bad),
            Build::read_exe(&missing),
        );
        fs::remove_dir_all(&dir).unwrap(); // this test's own temp folder
        assert_eq!(read.0.unwrap(), a);
        let bad_err = read.1.unwrap_err();
        assert_eq!(bad_err.kind(), io::ErrorKind::InvalidData);
        let text = bad_err.to_string();
        assert!(
            text.contains("notes.exe") && text.contains("the DOS header is cut off"),
            "{text}"
        );
        let missing_err = read.2.unwrap_err();
        assert_eq!(missing_err.kind(), io::ErrorKind::NotFound);
        assert!(
            missing_err.to_string().contains("none.exe"),
            "{missing_err}"
        );
    }

    #[test]
    fn the_kb_names_its_build_on_a_comment_line() {
        let exe = build(0x0BAD_F00D, b"synthetic KB build");
        let line = format!("# build: sha256={} stamp=0x0BADF00D", exe.sha256);
        let table = format!(
            "# synthetic KB names, not from any real binary\n{line}\nrva\tname\tkind\tconfidence\tsource\tdate\n\
             0x1000\tf\tfunction\tO\ttest\t2026-01-01\n"
        );
        assert_eq!(Build::of_kb(&table), Ok(exe.clone()));
        assert_eq!(exe.check_exe(&exe), Ok(()));
        // Indented, no space after #, the fields swapped, an uppercase sha256: the same build.
        let loose = format!(
            "  #build:  stamp=0x0badf00d\tsha256={}\n",
            exe.sha256.to_uppercase()
        );
        assert_eq!(Build::of_kb(&loose), Ok(exe.clone()));

        let sha = &exe.sha256;
        for (text, why) in [
            ("rva\tname\n# a comment\n".to_owned(), "no build line"),
            ("# builds: notes\n".to_owned(), "no build line"),
            (
                format!("x\n{line}\n{line}\n"),
                "line 3: a second build line (the first is line 2)",
            ),
            (
                "# build: see notes\n".to_owned(),
                "line 1: \"see\" is not one sha256= and one stamp=",
            ),
            (
                format!("# build: sha256={sha}\n"),
                "line 1: the build line needs both",
            ),
            (
                "# build: stamp=0x0BADF00D\n".to_owned(),
                "line 1: the build line needs both",
            ),
            ("# build:\n".to_owned(), "line 1: the build line needs both"),
            (
                format!("# build: sha256={sha} stamp=0x0BADF00D sha256={sha}\n"),
                "is not one sha256=",
            ),
            (
                format!("# build: sha256={sha} stamp=0x0BADF00D build=1\n"),
                "\"build=1\" is not one",
            ),
            (
                format!("# build: sha256={} stamp=0x0BADF00D\n", &sha[1..]),
                "is not 64 hex digits",
            ),
            (
                format!("# build: sha256={sha} stamp=0xBADF00D\n"),
                "is not 0x and 8 hex digits",
            ),
        ] {
            let e = Build::of_kb(&text).unwrap_err();
            assert!(e.contains(why), "{text:?}: {e}");
        }
    }

    #[test]
    fn an_exe_of_another_build_is_refused_by_the_kb() {
        let kb = build(0x1111_1111, b"synthetic build A");
        let update = build(0x2222_2222, b"synthetic build B");
        let patched = build(0x1111_1111, b"synthetic build A, patched");
        let wrong_line = Build {
            stamp: Stamp(0x3333_3333),
            ..kb.clone()
        };
        for (exe, kb, why) in [
            (&update, &kb, "another build: a game update"),
            (&patched, &kb, "the same PE stamp, other bytes"),
            (&kb, &wrong_line, "the KB's build line has its stamp wrong"),
        ] {
            let e = kb.check_exe(exe).unwrap_err();
            assert!(e.contains(why), "{e}");
            assert!(
                e.contains(&format!("the KB is for {kb}, but the exe is {exe}")),
                "{e}"
            );
        }
    }

    #[test]
    fn known_builds_parse_from_the_config_value() {
        let (a, b) = (sha256_hex(b"synthetic A"), sha256_hex(b"synthetic B"));
        let value = format!(
            " steam {a} 1.0 (Steam build 7) ;; gog  {}\tGOG 1.0;steam {b} 1.1; steam {a} 1.0 (Steam build 7);",
            b.to_uppercase()
        );
        let known = KnownBuilds::parse(&value).unwrap();
        let entry = |platform, sha256: &str, label: &str| Known {
            platform,
            sha256: sha256.into(),
            label: label.into(),
        };
        assert_eq!(
            known.0,
            [
                entry("steam", &a, "1.0 (Steam build 7)"),
                entry("gog", &b, "GOG 1.0"),
                entry("steam", &b, "1.1"),
            ]
        );
        assert_eq!(KnownBuilds::parse(""), Ok(KnownBuilds::default()));
        assert_eq!(KnownBuilds::parse(" ; "), Ok(KnownBuilds::default()));

        for (value, why) in [
            (format!("steam {a}"), "entry 1 (\"steam "),
            (format!("steam {a}"), "no label"),
            (
                format!("epic {a} 1.0"),
                "platform \"epic\" is not steam or gog",
            ),
            (
                format!("Steam {a} 1.0"),
                "platform \"Steam\" is not steam or gog",
            ),
            (format!("{a} 1.0"), "is not steam or gog"),
            (
                "steam 1.0".to_owned(),
                "sha256 \"1.0\" is not 64 hex digits",
            ),
            (format!("steam {}z 1.0", &a[1..]), "is not 64 hex digits"),
            (format!("steam {a} 1.0; steam {a} 1.1"), "entry 2 (\"steam "),
            (
                format!("steam {a} 1.0; steam {a} 1.1"),
                "listed before as \"1.0\": which is it?",
            ),
        ] {
            let e = KnownBuilds::parse(&value).unwrap_err();
            assert!(e.starts_with("known_builds entry "), "{e}");
            assert!(e.contains(why), "{value}: {e}");
        }
    }

    #[test]
    fn the_gate_knows_a_listed_build_and_refuses_another_naming_both_values() {
        let (v1, v2) = (build(0x1111_1111, b"v1"), build(0x2222_2222, b"v2"));
        let known = KnownBuilds::parse(&format!("steam {} 1.0", v1.sha256)).unwrap();
        let one = Gate::Known {
            label: "1.0".into(),
        };
        assert_eq!(gate(&v1, Some("steam"), &known, false), one);
        assert_eq!(
            gate(&v1, Some("steam"), &known, true),
            one,
            "the override is unused"
        );
        assert_eq!(
            gate(&v1, None, &known, false),
            one,
            "a copied folder counts as Steam's"
        );
        assert_eq!(one.refusal(), None);
        assert_eq!(
            serde_json::to_value(&one).unwrap(),
            json!({"status": "known", "label": "1.0"})
        );

        let refused = gate(&v2, Some("steam"), &known, false);
        let why = refused.refusal().unwrap();
        assert!(
            why.contains(&v2.sha256) && why.contains("0x22222222"),
            "{why}"
        );
        assert!(why.contains("not a build quen knows"), "{why}");
        assert!(
            why.contains(&format!(
                "add `steam {} <label>` to known_builds",
                v2.sha256
            )),
            "{why}"
        );
        assert!(why.contains("allow_unknown_build"), "{why}");
        let allowed = gate(&v2, Some("steam"), &known, true);
        assert_eq!(
            allowed,
            Gate::AllowedUnknown {
                why: why.to_owned()
            }
        );
        assert_eq!(allowed.refusal(), None);
        assert_eq!(
            serde_json::to_value(&allowed).unwrap()["status"],
            "allowed_unknown"
        );
        let why = gate(&v2, None, &known, false).refusal().unwrap().to_owned();
        assert!(
            why.contains("in a folder no store lists") && why.contains("add `steam "),
            "{why}"
        );
        assert!(
            gate(&v1, Some("steam"), &KnownBuilds::default(), false)
                .refusal()
                .is_some()
        );
    }

    #[test]
    fn gog_is_an_unknown_build_until_verified_there() {
        let (v1, other) = (build(0x1111_1111, b"v1"), build(0x1111_1111, b"GOG's own"));
        let steam_only = KnownBuilds::parse(&format!("steam {} 1.0", v1.sha256)).unwrap();
        // The same bytes as Steam's known build: still unknown on GOG.
        let why = gate(&v1, Some("gog"), &steam_only, false)
            .refusal()
            .unwrap()
            .to_owned();
        assert!(
            why.contains("on gog") && why.contains(&v1.sha256) && why.contains("0x11111111"),
            "{why}"
        );
        assert!(
            why.contains("the same bytes as Steam's \"1.0\", but GOG is untested"),
            "{why}"
        );
        assert!(
            why.contains(&format!("add `gog {} <label>`", v1.sha256)),
            "{why}"
        );
        let why = gate(&other, Some("gog"), &steam_only, false)
            .refusal()
            .unwrap()
            .to_owned();
        assert!(why.contains("until it is verified there"), "{why}");
        assert!(matches!(
            gate(&v1, Some("gog"), &steam_only, true),
            Gate::AllowedUnknown { .. }
        ));
        // Once verified, a gog entry knows it, and a GOG-only entry doesn't count for Steam.
        let gog = KnownBuilds::parse(&format!("gog {} 1.0 (GOG)", other.sha256)).unwrap();
        assert_eq!(
            gate(&other, Some("gog"), &gog, false),
            Gate::Known {
                label: "1.0 (GOG)".into()
            }
        );
        assert!(gate(&other, Some("steam"), &gog, false).refusal().is_some());
        assert!(gate(&other, None, &gog, false).refusal().is_some());
    }
}
