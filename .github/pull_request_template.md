## What and why

<!-- What this changes, and the issue it belongs to. -->

## How it was tested

<!-- The tests you added or ran. If you tried it in the game, what you did and what you saw. -->

## Checklist

- [ ] `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass
- [ ] The privacy hooks are on (`core.hooksPath .githooks`, your own deny-list), and nothing here identifies you or your PC
- [ ] It follows AGENTS.md: refuse before acting, new migrations only, synthetic test data, foreground tests `#[ignore]`d
- [ ] Docs updated where behaviour changes (facts only)
- [ ] If `w3/` changed: `w3/asi/build.bat test` and the offline script compile pass; the new sha256s are below
