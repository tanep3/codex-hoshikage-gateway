# Codex Hoshikage Gateway

[日本語](README.ja.md)

**Chat with Codex in Discord, even when you are away from your computer.** Ask it to investigate, create files or images, and continue the conversation in the same place. Approvals, model choices, stopping a task, and recovering a stalled conversation are available in Discord.

The Gateway runs on your own Linux computer and starts its own Codex App Server. You do **not** need a separate Proxy service, Proxy URL, or Proxy API key. Each text channel or forum post keeps its own conversation and working folder automatically. One Discord server and one authorized user are supported at present. People who can see your channel can also see its conversation, so choose a private channel for private work.

The Gateway can answer every authorized message or only messages that mention the Bot. Generated images are attached to the conversation; `/get` retrieves other files. When a task needs permission, you decide in Discord. The Gateway does not silently rerun an uncertain task.

- [Install and configure](docs/installation.md) · [日本語](docs/installation.ja.md)
- [User manual](docs/user-manual.md) · [日本語](docs/user-manual.ja.md)
- [Operations and recovery](docs/operations.md) · [日本語](docs/operations.ja.md)
- [Release notes](CHANGELOG.md) · [日本語](CHANGELOG.ja.md)

Built in Rust. Install the Gateway from this checkout with `cargo install --path . --locked`. [Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy) is a separate project for applications that need a shared HTTP API; this Gateway does not depend on it.

[MIT License](LICENSE) · Copyright © 2026 Tane Channel Technology
