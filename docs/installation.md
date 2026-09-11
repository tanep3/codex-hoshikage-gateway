# Installation guide

[日本語](installation.ja.md) · [Product overview](../README.md) · [User manual](user-manual.md)

Let's connect your Discord to Codex. We'll prepare the Proxy, add a Bot to your server, and start the Gateway together. Run the commands below in Bash on your Ubuntu host. Once setup is done, the [user manual](user-manual.md) shows you how to use it.

This is a source installation of development version 0.1.0. Start with a disposable project and complete the checks below before using important workspaces. Live-service acceptance and release packaging verification are still pending.

## 1. Start with the Proxy and your host

**This Gateway needs [Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy).** It provides the connection to Codex; this Gateway provides the Discord interface. They run as separate services.

If the Proxy is not installed yet, follow its [installation guide](https://github.com/tanep3/codex-hoshikage-proxy/blob/main/docs/installation.md) first. That guide covers Codex setup, authentication, Proxy configuration, and starting its service. Then come back here with your Proxy URL, API key, workspace, and model ID. If you already have it running, check the requirements below and carry on.

Here is what you will need:

- An Ubuntu user account that will own the Gateway files and run the service.
- Git, a C build toolchain, and Rust/Cargo **1.98 or later**. The recorded development checks used Rust 1.98.1. Follow the [official Rust installation instructions](https://www.rust-lang.org/tools/install) if Rust is not installed.
- A running [codex-hoshikage-proxy](https://github.com/tanep3/codex-hoshikage-proxy) supporting [Control API contract **1.0**](https://github.com/tanep3/codex-hoshikage-proxy/blob/main/docs/control-api.ja.md), with its required Gateway capabilities enabled. Have its base URL, API key, allowed project folder, and a usable provider-qualified model ID ready. A generic Responses API endpoint alone is insufficient.
- Outbound connectivity to Discord and connectivity to the Proxy from the Gateway host.
- A Discord account that owns the target server or can manage/install apps on it.

For the two services to work together, they need to use the same project folder: use the same existing workspace on the host for this initial setup. Configure Codex access, workspace permissions, approval policy, and network access on the Proxy beforehand. Gateway configuration cannot grant permissions that the Proxy denies.

One detail to check before moving on: the sample Proxy URL is `http://127.0.0.1:4040`; verify your actual endpoint. Loopback refers to the Gateway's own host. This Gateway connects to Discord through an outbound WebSocket and REST requests; it does not require a public web server, an inbound port, or an Interactions Endpoint URL.

## 2. Create a Discord server and project channel

In Discord's desktop/browser client, use the **+** in the server sidebar, choose to create your own server, enter a name, and finish creation. A personal server is sufficient; Community features are unnecessary. See Discord's [server creation instructions](https://support.discord.com/hc/en-us/articles/204849977-How-do-I-create-a-server).

Create a regular **text channel**, such as `project-demo`, for your first project. Each project needs its own channel. Forum and voice channels are not the project-channel type used by this Gateway. Discord explains channel types and access controls in its [server setup guide](https://support.discord.com/hc/en-us/articles/33023827550359-Discord-Server-Setup-Guide).

Use a dedicated server or restrict the project channel to yourself and the Bot. Gateway's user allowlist controls who can operate it; it does not hide messages from other people who can view the channel. `/new` creates a public thread within that channel, not a private conversation visible only to you.

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

## 5. Copy the three Discord IDs

Enable Developer Mode in Discord's user settings under Advanced. Copy your own user ID from your user/profile context menu, the server ID from the server icon's context menu, and the project channel ID from the channel's context menu. See Discord's [ID lookup guide](https://support.discord.com/hc/en-us/articles/206346498-Where-can-I-find-my-User-Server-Message-ID).

| Value | Gateway setting |
| --- | --- |
| Server ID | `discord.guild_id` |
| Your personal user ID, not the Bot's | `discord.allowed_user_id` |
| Project text channel ID, not a thread ID | `projects.channel_id` |

Keep the IDs as quoted strings in TOML. Names and invite links cannot replace them.

## 6. Build and store credentials

From your chosen source directory:

```sh
git clone https://github.com/tanep3/codex-hoshikage-gateway.git
cd codex-hoshikage-gateway
cargo build --release --locked
install -d "$HOME/.local/bin"
install -m 755 target/release/gateway "$HOME/.local/bin/gateway"
install -d -m 700 "$HOME/.config/codex-hoshikage-gateway"
cp config/config.example.toml "$HOME/.config/codex-hoshikage-gateway/config.toml"
chmod 600 "$HOME/.config/codex-hoshikage-gateway/config.toml"
```

The copy is for a **new installation**. Preserve your existing configuration when upgrading. Run these steps as the service user, not root.

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

## 7. Tell the Gateway about your setup

Edit `$HOME/.config/codex-hoshikage-gateway/config.toml` using a text editor. Start from the [complete configuration example](../config/config.example.toml).

**Replace every `/home/tane/...` example path with your own absolute path.** TOML values do not expand `~` or `$HOME`. Run `printf '%s\n' "$HOME"` and `id -u` to find your home directory and numeric user ID.

| Setting | What to enter |
| --- | --- |
| `discord.guild_id`, `allowed_user_id` | IDs from step 5 |
| `discord.token_file` | Absolute path to `discord-token` |
| `proxy.base_url` | Your running Proxy's base URL |
| `proxy.api_key_file` | Absolute path to `proxy-key` |
| `proxy.contract_version` | `"1.0"` for the current implementation |
| `storage.state_dir` | Dedicated persistent directory for Gateway state |
| `storage.temp_dir` | Dedicated temporary directory; do not share it with another application |
| `storage.socket_path` | For example `/run/user/YOUR_UID/codex-hoshikage-gateway/admin.sock`, replacing `YOUR_UID` |
| `projects.id` | A stable, unique UUID; Ubuntu can generate one with `cat /proc/sys/kernel/random/uuid` |
| `projects.name` | A recognizable project name |
| `projects.channel_id` | The project's text channel ID |
| `projects.cwd` | Existing absolute project folder allowed by the Proxy |
| `projects.default_model` | An actual available provider-qualified model ID; replace the sample placeholder |
| `projects.lifecycle` | `"ACTIVE"` |

For the first installation, create the dedicated storage directories with mode `0700` under your own account. With the sample layout adapted to your home:

```sh
install -d -m 700 "$HOME/.local/state/codex-hoshikage-gateway"
install -d -m 700 "$HOME/.local/state/codex-hoshikage-gateway/temp"
```

Add another `[[projects]]` block for each additional project. IDs and channels must be unique. Workspace folders must not resolve to the same location or contain one another. Keep the project ID/channel/folder mapping stable after initialization.

### Choose resource limits

The sample's zero values mean **required, not configured**; they do not mean unlimited or disabled. Set positive integers according to your host's memory/disk budget, actual files, and the Proxy/Discord limits. This guide deliberately does not assign unmeasured production limits.

| Limit | Unit and meaning |
| --- | --- |
| `attachments` | Maximum files in one request |
| `attachment_bytes` | Maximum bytes in one input attachment |
| `input_bytes` | Maximum combined input size; encoded image input must also fit |
| `text_bytes` | Maximum UTF-8 bytes in message text or an individual text attachment |
| `image_pixels` | Maximum decoded pixels per image |
| `artifact_bytes` | Maximum bytes in a file returned by `/get` |
| `temp_bytes` | Shared temporary-file capacity in bytes |
| `output_bytes` | Maximum retained reply bytes per request |
| `output_total_bytes` | Total retained reply budget in bytes |
| `delivery_retention_secs` | How long reply data may remain in memory for delivery, in seconds |
| `queue_conversation` | Waiting-request limit per conversation; sample: 5 |
| `queue_global` | Waiting-request limit overall; sample: 20 |
| `validation_secs` | Input-validation reservation timeout in seconds; sample: 120 |

Use byte counts, not strings such as `"10MB"`. Allow for image encoding overhead and two active requests. `temp_bytes` must be at least both `input_bytes` and `artifact_bytes`; `output_total_bytes` must be at least `output_bytes`. The global queue limit must be at least the per-conversation limit. Two concurrent executions is a fixed limit in this version. Discord's own file limit still applies even when the configured limit is larger.

## 8. Start it up and say hello

```sh
"$HOME/.local/bin/gateway" --config "$HOME/.config/codex-hoshikage-gateway/config.toml" check
"$HOME/.local/bin/gateway" --config "$HOME/.config/codex-hoshikage-gateway/config.toml" init
"$HOME/.local/bin/gateway" --config "$HOME/.config/codex-hoshikage-gateway/config.toml" run
```

`--config` must precede the subcommand. `check` validates local configuration and credential files; it does **not** test Discord or Proxy connectivity. `init` runs once for a new installation and creates the database and adjacent configuration-directory `gateway-instance.json` identity marker. `run` requires that initialized state.

In Discord, sign in as the allowed user, open the registered project channel, and run `/new title:First check`. Inside the new thread:

1. Use `/status` to inspect readiness and the model.
2. Send a small request such as “Reply with a short greeting. Do not change files.” Confirm receipt and the answer.
3. Send a follow-up to check conversation continuity.
4. In a disposable workspace, verify attachment input, `/get`, `/stop` followed by `/resume`, model selection, and approval/steer when applicable. A stop acknowledgement alone is not confirmation that execution has stopped.

If readiness fails, fix the Proxy connection, credentials, model, or capability mismatch before sending work. Do not repeatedly initialize or delete the state directory to bypass errors.

## 9. Keep it running in the background

After the foreground checks, press Ctrl+C and wait for that Gateway process to exit. From the repository directory:

```sh
install -d "$HOME/.config/systemd/user"
install -m 644 packaging/codex-hoshikage-gateway.service "$HOME/.config/systemd/user/codex-hoshikage-gateway.service"
systemctl --user daemon-reload
systemctl --user enable --now codex-hoshikage-gateway.service
systemctl --user status codex-hoshikage-gateway.service
journalctl --user -u codex-hoshikage-gateway.service -n 50 --no-pager
```

The supplied service uses the binary and configuration locations above. If you chose different locations, edit `ExecStart` before enabling it. Run only one instance for a state directory. To keep a user service running without a login session, the host administrator may need to enable lingering for the service account, for example `loginctl enable-linger USERNAME`.

Stop it with `systemctl --user stop codex-hoshikage-gateway.service`; restart it with `systemctl --user restart codex-hoshikage-gateway.service`. Stopping the Gateway service is not a substitute for confirming that a Codex task has stopped. Use `/stop` and inspect the result before planned maintenance.

## 10. Looking after your Gateway

Keep your configuration, state, and `gateway-instance.json` marker. Initial identity settings cannot be changed simply by pointing an existing database at a different user, server, Proxy URL, or state directory. Do not repair startup problems by editing SQLite or removing identity files.

While the Gateway is running, the local service user can inspect it and create a consistent backup:

```sh
"$HOME/.local/bin/gateway" --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin status
"$HOME/.local/bin/gateway" --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin backup --to /absolute/path/new-backup-bundle
```

Replace the backup path with a new, private destination. Do not copy only a live SQLite file. This bundle backs up Gateway state, not project files, Discord message history, credentials, or lost replies. Preserve configuration and identity files separately with appropriate access controls. Restoring a backup deliberately quarantines old requests instead of resending them; see the [technical recovery design](system-design.ja.md) before performing recovery.

| Symptom | Check |
| --- | --- |
| `check` fails | Replace all placeholders/zero limits; use absolute paths and owner-only credential files |
| Bot stays offline | Service logs, token validity, outbound connectivity, intent configuration |
| Slash commands are missing | Correct application/server installation, command scope, your command permissions, successful Gateway startup |
| Ordinary posts do nothing | Allowed user, registered thread created by `/new`, Message Content Intent |
| Bot cannot create/reply in threads | Channel/category overrides and all six permissions in step 4 |
| Proxy is not ready | Running Proxy, API key, required capabilities and matching contract; `check` alone cannot establish this |
| File rejected | Supported input type, configured limits, actual file content, and Discord's upload limit |
| Request is `UNKNOWN` | Inspect status and reconcile with the Proxy; do not resubmit it to force progress |

All set? Head to the [user manual](user-manual.md) for your first real conversation and a handy command reference.

Discord-specific setup was checked against the linked official documentation on **2026-09-11**. Portal labels may vary by language or later updates.
