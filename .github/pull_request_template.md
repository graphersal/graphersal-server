## What and why

<!-- What the change does and the problem it solves; link the issue (Closes #...). -->

## Checklist

- [ ] `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked` pass
- [ ] `node --test crates/graphersal-server/ui/tests/*.test.mjs` passes (when `ui/` changed)
- [ ] Tests added or updated for the change
- [ ] A user-visible change updates the book (`book/src/`, `mdbook build book` clean) and the release notes of the next version
- [ ] A new feature has a step in `scripts/smoke.sh` and `scripts/smoke.ps1`
- [ ] No tokens, real data or private paths in code, tests or docs
- [ ] I have signed the CLA (the CLA check comments on your first pull request; see CONTRIBUTING.md)
