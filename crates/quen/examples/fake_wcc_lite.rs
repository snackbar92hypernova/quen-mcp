//! A stand-in for REDkit's wcc_lite in script_compile's and quen_setup licence's tests (tests/data.rs), copied into a
//! synthetic REDkit folder as `bin\x64_RedKit\wcc_lite.exe`. Every run prints its arguments, rewrites
//! `profile.settings` and creates `editoruser.settings` in the documents folder `fake_docs.txt` beside it names
//! (wcc_lite writes the user's). With no command it exits 0, as wcc_lite listing its commands. `compilescripts <tree>
//! -out=<dir> ...` writes `..\wcc.log` (REDkit's bin folder, as wcc_lite does) with two script errors and a warning, and
//! exits 1, as a compile with errors does. It shows no window.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    println!("{}", args[1..].join(" "));
    if let Ok(docs) = std::fs::read_to_string("fake_docs.txt") {
        let docs = std::path::Path::new(docs.trim());
        std::fs::write(docs.join("profile.settings"), "[Telemetry]\nrewritten=1\n").unwrap();
        std::fs::write(docs.join("editoruser.settings"), "[Editor]\nnew=1\n").unwrap();
    }
    match args.get(1).map(String::as_str) {
        None => std::process::exit(0),
        Some("compilescripts") => {}
        Some(_) => std::process::exit(2),
    }
    let log = "[2026.10.20 00:55:20][Error][Core] [Script]: Error local\\a.ws[1]: Cannot find global function 'A' to \
               import.\n\
               [2026.10.20 00:55:20][Error][Core] [Script]: Error game\\x.ws[2]: Unknown function 'Nope'.\n\
               [2026.10.20 00:55:20][Warning][Core] [Script]: Warning game\\x.ws[1]: Class X has missing functions!\n";
    std::fs::write(r"..\wcc.log", log).unwrap();
    std::process::exit(1);
}
