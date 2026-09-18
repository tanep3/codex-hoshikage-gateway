# Operations and recovery

[日本語](operations.ja.md) · [Installation](installation.md) · [User manual](user-manual.md)

This guide is for the person who installed the Gateway. Most everyday actions, including stopping work and recovering a blocked conversation, happen in Discord. Start with `/status`, `/cancel`, `/stop`, and `/recover` in the [user manual](user-manual.md).

## Check the service

```bash
systemctl --user status codex-hoshikage-gateway.service
journalctl --user -u codex-hoshikage-gateway.service -n 50 --no-pager
```

If it is not `active (running)`, check the **binary path** and `direct run` in `ExecStart` first. After fixing a setting, restart the service. Check `/status` in Discord before restarting because a running task will be interrupted.

```bash
systemctl --user daemon-reload
systemctl --user restart codex-hoshikage-gateway.service
```

If the Bot is online but silent, check your Discord server and user IDs, Bot permissions, Message Content Intent, and `response_mode`. In `mention` mode, test with a mention. Check sign-in in the Gateway's dedicated Codex home:

```bash
CODEX_HOME="$HOME/.config/codex-hoshikage-gateway/codex-home" codex login status
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" direct check
```

`direct check` does not test outside connections. Editing the file does not change a running service; restart after changes. Configure MCP tools in the Gateway's dedicated `codex-home`.

## Recover a blocked conversation

The usual path is for the authorized person to run `/recover` **in that Discord conversation**. The screen explains that the old task is recorded but not rerun, unsent queued tasks are cancelled, and Codex starts a fresh context. Nothing is released until the person presses the confirmation button. Work files and Discord history remain.

`/recover` does not release work that is still running. Check `/status` and use `/stop` if needed. `/resume` only unpauses a queue; it does not resolve an uncertain task.

Only when Discord recovery keeps failing should an operator stop the service and list held tasks:

```bash
systemctl --user stop codex-hoshikage-gateway.service
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" direct holds
```

`direct abandon` is a last resort. Check for a remaining Codex process and inspect the affected workspace first. Use the listed `request_id` and `generation`, and choose a **new, separate backup destination** for `--backup-to`. This releases the hold without marking the old task successful; the next task uses a fresh context. Do not use it if the old work may still be running.

```bash
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" direct abandon --request-id REQUEST_ID --generation GENERATION --reason "Investigation and reason for release" --backup-to /safe/place/new-backup-name --accept-risk
systemctl --user start codex-hoshikage-gateway.service
```

## Configuration and backups

Configuration lives at `~/.config/codex-hoshikage-gateway/config.toml`, the Bot token in a separate `discord-token` file, and Codex sign-in/MCP settings in the dedicated `codex-home`. `state_dir` contains conversation and delivery records. Back up the work files under `workspace_root` too; saving only one side does not preserve both conversations and files.

Stop the Gateway before copying **configuration, token, codex-home, state_dir, and workspace_root** into a backup accessible only to its owner. These files contain secrets: never publish them or commit them to Git. Restoring old execution state without checking can make already-sent work look unsent. Review the operating state before a restore, and do not hand-edit SQLite or identity files.

Changing `workspace_root` does **not move an existing conversation's files**. New conversations use the new parent folder. Do not point an existing database at a different `state_dir`, Discord server, or authorized user. Do not rerun `direct init` as a recovery shortcut.
