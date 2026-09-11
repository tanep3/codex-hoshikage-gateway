# Installation

[日本語](installation.ja.md) · [User manual](user-manual.md)

**Requires Proxy API v2.** Prepare the Proxy first, then follow the steps below.

## 1. Prepare the Proxy

An API v2 version of [Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy) is required. Configure Codex authentication, managed workspaces, storage, and permissions on the Proxy. The Gateway needs its URL, API key, and model; it does not need cwd or Project registration. Artifacts use HTTP on both shared and separate hosts. Use HTTPS for a remote host.

## 2. Create a Discord server and project channel

In Discord's desktop/browser client, use the **+** in the server sidebar, choose to create your own server, enter a name, and finish creation. A personal server is sufficient; Community features are unnecessary. See Discord's [server creation instructions](https://support.discord.com/hc/en-us/articles/204849977-How-do-I-create-a-server).

Create a regular **text channel**, or use a **forum** to keep separate conversations in individual posts. To create a forum, enable Community on your Discord server, press the channel category’s plus button, and choose Forum. See the [official Forum Channels FAQ](https://support.discord.com/hc/en-us/articles/6208479917079-Forum-Channels-FAQ).

Use a dedicated server or restrict the project channel to yourself and the Bot. Gateway's user allowlist controls who can operate it; it does not hide messages from other people who can view the channel. Normal conversations stay in the channel. Optional `/new` creates an additional public thread; neither is private to you alone.

## 3. Create the Bot and enable message access

Open the [Discord Developer Portal](https://discord.com/developers/applications), create a dedicated application, and name it. New applications already include a Bot user. On its **Bot** page, generate a token using **Reset Token** and keep it for step 6. Discord documents this in its [application setup guide](https://docs.discord.com/developers/quick-start/getting-started).

The Bot token is the Gateway's Discord credential. It is different from the Application ID, Public Key, Client Secret, and your personal Discord password. Use a dedicated application because the Gateway registers/replaces that application's commands in the configured server on startup.

On the Bot page, enable **Message Content Intent** under privileged intents and save. Ordinary message text and attachments require this access. Presence and member-list intents are not needed by this implementation. If the Portal requires approval, complete Discord's process before starting. See the [official intent rules](https://docs.discord.com/developers/events/gateway#privileged-intents).

Leave the application's Interactions Endpoint URL unset for this Gateway. You do not need a webhook URL or the HTTP-server setup shown in some generic Bot tutorials.

## 4. Install the Bot in your server

In the application's installation settings, enable **Guild Install** for server installation; this Gateway does not use User Install. Select the Discord-provided install link and configure the server installation scopes `bot` and `applications.commands`. Save the settings. See Discord's [application installation documentation](https://docs.discord.com/developers/resources/application).

Give the Bot these permissions in the project channel and its threads:

| Permission | Purpose in this Gateway |
| --- | --- |
| View Channels | Find the configured project and conversation |
| Send Messages | Send Bot messages |
| Send Messages in Threads | Reply inside conversations |
| Create Public Threads | Create conversations with `/new` |
| Read Message History | Read and verify requests, including after restart |
| Attach Files | Return explicitly requested output files |

These names correspond to Discord's [permission definitions](https://docs.discord.com/developers/topics/permissions). Administrator and Manage Channels are not required by this implementation. Check category/channel overrides as well as the Bot role. Your own account also needs permission to view/post in the channel and use application commands.

Open the install link, choose your server, review the requested permissions, and authorize installation. The installing account needs server-management permission. The Bot should appear in the server member list; it can remain offline until the Gateway runs. See Discord's [Bot authorization flow](https://docs.discord.com/developers/topics/oauth2#bot-authorization-flow).

## 5. Copy your server and user IDs

Enable Developer Mode in Discord's user settings under Advanced. Copy your own user ID from your user/profile context menu and the server ID from the server icon's context menu. Channel IDs are obtained automatically from the conversation location. See Discord's [ID lookup guide](https://support.discord.com/hc/en-us/articles/206346498-Where-can-I-find-my-User-Server-Message-ID).

| Value | Gateway setting |
| --- | --- |
| Server ID | `discord.guild_id` |
| Your personal user ID, not the Bot's | `discord.allowed_user_id` |

Keep the IDs as quoted strings in TOML. Names and invite links cannot replace them.

## 6. Build and store credentials

From your chosen source directory:

```sh
git clone https://github.com/tanep3/codex-hoshikage-gateway.git
cd codex-hoshikage-gateway
cargo install --path . --locked
install -d -m 700 "$HOME/.config/codex-hoshikage-gateway"
cp config/config.example.toml "$HOME/.config/codex-hoshikage-gateway/config.toml"
chmod 600 "$HOME/.config/codex-hoshikage-gateway/config.toml"
```

Install with `cargo install --path .`. The example adds `--locked` to use the dependency versions in Cargo.lock. The usual executable path is `~/.cargo/bin/codex-hoshikage-gateway`. If `~/.cargo/bin` is on your PATH, check it with `codex-hoshikage-gateway --version`. If you customize `CARGO_HOME` or the Cargo install directory, adjust the paths below and the service’s `ExecStart` to match.

Run these steps as the user who will run the Gateway, not root.

Enter the Bot token at a hidden prompt. Paste only the token, without a `Bot ` prefix. The token itself will not become part of your shell command history:

```bash
read -r -s -p 'Discord Bot token: ' gateway_bot_token
printf '\n'
(umask 077; printf '%s\n' "$gateway_bot_token" > "$HOME/.config/codex-hoshikage-gateway/discord-token")
unset gateway_bot_token
chmod 600 "$HOME/.config/codex-hoshikage-gateway/discord-token"
```

Next, save the **Proxy API key** in its own file. This is the key for connecting to your Proxy, not an OpenAI API key. Configure a non-empty key on the Proxy even for this loopback setup, because the Gateway requires a key file:

```bash
read -r -s -p 'Proxy API key: ' gateway_proxy_key
printf '\n'
(umask 077; printf '%s\n' "$gateway_proxy_key" > "$HOME/.config/codex-hoshikage-gateway/proxy-key")
unset gateway_proxy_key
chmod 600 "$HOME/.config/codex-hoshikage-gateway/proxy-key"
```

Credential files must be ordinary files owned by the service user, with no group/other access; symlinks are rejected. Keep them out of Git, Discord messages, and screenshots. To replace a Bot token, stop the Gateway, reset the token in the Portal, update this file using the hidden prompt, and restart. The old token no longer authenticates after a reset.

## 7. Configure the Gateway

Copy the [sample](../config/config.example.toml) to config.toml. Set the Discord Guild/user IDs and token_file, the Proxy URL and api_key_file, and default_model. Use `proxy.contract_version = "2.0"`. Choose `response_mode = "all"` or `"mention"`.

Start with the sample's positive resource defaults. Use the absolute equivalent of `~/.config/codex-hoshikage-gateway/temp` for the delivery cache. This is not the Codex working directory. Adapt state_dir and socket_path to your host.

## 8. Start in your terminal

```sh
cargo install --path . --locked
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" check
# First installation only. Never reinitialize an existing database.
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" init
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" run
```

Your configured Cargo installation destination is respected. Test conversations, model changes, stopping, and saved output retrieval. The foreground process keeps running until you press Ctrl+C. No startup shell script or systemd setup is needed for this step.

## 9. Optional: keep it running in the background

After the foreground checks, press Ctrl+C and wait for that Gateway process to exit. From the repository directory:

```sh
install -d "$HOME/.config/systemd/user"
install -m 644 packaging/codex-hoshikage-gateway.service "$HOME/.config/systemd/user/codex-hoshikage-gateway.service"
systemctl --user daemon-reload
systemctl --user enable --now codex-hoshikage-gateway.service
systemctl --user status codex-hoshikage-gateway.service
journalctl --user -u codex-hoshikage-gateway.service -n 50 --no-pager
```

systemd starts the binary directly; no wrapper shell script is needed. The supplied service uses the binary and configuration locations above. If you chose different locations, edit `ExecStart` before enabling it. Run only one instance for a state directory. To keep a user service running without a login session, the host administrator may need to enable lingering for the service account, for example `loginctl enable-linger USERNAME`.

Stop it with `systemctl --user stop codex-hoshikage-gateway.service`; restart it with `systemctl --user restart codex-hoshikage-gateway.service`. Stopping the Gateway service is not a substitute for confirming that a Codex task has stopped. Use `/stop` and inspect the result before planned maintenance.

## 10. Looking after your Gateway

Keep your configuration, state, and `gateway-instance.json` marker. Initial identity settings cannot be changed simply by pointing an existing database at a different user, server, Proxy URL, or state directory. Do not repair startup problems by editing SQLite or removing identity files.

While the Gateway is running, the local service user can inspect it and create a consistent backup:

```sh
"$HOME/.cargo/bin/codex-hoshikage-gateway" --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin status
"$HOME/.cargo/bin/codex-hoshikage-gateway" --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin backup --to /absolute/path/new-backup-bundle
```

Replace the backup path with a new, private destination. Do not copy only a live SQLite file. This bundle backs up Gateway state, not project files, Discord message history, credentials, or lost replies. Preserve configuration and identity files separately with appropriate access controls. Restoring a backup deliberately quarantines old requests instead of resending them; see the [technical recovery design](system-design.ja.md) before performing recovery.

| Symptom | Check |
| --- | --- |
| `check` fails | Check ID/path/model placeholders and any limits you changed; use absolute paths and owner-only credential files |
| Bot stays offline | Service logs, token validity, outbound connectivity, intent configuration |
| Slash commands are missing | Correct application/server installation, command scope, your command permissions, successful Gateway startup |
| Ordinary posts do nothing | Allowed user, registered channel, response mode and Bot mention, Message Content Intent |
| Bot cannot create/reply in threads | Channel/category overrides and all six permissions in step 4 |
| Proxy is not ready | Running Proxy, API key, required capabilities and matching contract; `check` alone cannot establish this |
| File rejected | Supported input type, configured limits, actual file content, and Discord's upload limit |
| Request is `UNKNOWN` | Inspect status and reconcile with the Proxy; do not resubmit it to force progress |

All set? Head to the [user manual](user-manual.md) for your first real conversation and a handy command reference.

Discord-specific setup was checked against the linked official documentation on **2026-09-11**. Portal labels may vary by language or later updates.

See [Operations guide](operations.md).
