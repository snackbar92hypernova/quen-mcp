# Contributing to quen

Thanks for helping. quen is in early access: this repository is private, and invited testers have read access. You contribute through a fork and a pull request. For anything bigger than a fix, open an issue first, so we agree on the approach before you build it.

[docs/intent.md](docs/intent.md) says what quen is for, the principles every change keeps (always restore, refuse before acting, the user decides, evidence over opinion, privacy) and what is out of scope. Read it before you propose a feature.

## Set up once

1. **Fork** the repository on GitHub and clone your fork.
2. **Turn on the privacy hooks:** `git config core.hooksPath .githooks`. They check every commit and commit message for personal data.
3. **Create your deny-list:** `%USERPROFILE%\.quen\deny.txt`.
   - One case-insensitive string per line, `#` for comments.
   - List what identifies you: your Windows user name, PC name, real name, email, Steam id, and anything else you'd never want in a public repository.
   - Without it the hooks refuse every commit. It stays on your PC: never commit it.
4. **Rust:** install rustup (<https://rustup.rs>) with the Microsoft C++ build tools. The repository pins its Rust version (`rust-toolchain.toml`). [INSTALL.md](INSTALL.md) §1 and §2 have the details.
5. **Only if you change `w3/`** (quen's in-game package):
   - you need MSVC and the Witcher Native Bridge SDK (`WNB_SDK`);
   - two Tracy files aren't in git; [w3/third_party/README.md](w3/third_party/README.md) says where to get them and checks their sha256.

## The rules

[AGENTS.md](AGENTS.md) has the full list. Coding agents working on quen read it too, so point yours at it. In short:

- **No personal data, ever:**
  - no user names, emails, machine names, home or install paths, save names, hardware details or Steam ids;
  - no logs or screenshots from a real machine.
  Use placeholders such as `%USERPROFILE%` and `%GAME_DIR%`, and synthetic test data.
- **Never in git:** build output, ledgers, captures, logs, files from or derived from the game, decompiled code, reverse-engineering notes. The one exception is `w3/asi/hooks.inc` (AGENTS.md says what it may hold).
- **Refuse before acting:** every tool checks first and changes nothing when it refuses. After its first change, a failure unwinds through the session journal.
- **Ledger migrations are never edited, only added:** a fix is a new `schema/NNNN_*.sql`, with its sha256 pin and its description in `schema/README.md`.
- **Small, scoped changes.** Tests use synthetic fixtures only. Tests that take the foreground stay `#[ignore]`d.
- **Docs state facts only:** what the code does or a test showed, nothing guessed.

## Before you open a pull request

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- If you changed `w3/`: `w3/asi/build.bat test` and the offline script compile ([w3/README.md](w3/README.md)). Put the new sha256s in the pull request.
- Update the docs where behaviour changes.

## The pull request

- Open it from your fork into `main`, and fill in the template's checklist.
- **CI:** a privacy scan runs on every pull request, and the Rust checks run when code changes. GitHub Actions minutes are limited, so a pull request from a fork runs its checks only after the maintainer approves them.
- The maintainer reviews and merges. Testing in the game happens on the maintainer's PC too, through quen.
- Never attach game logs (`game.log`, `game-logs\`), your ledger, captures, saves, Nsight's `REPRO_INFO.xls`, or screenshots of the game's main menu (they show account names).

## Licence

quen is licensed under MIT OR Apache-2.0. Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in quen, as defined in the Apache-2.0 licence, is dual licensed as above, without any additional terms or conditions.

## Bugs and security

Report bugs with the issue template. For a security problem, see [SECURITY.md](SECURITY.md).
