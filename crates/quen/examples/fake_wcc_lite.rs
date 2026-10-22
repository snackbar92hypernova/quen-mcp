//! A stand-in for REDkit's wcc_lite in script_compile's and quen_setup licence's tests (tests/data.rs), copied into a
//! synthetic REDkit folder as `bin\x64_RedKit\wcc_lite.exe`. Every run prints its arguments, rewrites
//! `profile.settings` and creates `editoruser.settings` in the documents folder `fake_docs.txt` beside it names
//! (wcc_lite writes the user's). With no command it exits 0, as wcc_lite listing its commands. `compilescripts <tree>
//! -out=<dir> ...` reads each line of the tree's `.ws` files: `import function <name>(` binds when <name> is one of
//! [`ENGINE`] and is an error otherwise, and a plain `function <name>(` of one of them is an error too (a synthetic
//! message). It writes `..\wcc.log` (REDkit's bin folder, as wcc_lite does) with those errors and a warning, and exits
//! 1 with errors, else 0. It shows no window.

use std::fs;
use std::path::{Path, PathBuf};

/// The natives the stand-in binds itself, as wcc_lite binds the engine's.
const ENGINE: [&str; 1] = ["SetCommunityRadius"];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    println!("{}", args[1..].join(" "));
    if let Ok(docs) = fs::read_to_string("fake_docs.txt") {
        let docs = Path::new(docs.trim());
        fs::write(docs.join("profile.settings"), "[Telemetry]\nrewritten=1\n").unwrap();
        fs::write(docs.join("editoruser.settings"), "[Editor]\nnew=1\n").unwrap();
    }
    match args.get(1).map(String::as_str) {
        None => std::process::exit(0),
        Some("compilescripts") => {}
        Some(_) => std::process::exit(2),
    }
    let tree = Path::new(&args[2]);
    let at = "[2026.10.20 00:55:20]";
    let mut log = format!(
        "{at}[Warning][Core] [Script]: Warning game\\x.ws[1]: Class X has missing functions!\n"
    );
    let mut errors = 0;
    let mut todo: Vec<PathBuf> = vec![tree.into()];
    while let Some(dir) = todo.pop() {
        for path in fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()) {
            if path.is_dir() {
                todo.push(path);
                continue;
            }
            let text = fs::read_to_string(&path).unwrap_or_default();
            let file = path.strip_prefix(tree).unwrap().display().to_string();
            for (n, line) in text.lines().enumerate() {
                let (import, rest) = match line.strip_prefix("import function ") {
                    Some(rest) => (true, rest),
                    None => match line.strip_prefix("function ") {
                        Some(rest) => (false, rest),
                        None => continue,
                    },
                };
                let name = rest.split('(').next().unwrap_or_default();
                let msg = match (import, ENGINE.contains(&name)) {
                    (true, false) => format!("Cannot find global function '{name}' to import."),
                    (false, true) => format!("'{name}' is a native function (synthetic)."),
                    _ => continue,
                };
                errors += 1;
                log += &format!(
                    "{at}[Error][Core] [Script]: Error {file}[{}]: {msg}\n",
                    n + 1
                );
            }
        }
    }
    fs::write(r"..\wcc.log", log).unwrap();
    std::process::exit(i32::from(errors > 0));
}
