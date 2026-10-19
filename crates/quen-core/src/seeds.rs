//! The REA (Ghidra) seed file, generated from the RE knowledge base (docs/intent.md, "Game knowledge").
//!
//! REA reads `REA_GHIDRA_SEED_FILE`: `address<TAB>kind[<TAB>name]` lines with absolute VAs, kind `function`, `label` or
//! `code`, names of 1 to 2000 characters without whitespace, at most 1 M entries. The KB keeps RVAs, so VA = RVA +
//! [`IMAGE_BASE`]. RVAs are `0x`-prefixed hex of at most 32 bits, which also catches a VA pasted where an RVA belongs.
//!
//! REA applies a seed file to whichever exe it opens, so a seed file is made for one build: `names.tsv` names
//! its build on a `# build:` line ([`Build::of_kb`]), [`seed_file`] refuses an exe of another build, and the file goes
//! to a path named by the build ([`path`]), which `REA_GHIDRA_SEED_FILE` then names.

use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::build::Build;

/// witcher3.exe's preferred image base, where REA's Ghidra import places it.
pub const IMAGE_BASE: u64 = 0x1_4000_0000;
/// The longest name, in UTF-16 units as REA's JavaScript and Ghidra's Java count them.
pub const MAX_NAME: usize = 2000;
/// The most seeds REA reads.
pub const MAX_SEEDS: usize = 1_000_000;

const TSV_HEADER: [&str; 6] = ["rva", "name", "kind", "confidence", "source", "date"];

/// Sorted by declaration order after the address, so the file's line order is fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Function,
    Label,
    Code,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Function => "function",
            Kind::Label => "label",
            Kind::Code => "code",
        }
    }
}

/// One seed line. A function may have a name, a label must, a code seed has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seed {
    pub va: u64,
    pub kind: Kind,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A bad `names.tsv` line: header, column count, address, kind or name.
    Tsv {
        line: usize,
        msg: String,
    },
    /// A bad `func_names.json`: not an object of strings, or a bad key or name.
    Json(String),
    Duplicate {
        va: u64,
        kind: Kind,
    },
    /// REA applies labels after functions and makes them primary, so the label would rename the function.
    LabelAtFunction(u64),
    TooMany(usize),
    /// `names.tsv`'s build line is missing or bad, or the exe is another build.
    Build(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Tsv { line, msg } => write!(f, "names.tsv line {line}: {msg}"),
            Error::Json(msg) => write!(f, "func_names.json: {msg}"),
            Error::Duplicate { va, kind } => write!(f, "{va:#x} has two {} seeds", kind.as_str()),
            Error::LabelAtFunction(va) => {
                write!(
                    f,
                    "{va:#x} has a function and a label seed: the label would rename the function"
                )
            }
            Error::TooMany(n) => write!(f, "{n} seeds, REA reads at most {MAX_SEEDS}"),
            Error::Build(msg) => write!(f, "names.tsv: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// REA's seed file for `exe` from a KB `names.tsv` ([`render`] of [`from_names_tsv`]), refused unless `exe` is the
/// build the table's build line names. It goes to [`path`]`(exe)` in the KB.
pub fn seed_file(names_tsv: &str, exe: &Build) -> Result<String, Error> {
    let (build, seeds) = from_names_tsv(names_tsv)?;
    build.check_exe(exe).map_err(Error::Build)?;
    render(seeds)
}

/// Where a build's seed file goes, relative to the KB folder: `seeds/<stamp>-<first 16 hex digits of its sha256>.tsv`.
/// `REA_GHIDRA_SEED_FILE` names it, so the setting says which build its names are for, and each build's seeds are a
/// file of their own.
pub fn path(build: &Build) -> PathBuf {
    let sha: String = build.sha256.chars().take(16).collect();
    Path::new("seeds").join(format!("{}-{sha}.tsv", build.stamp))
}

/// The build a KB `names.tsv` names ([`Build::of_kb`]) and its seeds: a header row of `rva name kind confidence source
/// date`, then one row per name (see [`tsv_rows`]). An empty name cell means no name.
pub fn from_names_tsv(text: &str) -> Result<(Build, Vec<Seed>), Error> {
    let build = Build::of_kb(text).map_err(Error::Build)?;
    let err = |line, msg| Error::Tsv { line, msg };
    let rows = tsv_rows(text, &TSV_HEADER).map_err(|(line, msg)| err(line, msg))?;
    let seeds = rows
        .into_iter()
        .map(|(line, [rva, name, kind, _confidence, _source, _date])| {
            let kind = match kind {
                "function" => Kind::Function,
                "label" => Kind::Label,
                "code" => Kind::Code,
                _ => {
                    let msg = format!("kind {kind:?} is not function, label or code");
                    return Err(err(line, msg));
                }
            };
            seed(rva, kind, name).map_err(|msg| err(line, msg))
        })
        .collect::<Result<_, _>>()?;
    Ok((build, seeds))
}

/// The rows of a KB TSV file (`names.tsv`, `patches.tsv`): `header` first, then rows of as many tab-separated cells,
/// each trimmed, with their 1-based line numbers. Lines starting with `#` and blank lines are skipped. The error is a
/// line number and the reason.
pub(crate) fn tsv_rows<'a, const N: usize>(
    text: &'a str,
    header: &[&str; N],
) -> Result<Vec<Row<'a, N>>, (usize, String)> {
    let mut rows = Vec::new();
    let mut seen = false;
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let cells: Vec<&str> = line.split('\t').map(str::trim).collect();
        if !seen {
            if cells[..] != header[..] {
                return Err((
                    i + 1,
                    format!("the header must be {}", header.join(" <TAB> ")),
                ));
            }
            seen = true;
            continue;
        }
        let row = cells
            .try_into()
            .map_err(|cells: Vec<_>| (i + 1, format!("{} columns, expected {N}", cells.len())))?;
        rows.push((i + 1, row));
    }
    if !seen {
        return Err((text.lines().count() + 1, "no header row".into()));
    }
    Ok(rows)
}

