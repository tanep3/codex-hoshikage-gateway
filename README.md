# Codex Hoshikage Gateway

[日本語](README.ja.md)

**Work with Codex from Discord, using your own server and project folders.**

Codex Hoshikage Gateway connects a Discord Bot to your running `codex-hoshikage-proxy`. Send a request from Discord on your computer or phone, follow the reply, respond to approval requests, and retrieve files from your project—all in the same conversation.

## What you can do

- Ask Codex to investigate code, make changes, or prepare a document without opening a terminal for each request.
- Keep work organized: one Discord text channel represents a project; each thread is a separate conversation with its own context.
- Guide work while it is running with additional instructions, or request a stop and pause queued requests.
- Choose the model for the next request, attach images or text files, and explicitly download an output file.
- Return to an existing conversation after a Gateway restart. Uncertain requests are held for inspection instead of automatically running again.

The first version is for **one authorized user in one configured Discord server**. You host the Gateway; the Proxy supplies Codex access and controls its execution permissions. The Gateway does not install or manage the Proxy. Requests and replies pass through Discord, so access to the project channels matters.

## A typical session

1. In your configured project channel, run `/new title:Investigate a bug`.
2. In the thread it creates, post: “Investigate why the tests fail and explain the cause before editing.”
3. Read the reply, continue the conversation, or use `/get path:output/report.txt` to retrieve a file you requested.

A normal message in the project channel does not start Codex. Work is accepted inside Gateway-created conversation threads.

## Get started

| Document | English | 日本語 |
| --- | --- | --- |
| Installation: server, Bot, credentials, configuration, startup | [Installation guide](docs/installation.md) | [導入手順書](docs/installation.ja.md) |
| Daily use: conversations, commands, attachments, recovery | [User manual](docs/user-manual.md) | [ユーザーマニュアル](docs/user-manual.ja.md) |

You need an Ubuntu host, a compatible running Proxy, a Discord account with permission to install a Bot, and Rust to build this version from source. The installation guide walks through Discord setup from the beginning.

## Current status

This is an **early development version (0.1.0)**. Automated tests using simulated Discord and Proxy services have passed; live Discord/Codex acceptance tests and long-running service verification remain. It is ready for controlled evaluation, not a verified production release. Bot command descriptions and replies currently use Japanese; the English documentation does not imply an English Bot interface.

With the current Proxy contract, lost answer text cannot be fetched again after it leaves Gateway memory. A completed Codex task can therefore have an incomplete Discord reply. The Gateway does not rerun the task to replace missing text. See the [user manual](docs/user-manual.md) for recovery behavior.

For contributors, the [implementation status](docs/implementation-status.ja.md), [requirements](docs/requirements.ja.md), and [system design](docs/system-design.ja.md) are available in Japanese.

## License

Copyright © 2026 **Tane Channel Technology**. Distributed under the [MIT License](LICENSE). Dependency licenses also apply; see [dependency information](packaging/dependencies.md).
