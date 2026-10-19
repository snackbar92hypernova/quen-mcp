//! A stand-in for REDkit's wcc_lite in script_compile's test (tests/data.rs), copied into a synthetic REDkit folder as
//! `bin\x64_RedKit\wcc_lite.exe`. `compilescripts <tree> -out=<dir> ...` prints its arguments, writes `..\wcc.log`
//! (REDkit's bin folder, as wcc_lite does) with two script errors and a warning, rewrites `profile.settings` and
//! creates `editoruser.settings` in the documents folder `fake_docs.txt` beside it names (wcc_lite writes the user's),
//! and exits 1, as a compile with errors does.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    println!("{}", args[1..].join(" "));
    if args.get(1).map(String::as_str) != Some("compilescripts") {
        std::process::exit(2);
    }
    let log = "[2026.10.20 00:55:20][Error][Core] [Script]: Error local\\a.ws[1]: Cannot find global function 'A' to \
               import.\n\
               [2026.10.20 00:55:20][Error][Core] [Script]: Error game\\x.ws[2]: Unknown function 'Nope'.\n\
               [2026.10.20 00:55:20][Warning][Core] [Script]: Warning game\\x.ws[1]: Class X has missing functions!\n";
    std::fs::write(r"..\wcc.log", log).unwrap();
    if let Ok(docs) = std::fs::read_to_string("fake_docs.txt") {
        let docs = std::path::Path::new(docs.trim());
        std::fs::write(docs.join("profile.settings"), "[Telemetry]\nrewritten=1\n").unwrap();
        std::fs::write(docs.join("editoruser.settings"), "[Editor]\nnew=1\n").unwrap();
    }
    std::process::exit(1);
}
