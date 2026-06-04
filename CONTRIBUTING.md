# Contributing to OpenSnow

Thanks for your interest in contributing! OpenSnow is an open-source analytics
warehouse written in Rust. Contributions of all kinds are welcome — bug
reports, documentation, tests, and code.

## Getting started

Prerequisites:

- A recent stable Rust toolchain (the repository pins one via
  `rust-toolchain.toml`; `rustup` will install it automatically).
- `python3` for the demo and smoke scripts.

Build and test:

```bash
cargo build --workspace
cargo test --workspace
```

Before opening a pull request, please make sure the following pass locally —
they are the same gates CI enforces:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Pull requests

- Keep changes focused; one logical change per pull request.
- Add or update tests for behavior changes.
- Update documentation (`README.md`, `ARCHITECTURE.md`, `docs/`) when you
  change user-facing behavior.
- Write clear commit messages describing the *why*, not just the *what*.

## Reporting bugs

Open a GitHub issue with reproduction steps, the commit hash, and what you
expected versus what happened. For **security** issues, do not open a public
issue — follow [SECURITY.md](SECURITY.md) instead.

## License

By contributing, you agree that your contributions will be licensed under the
[Apache License 2.0](LICENSE), the same license that covers the project.

## Code of conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). By
participating, you are expected to uphold it.
