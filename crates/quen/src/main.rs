//! quen: the MCP server that drives one game session for mod testing: the stdio proxy and the
//! daemon, the lock, session token, Job Object and journal of record, and the native boot. Windows only; elsewhere it
//! only says so, so the workspace still builds in the cloud.

#[cfg(windows)]
mod boot;
#[cfg(windows)]
mod camera;
#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod channel;
#[cfg(windows)]
mod config;
#[cfg(windows)]
mod data;
#[cfg(windows)]
mod disk;
#[cfg(windows)]
mod doctor;
#[cfg(windows)]
mod focus;
#[cfg(windows)]
mod game;
#[cfg(windows)]
mod hands;
#[cfg(windows)]
mod input;
#[cfg(windows)]
mod ledger;
#[cfg(windows)]
mod mcp;
#[cfg(windows)]
mod measure;
#[cfg(windows)]
mod mem;
#[cfg(windows)]
mod notice;
#[cfg(windows)]
mod photo;
#[cfg(windows)]
mod plan;
#[cfg(windows)]
mod profile;
#[cfg(windows)]
mod proxy;
#[cfg(windows)]
mod relocate;
#[cfg(windows)]
mod save;
#[cfg(windows)]
mod scene;
#[cfg(windows)]
mod session;
#[cfg(windows)]
mod setup;
#[cfg(windows)]
mod w3;
#[cfg(windows)]
mod win;

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["mcp"] => proxy::run(),
        ["serve"] => mcp::serve(false),
        ["serve", "--stay"] => mcp::serve(true),
        ["serve", "--spawn"] => proxy::spawn_serve(),
        ["ledger", ..] => std::process::exit(ledger::cli(&args[1..])),
        ["kb", ..] => std::process::exit(ledger::cli_kb(&args[1..])),
        ["re", ..] => std::process::exit(ledger::cli_re(&args[1..])),
        ["setup", ..] => std::process::exit(setup::cli(&args[1..])),
        ["doctor"] => std::process::exit(doctor::cli()),
        ["recover"] => std::process::exit(session::cli_recover()),
        ["--version" | "-V"] => println!(
            "quen {} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("QUEN_COMMIT")
        ),
        _ => {
            eprintln!(
                "usage: quen mcp      the stdio MCP server: a proxy to quen serve, started when needed"
            );
            eprintln!("       quen serve    the daemon that owns the game session");
            eprintln!(
                "       quen serve --stay   the same, started by hand, until Ctrl+C (for Codex CLI: docs/clients.md)"
            );
            eprintln!("       quen ledger   add | about | migrate: see quen ledger --help");
            eprintln!(
                "       quen kb       scan | claims | export: the RE knowledge base's index (quen kb --help)"
            );
            eprintln!("       quen re       import: an REA evidence bundle (quen re --help)");
            eprintln!(
                "       quen setup    quen's paths; KEY=PATH ... --user-ok writes them (quen_setup)"
            );
            eprintln!(
                "       quen doctor   what quen depends on: found, missing or the wrong version"
            );
            eprintln!(
                "       quen recover  replays a journal a crash or power loss left (session_recover)"
            );
            eprintln!("       quen --version   the version and the git commit it was built from");
            std::process::exit(2);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("quen runs on Windows only");
    std::process::exit(2);
}
