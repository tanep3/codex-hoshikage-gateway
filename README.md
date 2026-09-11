# Codex Hoshikage Gateway

[日本語](README.ja.md)

**Talk to Codex from Discord, ask it to do work, and collect the results.**

This Discord Bot works with [Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy). Ask questions, request work, choose a model, approve actions, or stop a task without opening a terminal.

Keep conversations in text channels or individual forum posts. You do not need to register a working directory: the Proxy manages conversation workspaces. Choose whether the Bot responds to every authorized post or only mentions. The initial release is for one authorized user in one Discord server.

API v2 lets the Proxy retain fixed versions of artifacts and final replies. The Gateway handles selection and Discord delivery. A delivery failure never triggers an automatic rerun of the AI task.

- [Installation](docs/installation.md): create the Discord server and Bot, configure credentials, and install.
- [User manual](docs/user-manual.md): conversations, models, stopping work, and files.
- [Implementation status (Japanese)](docs/implementation-status.ja.md): implemented paths and remaining work.

**Requires Proxy API v2.** See [verification status](docs/implementation-status.ja.md) for tested coverage.

Written in Rust. Install with `cargo install --path . --locked`; your existing Cargo installation destination is respected. The Bot interface currently uses Japanese.

[MIT License](LICENSE) — Copyright (c) 2026 Tane Channel Technology

See [Operations guide](docs/operations.md).
