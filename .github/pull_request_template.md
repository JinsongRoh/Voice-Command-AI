## What this changes

<!-- What does this pull request do, and why? Link the issue it addresses (for example "Closes #123"). -->

## How it was tested

<!-- Commands you ran, platforms and devices you tried, agents you used. -->

## Checklist

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass.
- [ ] If `apps/` changed: the same checks with `--manifest-path apps/Cargo.toml` pass.
- [ ] If `apps/ui` changed: `pnpm check` passes (lint, Prettier, typecheck, tests, build).
- [ ] New UI text is added to all four locale files in `apps/ui/src/i18n/` (en, ko, ja, es).
- [ ] Documentation and `CHANGELOG.md` (unreleased section) are updated for user-visible changes.
- [ ] Commit messages are in English and use conventional prefixes (`feat:`, `fix:`, `docs:`, ...).
- [ ] No secrets, tokens, personal paths, machine names, internal hostnames or IP addresses are included in code, tests, logs or screenshots.
- [ ] I agree that my contribution is licensed under the MIT License of this project.

<!-- Security fixes: do not describe an unfixed vulnerability here. Report it privately first; see SECURITY.md. -->
