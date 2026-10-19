//! The game's own data, read-only: its bundles ([`bundle`]) and the CR2W files in them and in REDkit's depot
//! ([`cr2w`]), and what the data tools ask of them: which paths a call means ([`Filter`]), every copy of a path across
//! the bundles ([`index`]), and the names-level question a scan tests on each file ([`Question`]).

pub mod bundle;
pub mod cr2w;

use std::collections::BTreeMap;

use crate::ledger::refs::glob;

/// A path as the data tools compare paths: lower case, `\` between folders (the engine is case-insensitive).
pub fn key(path: &str) -> String {
    path.to_lowercase().replace('/', "\\")
}

/// Which paths a call asks for: `kinds` (extensions, with or without the dot) and `pattern` (a substring, or with `*` a
/// glob over the whole path, `*` matching any run of characters), both case-insensitive. Empty: every path.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    kinds: Vec<String>,
    pattern: Option<String>,
}

impl Filter {
    pub fn new(kinds: &[String], pattern: Option<&str>) -> Filter {
        let kinds = kinds
            .iter()
            .map(|k| format!(".{}", k.trim_start_matches('.').to_lowercase()));
        Filter {
            kinds: kinds.collect(),
            pattern: pattern.map(key),
        }
    }

    /// Whether `path` (a [`key`]) is asked for.
    pub fn matches(&self, path: &str) -> bool {
        let kind = self.kinds.is_empty() || self.kinds.iter().any(|k| path.ends_with(k.as_str()));
        kind && self
            .pattern
            .as_deref()
            .is_none_or(|p| match p.contains('*') {
                true => glob(p, path),
                false => path.contains(p),
            })
    }
}

/// Every path the tables of contents `tocs` hold that `filter` asks for, by [`key`] in key order, each with its copies
/// in the order given: (the toc's index, the entry's index).
pub fn index<'a>(
    tocs: impl IntoIterator<Item = &'a bundle::Toc>,
    filter: &Filter,
) -> BTreeMap<String, Vec<(usize, usize)>> {
    let mut out: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    for (b, toc) in tocs.into_iter().enumerate() {
        for (e, entry) in toc.entries.iter().enumerate() {
            let k = key(&entry.name);
            if filter.matches(&k) {
                out.entry(k).or_default().push((b, e));
            }
        }
    }
    out
}

/// A names-level question about a CR2W file, answered from its name and import tables alone: every name in
/// `has_names` there, none of `lacks_names`, and with `imports` an import whose path is one of them or ends in `\` and
/// one (so a bare file name finds every folder's). Names compare exactly, as the file writes them (the game's own
/// files hold `Roughness_Max` beside `Roughness_max`, 2026-10-20); paths as [`key`]s.
#[derive(Debug, Clone, Default)]
pub struct Question {
    pub has_names: Vec<String>,
    pub lacks_names: Vec<String>,
    pub imports: Vec<String>,
}

impl Question {
    pub fn is_empty(&self) -> bool {
        self.has_names.is_empty() && self.lacks_names.is_empty() && self.imports.is_empty()
    }

    /// The imports that matched (empty without `imports`) when `n` answers it; None when it doesn't.
    pub fn test(&self, n: &cr2w::Names) -> Option<Vec<String>> {
        let has = |name: &String| n.names.contains(name);
        if !self.has_names.iter().all(has) || self.lacks_names.iter().any(has) {
            return None;
        }
        if self.imports.is_empty() {
            return Some(Vec::new());
        }
        let wanted: Vec<String> = self.imports.iter().map(|i| key(i)).collect();
        let hits: Vec<String> = n
            .imports
            .iter()
            .filter(|i| {
                let k = key(&i.path);
                wanted
                    .iter()
                    .any(|w| k == *w || k.ends_with(&format!("\\{w}")))
            })
            .map(|i| i.path.clone())
            .collect();
        (!hits.is_empty()).then_some(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_take_kinds_substrings_and_globs() {
        let path = key(r"Characters\Models\Geralt\D_05_mg__bear_px.redcloth");
        let f = |kinds: &[&str], pattern: Option<&str>| {
            let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
            Filter::new(&kinds, pattern).matches(&path)
        };
        assert!(f(&[], None));
        assert!(f(&["REDCLOTH"], None) && f(&[".redcloth", "w2mi"], None));
        assert!(!f(&["w2mi"], None));
        assert!(f(&[], Some("bear_PX")) && f(&[], Some("characters/models")));
        assert!(f(&[], Some("characters\\*.redcloth")) && f(&[], Some("*bear*")));
        assert!(!f(&[], Some("*.w2mi")) && !f(&["redcloth"], Some("wolf")));
    }

    #[test]
    fn the_index_lists_every_copy_in_key_order() {
        let entry = |name: &str| bundle::Entry {
            name: name.into(),
            offset: 0,
            size: 0,
            zsize: 0,
            crc32: 0,
            compression: 0,
        };
        let toc = |names: &[&str]| bundle::Toc {
            layout: 304,
            entries: names.iter().map(|n| entry(n)).collect(),
            sha256: String::new(),
        };
        let tocs = [toc(&["B\\x.w2mi", "a\\y.xbm"]), toc(&["b\\X.W2MI"])];
        let all = index(&tocs, &Filter::default());
        let keys: Vec<&String> = all.keys().collect();
        assert_eq!(keys, ["a\\y.xbm", "b\\x.w2mi"]);
        assert_eq!(all["b\\x.w2mi"], [(0, 0), (1, 0)]);
        let only = index(&tocs, &Filter::new(&["xbm".into()], None));
        assert_eq!(only.len(), 1);
    }

    #[test]
    fn questions_test_names_and_imports() {
        let n = cr2w::Names {
            version: 164,
            names: vec![
                "".into(),
                "CMaterialInstance".into(),
                "Roughness_min".into(),
            ],
            imports: vec![cr2w::Import {
                path: r"gameplay\behaviors\pc\steering\pc_steering.w2steer".into(),
                class: "CMoveSteeringBehavior".into(),
                flags: 0,
            }],
        };
        let q = |has: &[&str], lacks: &[&str], imports: &[&str]| {
            let v = |l: &[&str]| l.iter().map(|s| s.to_string()).collect();
            Question {
                has_names: v(has),
                lacks_names: v(lacks),
                imports: v(imports),
            }
            .test(&n)
        };
        assert_eq!(
            q(&["CMaterialInstance"], &["Roughness_max"], &[]),
            Some(vec![])
        );
        assert_eq!(q(&["CMaterialInstance"], &["Roughness_min"], &[]), None);
        assert_eq!(q(&["cmaterialinstance"], &[], &[]), None);
        assert_eq!(q(&[], &["roughness_min"], &[]), Some(vec![]));
        let hit = Some(vec![n.imports[0].path.clone()]);
        assert_eq!(q(&[], &[], &["PC_STEERING.w2steer"]), hit);
        assert_eq!(
            q(
                &[],
                &[],
                &["gameplay/behaviors/pc/steering/pc_steering.w2steer"]
            ),
            hit
        );
        assert_eq!(q(&[], &[], &["steering.w2steer"]), None);
    }
}