/// A KB TSV row: its line number and its cells.
pub(crate) type Row<'a, const N: usize> = (usize, [&'a str; N]);

/// A KB RVA cell: `0x`-prefixed hex of at most 32 bits. The error is the reason, without the location.
pub(crate) fn rva(cell: &str) -> Result<u32, String> {
    let hex = cell
        .strip_prefix("0x")
        .or_else(|| cell.strip_prefix("0X"))
        .unwrap_or_default();
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("address {cell:?} is not 0x-prefixed hex"));
    }
    u32::from_str_radix(hex, 16).map_err(|_| format!("RVA {cell} is over 32 bits: a VA?"))
}

/// The function seeds of a `func_names.json` object: RVA keys, name values. Keys starting with `_` (such as `_about`)
/// are notes and skipped. A key that appears twice is a duplicate, not silently the last one.
pub fn from_func_names_json(text: &str) -> Result<Vec<Seed>, Error> {
    let Entries(entries) = serde_json::from_str(text).map_err(|e| Error::Json(e.to_string()))?;
    let names = entries.into_iter().filter(|(key, _)| !key.starts_with('_'));
    names
        .map(|(key, value)| {
            let name = value
                .as_str()
                .ok_or_else(|| Error::Json(format!("{key}: the name is not a string")))?;
            seed(&key, Kind::Function, name).map_err(|msg| Error::Json(format!("{key}: {msg}")))
        })
        .collect()
}

/// The seed file's text, sorted by address then kind so that reordering the inputs doesn't change it (REA treats any
/// seed change as a new analysis).
pub fn render(mut seeds: Vec<Seed>) -> Result<String, Error> {
    if seeds.len() > MAX_SEEDS {
        return Err(Error::TooMany(seeds.len()));
    }
    seeds.sort_by_key(|s| (s.va, s.kind));
    if let Some(pair) = seeds
        .windows(2)
        .find(|w| (w[0].va, w[0].kind) == (w[1].va, w[1].kind))
    {
        return Err(Error::Duplicate {
            va: pair[0].va,
            kind: pair[0].kind,
        });
    }
    // Function sorts right before Label, so a pair at one address is adjacent.
    if let Some(pair) = seeds
        .windows(2)
        .find(|w| w[0].va == w[1].va && (w[0].kind, w[1].kind) == (Kind::Function, Kind::Label))
    {
        return Err(Error::LabelAtFunction(pair[0].va));
    }
    Ok(seeds
        .iter()
        .map(|s| match &s.name {
            Some(name) => format!("{:#x}\t{}\t{name}\n", s.va, s.kind.as_str()),
            None => format!("{:#x}\t{}\n", s.va, s.kind.as_str()),
        })
        .collect())
}

