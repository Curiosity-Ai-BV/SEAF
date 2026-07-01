# Repository Guidelines

## Project Structure & Module Organization

SEAF is a Rust workspace with a small TypeScript SDK. Rust crates live in `crates/`: `seaf-core` holds shared models and validation, `seaf-cli` exposes the `seaf` binary, `seaf-loop` implements the agent loop and policy gate, `seaf-models` contains model providers, and `seaf-local-runtime` contains runtime primitives. The TypeScript package is `packages/seaf-sdk-js`, with source in `src/` and Vitest tests in `test/`. Public schemas live in `specs/`, runnable examples in `examples/`, fixtures in `fixtures/`, and design/security notes in `docs/`.

## Build, Test, and Development Commands

- `pnpm install --frozen-lockfile`: install pinned Node tooling.
- `cargo fmt --all -- --check`: verify Rust formatting.
- `cargo clippy --all-targets --all-features -- -D warnings`: run Rust linting.
- `cargo test --workspace`: run all Rust crate tests.
- `pnpm format:check`: check Markdown, JSON, YAML, and TypeScript.
- `pnpm lint`: run Rust clippy plus package lint scripts.
- `pnpm typecheck && pnpm test && pnpm build`: verify the TypeScript SDK and build `dist/`.
- `cargo run -p seaf-cli -- ticket validate examples/local-loop/tickets/add-health-command.yaml`: smoke-test CLI validation against an example ticket.

## Coding Style & Naming Conventions

Use Rust 2021 conventions and keep code `rustfmt`-clean. Rust modules, files, functions, and tests use `snake_case`; types use `PascalCase`. TypeScript is strict ESM targeting ES2022; prefer explicit exported types, `camelCase` functions, and `PascalCase` interfaces/types. Let Prettier own `*.md`, `*.json`, `*.yaml`, `*.yml`, and `*.ts`. Keep changes surgical and match nearby patterns before adding helpers.

## Testing Guidelines

Place Rust integration tests under each crate's `tests/` directory and encode the reason behavior matters in the test name, e.g. `policy_gate_rejects_forbidden_paths_and_never_invokes_apply`. Put SDK tests in `packages/seaf-sdk-js/test/*.test.ts`. Add fixtures under `fixtures/` or `examples/` when validating schemas, policy gates, CLI flows, or model responses. Before a PR, run the focused test plus relevant workspace checks above.

## Commit & Pull Request Guidelines

Recent history uses short imperative subjects such as `Harden CI for local agent loop` and `Add AgentBench-lite benchmark`; follow that style and keep one logical change per commit. Pull requests should describe impact, list validation commands, link issues or roadmap items when relevant, and include CLI output or screenshots only when they clarify behavior.

## Security & Agent Loop Notes

Treat `.seaf/loops/current/` as restartable loop state and avoid overwriting it casually. Do not weaken policy gates, CI checks, schema fixtures, or forbidden-path coverage without calling that out explicitly in the PR.
