//! Writes REA's seed file for an exe from a KB `names.tsv`, refused unless the exe is the build
//! the table's `# build:` line names: `cargo run -p quen-core --example seeds -- <names.tsv> <exe>`. The file goes to
//! `seeds/<stamp>-<sha256 prefix>.tsv` beside names.tsv, the path `REA_GHIDRA_SEED_FILE` then names.
//! It writes the file itself rather than stdout, so a shell redirect can't re-encode it.

use std::path::Path;

use quen_core::build::Build;
use quen_core::seeds;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, names, exe] = &args[..] else {
        return Err("usage: seeds <names.tsv> <exe>".into());
    };
    let exe = Build::read_exe(Path::new(exe))?;
    let text = seeds::seed_file(&std::fs::read_to_string(names)?, &exe)?;
    let kb = Path::new(names).parent().unwrap_or(Path::new(""));
    let out = kb.join(seeds::path(&exe));
    std::fs::create_dir_all(out.parent().expect("a seed file's path has a folder"))?;
    std::fs::write(&out, &text)?;
    eprintln!(
        "{} seeds for {exe} -> {}",
        text.lines().count(),
        out.display()
    );
    Ok(())
}