/// One seed from an RVA cell, a kind and a name cell (blank for none). Each whitespace run in the name, the ends
/// trimmed first, becomes `_`. The error is the reason, without the location.
fn seed(rva: &str, kind: Kind, name: &str) -> Result<Seed, String> {
    let rva = self::rva(rva)?;
    let name = name.split_whitespace().collect::<Vec<_>>().join("_");
    let name = (!name.is_empty()).then_some(name);
    match (kind, &name) {
        (Kind::Code, Some(_)) => return Err("a code seed takes no name".into()),
        (Kind::Label, None) => return Err("a label seed needs a name".into()),
        (_, Some(name)) if name.encode_utf16().count() > MAX_NAME => {
            return Err(format!("name longer than {MAX_NAME} characters"));
        }
        _ => {}
    }
    Ok(Seed {
        va: IMAGE_BASE + u64::from(rva),
        kind,
        name,
    })
}

/// A JSON object's entries in file order, duplicate keys kept.
struct Entries(Vec<(String, serde_json::Value)>);

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntriesVisitor;
        impl<'de> Visitor<'de> for EntriesVisitor {
            type Value = Entries;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object of RVA keys and name values")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entries, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(Entries(entries))
            }
        }
        deserializer.deserialize_map(EntriesVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::Stamp;
    use crate::journal::sha256_hex;

    const HEADER: &str = "rva\tname\tkind\tconfidence\tsource\tdate\n";

    /// A synthetic build, not from any real binary.
    fn build(stamp: u32, exe: &[u8]) -> Build {
        Build {
            sha256: sha256_hex(exe),
            stamp: Stamp(stamp),
        }
    }

    fn kb() -> Build {
        build(0x0BAD_F00D, b"synthetic exe")
    }

    fn build_line(build: &Build) -> String {
        format!("# build: sha256={} stamp={}\n", build.sha256, build.stamp)
    }

    /// The build line goes anywhere: here, after the rows, so the rows keep their line numbers.
    fn names(text: &str) -> Result<Vec<Seed>, Error> {
        let (build, seeds) = from_names_tsv(&format!("{text}{}", build_line(&kb())))?;
        assert_eq!(build, kb());
        Ok(seeds)
    }

    fn tsv(rows: &str) -> Result<Vec<Seed>, Error> {
        names(&format!("{HEADER}{rows}"))
    }

    fn function(va: u64, name: &str) -> Seed {
        Seed {
            va,
            kind: Kind::Function,
            name: Some(name.into()),
        }
    }

    #[test]
    fn names_tsv_to_seed_file() {
        let text = "\
# synthetic KB names, not from any real binary
\r
rva\tname\tkind\tconfidence\tsource\tdate\r
0x1000\toperator new\tfunction\tO\ttest\t2026-01-01\r
  # an indented comment
0x2a0\t\tcode\tI\ttest\t2026-01-01
0X1F0\tTable::Lookup\u{a0} slot \tlabel\tO\ttest\t2026-01-01
0x00000010\t\tfunction\tO\ttest\t
0x1000\t\tcode\tO\ttest\t2026-01-01
";
        let seeds = names(text).unwrap();
        assert_eq!(seeds[0], function(0x1_4000_1000, "operator_new"));
        assert_eq!(
            seeds[1],
            Seed {
                va: 0x1_4000_02a0,
                kind: Kind::Code,
                name: None
            }
        );
        assert_eq!(
            seeds[3],
            Seed {
                va: 0x1_4000_0010,
                kind: Kind::Function,
                name: None
            }
        );
        assert_eq!(
            render(seeds).unwrap(),
            "0x140000010\tfunction\n\
             0x1400001f0\tlabel\tTable::Lookup_slot\n\
             0x1400002a0\tcode\n\
             0x140001000\tfunction\toperator_new\n\
             0x140001000\tcode\n"
        );
        let label = Seed {
            va: 0x1_4000_1000,
            kind: Kind::Label,
            name: Some("slot".into()),
        };
        assert_eq!(
            render(vec![label, function(0x1_4000_1000, "f")]),
            Err(Error::LabelAtFunction(0x1_4000_1000))
        );
        assert!(names(HEADER).unwrap().is_empty());
        assert_eq!(render(vec![]).unwrap(), "");
    }

    #[test]
    fn func_names_json_to_function_seeds() {
        let json = r#"{
            "_about": {"note": "synthetic names", "count": 2},
            "0x12AB34C": "CRT thread_start",
            "0x10": "  padded   name ",
            "_todo": "skipped"
        }"#;
        let seeds = from_func_names_json(json).unwrap();
        assert_eq!(
            seeds,
            vec![
                function(0x1_412A_B34C, "CRT_thread_start"),
                function(0x1_4000_0010, "padded_name")
            ]
        );
        assert_eq!(
            render(seeds).unwrap(),
            "0x140000010\tfunction\tpadded_name\n0x1412ab34c\tfunction\tCRT_thread_start\n"
        );
        assert_eq!(
            from_func_names_json(r#"{"0x10": ""}"#).unwrap(),
            vec![Seed {
                va: 0x1_4000_0010,
                kind: Kind::Function,
                name: None
            }]
        );
    }

    #[test]
    fn duplicates_are_refused_across_spellings_and_inputs() {
        let mut seeds = tsv("0x12AB34C\tfrom tsv\tfunction\tO\ttest\t2026-01-01\n").unwrap();
        seeds.extend(from_func_names_json(r#"{"0x12ab34c": "from json"}"#).unwrap());
        assert_eq!(
            render(seeds),
            Err(Error::Duplicate {
                va: 0x1_412A_B34C,
                kind: Kind::Function
            })
        );
        let seeds = tsv("0x10\ta\tlabel\tO\tt\td\n0x0010\tb\tlabel\tO\tt\td\n").unwrap();
        assert_eq!(
            render(seeds),
            Err(Error::Duplicate {
                va: 0x1_4000_0010,
                kind: Kind::Label
            })
        );
        let seeds = from_func_names_json(r#"{"0x10": "a", "0x10": "b"}"#).unwrap();
        assert_eq!(
            render(seeds),
            Err(Error::Duplicate {
                va: 0x1_4000_0010,
                kind: Kind::Function
            })
        );
    }

    #[test]
    fn bad_rows_are_refused_with_their_line() {
        for (row, why) in [
            ("1AB4\tn\tfunction\tO\tt\td", "not 0x-prefixed hex"),
            ("0x\tn\tfunction\tO\tt\td", "not 0x-prefixed hex"),
            ("0xZZ\tn\tfunction\tO\tt\td", "not 0x-prefixed hex"),
            ("0x+1A\tn\tfunction\tO\tt\td", "not 0x-prefixed hex"),
            ("4096\tn\tfunction\tO\tt\td", "not 0x-prefixed hex"),
            ("0x140001000\tn\tfunction\tO\tt\td", "over 32 bits"),
            ("0x10\t\tlabel\tO\tt\td", "needs a name"),
            ("0x10\t \tlabel\tO\tt\td", "needs a name"),
            ("0x10\tn\tcode\tO\tt\td", "takes no name"),
            ("0x10\tn\tdata\tO\tt\td", "is not function, label or code"),
            (
                "0x10\tn\tFunction\tO\tt\td",
                "is not function, label or code",
            ),
            ("0x10\tn\tfunction\tO\tt", "5 columns"),
            ("0x10 n function O t d", "1 columns"),
        ] {
            match tsv(&format!("0x20\tok\tfunction\tO\tt\td\n# note\n{row}\n")) {
                Err(Error::Tsv { line: 4, msg }) => assert!(msg.contains(why), "{row:?}: {msg}"),
                other => panic!("{row:?}: {other:?}"),
            }
        }
        assert!(matches!(
            names("# only a comment\n\n"),
            Err(Error::Tsv { line: 4, .. })
        ));
        assert!(matches!(
            names("rva\tname\tkind\n"),
            Err(Error::Tsv { line: 1, .. })
        ));
        assert!(matches!(
            names("0x10\tn\tfunction\tO\tt\td\n"),
            Err(Error::Tsv { line: 1, .. })
        ));
    }

    #[test]
    fn names_are_capped_at_2000_characters() {
        let row = |name: &str| tsv(&format!("0x10\t{name}\tfunction\tO\tt\td\n"));
        assert_eq!(
            row(&"n".repeat(MAX_NAME)).unwrap()[0]
                .name
                .as_ref()
                .unwrap()
                .len(),
            MAX_NAME
        );
        assert!(matches!(
            row(&"n".repeat(MAX_NAME + 1)),
            Err(Error::Tsv { .. })
        ));
        // Whitespace runs shrink before the cap is checked; an astral character counts as two.
        assert!(row(&format!("{} {}", "n".repeat(1000), "n".repeat(999))).is_ok());
        assert!(matches!(
            row(&format!("{}\u{1F600}", "n".repeat(MAX_NAME - 1))),
            Err(Error::Tsv { .. })
        ));
    }

    #[test]
    fn bad_json_is_refused() {
        for (json, why) in [
            (r#"["0x10"]"#, "an object"),
            (r#"{"0x10": 7}"#, "not a string"),
            (r#"{"0x10": null}"#, "not a string"),
            (r#"{"16": "n"}"#, "not 0x-prefixed hex"),
            (r#"{"0x1_0": "n"}"#, "not 0x-prefixed hex"),
            (r#"{"0x100000000": "n"}"#, "over 32 bits"),
            (r#"{"0x10": "n""#, "EOF"),
        ] {
            match from_func_names_json(json) {
                Err(Error::Json(msg)) => assert!(msg.contains(why), "{json}: {msg}"),
                other => panic!("{json}: {other:?}"),
            }
        }
    }

    #[test]
    fn at_most_a_million_seeds() {
        let seeds = vec![
            Seed {
                va: IMAGE_BASE,
                kind: Kind::Code,
                name: None
            };
            MAX_SEEDS + 1
        ];
        assert_eq!(render(seeds), Err(Error::TooMany(MAX_SEEDS + 1)));
    }

    #[test]
    fn a_seed_file_is_made_for_the_kb_s_build_only() {
        let rows = format!("{HEADER}0x1000\tf\tfunction\tO\ttest\t2026-01-01\n");
        let table = format!("{}{rows}", build_line(&kb()));
        let want = "0x140001000\tfunction\tf\n";
        assert_eq!(seed_file(&table, &kb()).as_deref(), Ok(want));

        let update = build(0x1234_5678, b"synthetic update");
        let patched = build(0x0BAD_F00D, b"synthetic exe, patched");
        for (exe, why) in [(&update, "another build"), (&patched, "the same PE stamp")] {
            let e = seed_file(&table, exe).unwrap_err();
            assert!(matches!(&e, Error::Build(msg) if msg.contains(why)), "{e}");
            assert!(
                e.to_string().starts_with("names.tsv: the KB is for "),
                "{e}"
            );
        }
        let e = seed_file(&rows, &kb()).unwrap_err();
        assert!(
            matches!(&e, Error::Build(msg) if msg.starts_with("no build line")),
            "{e}"
        );

        // Named by the build, so each build's seeds are a file of their own.
        let sha = &kb().sha256[..16];
        assert_eq!(
            path(&kb()),
            Path::new("seeds").join(format!("0x0BADF00D-{sha}.tsv"))
        );
        assert_ne!(path(&patched), path(&kb()));
        assert_ne!(path(&update), path(&kb()));
    }
}
