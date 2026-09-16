# Installation

[日本語](installation.ja.md) · [User manual](user-manual.md)

This guide gets you to one concrete result: **you can talk to your Discord Bot and receive an answer from Codex.** After that, you can add background startup and optional MCP permissions. MCP connects Codex to external tools such as browser tools; it is not required just to start a conversation.

For first-time setup, follow steps 1–8 in order. Step 9 adds background startup, step 10 helps with problems, and step 11 enables optional MCP features. For everyday use, see the [user manual](user-manual.md).

## Before you start

These instructions use Ubuntu/Linux and the account that will run the Gateway. Do not run the Gateway itself as root.

| Where you work | What you do there |
| --- | --- |
| Discord app/browser | Create a server, invite the Bot, and try a conversation |
| Discord Developer Portal | Create the Bot and issue its connection token |
| Terminal on the Gateway machine | Install, configure, and start the Gateway |
| Proxy machine | Configure Codex execution and optional MCP permissions |

The Gateway handles Discord. [Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy) runs Codex. They can run on one machine or separate machines, and they have separate configuration files.

You need a Discord account, permission to add a Bot to your server, an API v2-compatible Proxy, Git, Rust/Cargo, and Python 3.11 or newer. This source requires **Rust 1.98 or newer** to build. Python is used for the connection checks below.

Run these in the Linux terminal. Each should print a version:

```sh
git --version
rustc --version
cargo --version
python3 --version
```

Install missing commands before continuing. See the [official Rust installation instructions](https://www.rust-lang.org/tools/install) for Rust/Cargo. On Ubuntu, an administrator can install the other prerequisites with:

```sh
sudo apt update
sudo apt install git build-essential pkg-config libssl-dev python3 nano
```

Run the following command blocks in Bash. Copy the commands without adding a prompt character. `$HOME` expands to your home directory in shell commands. **Inside the Gateway TOML file, use full absolute paths, not `~` or `$HOME`.** In the `nano` editor, save with Ctrl+O then Enter, and exit with Ctrl+X.

## 1. Prepare the Proxy

An API v2 version of [Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy) is required. Configure Codex authentication, managed workspaces, storage, and permissions on the Proxy. The Gateway needs its URL, API key, and model; it does not need cwd or Project registration. Artifacts use HTTP on both shared and separate hosts. Use HTTPS for a remote host.

If the Proxy is not installed yet, complete its [installation guide](https://github.com/tanep3/codex-hoshikage-proxy/blob/main/docs/installation.md) first. Obtain its connection URL, API key, and available models from the administrator. The Proxy API key is different from the Discord Bot token.

## 2. Create a Discord server and project channel

In Discord's desktop/browser client, use the **+** in the server sidebar, choose to create your own server, enter a name, and finish creation. For a regular text channel, a personal server is sufficient; Community features are unnecessary. See Discord's [server creation instructions](https://support.discord.com/hc/en-us/articles/204849977-How-do-I-create-a-server).

Create a regular **text channel**, or use a **forum** to keep separate conversations in individual posts. To create a forum, enable Community on your Discord server, press the channel category’s plus button, and choose Forum. See the [official Forum Channels FAQ](https://support.discord.com/hc/en-us/articles/6208479917079-Forum-Channels-FAQ).

Use a dedicated server or restrict the project channel to yourself and the Bot. Gateway's user allowlist controls who can operate it; it does not hide messages from other people who can view the channel. Normal conversations stay in the channel. Optional `/new` creates an additional public thread; neither is private to you alone.

## 3. Create the Bot and enable message access

Open the [Discord Developer Portal](https://discord.com/developers/applications), create a dedicated application, and name it. New applications already include a Bot user. On its **Bot** page, generate a token using **Reset Token** and keep it for step 6. Discord documents this in its [application setup guide](https://docs.discord.com/developers/quick-start/getting-started).

The Bot token is a secret, like a password that lets the Gateway connect as this Bot. It is different from the Application ID, Public Key, Client Secret, and your personal Discord password. Use a dedicated application because the Gateway registers/replaces that application's commands in the configured server on startup.

An intent determines which information Discord sends to the Bot. On the Bot page, enable **Message Content Intent** under privileged intents and save. Ordinary message text and attachments require this access. Presence and member-list intents are not needed by this implementation. If the Portal requires approval, complete Discord's process before starting. See the [official intent rules](https://docs.discord.com/developers/events/gateway#privileged-intents).

Leave the application's Interactions Endpoint URL unset for this Gateway. You do not need a webhook URL or the HTTP-server setup shown in some generic Bot tutorials.

## 4. Install the Bot in your server

Scopes describe why the app is being installed; permissions describe what the Bot can do. In the application's installation settings, enable **Guild Install** for server installation; this Gateway does not use User Install. Select the Discord-provided install link and configure the server installation scopes `bot` and `applications.commands`. Save the settings. See Discord's [application installation documentation](https://docs.discord.com/developers/resources/application).

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

These commands are for a **first installation**. For an existing installation, run `cargo install --path . --locked` from its source directory and skip copying the sample configuration. Do not overwrite your current config.

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

### Check the installed executable

```sh
command -v codex-hoshikage-gateway
codex-hoshikage-gateway --version
```

The first command should print its location; the second should print a version. Keep that path for systemd in step 9. If you get `command not found`, check the destination printed by `cargo install`, then add that directory to PATH or use the full executable path. A custom `~/bin` installation does not need to move to `~/.cargo/bin`.

Hidden key prompts show neither letters nor asterisks while you type. Paste the key and press Enter; there is no need to display its contents to check it.

## 7. Fill in your Gateway settings

Open the file copied in step 6. **For an existing installation, edit the current file instead of copying the sample over it.**

```sh
nano "$HOME/.config/codex-hoshikage-gateway/config.toml"
```

In another terminal, check your home directory and numeric user ID:

```sh
printf 'Home directory: %s\n' "$HOME"
id -u
```

For example, if these show `/home/hanako` and `1001`, replace every `/home/tane` in the sample with `/home/hanako`, and `/run/user/1000/` with `/run/user/1001/`. Use your actual values, not this example.

| Setting | What to enter |
| --- | --- |
| `default_model` | The first model to use. Retrieve the list below and copy one exact ID |
| `discord.guild_id` | Server ID copied in step 5 |
| `discord.allowed_user_id` | Your own user ID from step 5, not the Bot's ID |
| `discord.response_mode` | `"all"`: answer all posts from the allowed user. `"mention"`: answer only posts mentioning the Bot |
| `discord.token_file` | Absolute path to the `discord-token` file from step 6 |
| `proxy.base_url` | Proxy address. Use `"http://127.0.0.1:4040"` only if it runs on the same machine at port 4040. Do not append `/v1` or `/v2` |
| `proxy.api_key_file` | Absolute path to the `proxy-key` file from step 6 |
| `proxy.contract_version` | Leave as `"2.0"` |
| `storage.state_dir` | Gateway conversation/delivery state. Adjust the home directory in the sample |
| `storage.temp_dir` | Temporary files for Discord delivery, not the Codex working directory |
| `storage.socket_path` | Local connection point for administration commands. Adjust the numeric user ID |

For a Proxy on another machine, `127.0.0.1` would refer to the Gateway machine itself. Obtain an HTTPS URL from the Proxy administrator instead. The Gateway rejects unencrypted HTTP to another host, including on a LAN.

Keep the sample `[limits]` values initially. For example, the input defaults allow four attachments per request, up to 8 MiB each. You do not need to invent resource limits before starting. Zero is invalid; it does not mean unlimited.

### Check connectivity and choose a model

This command reads the Gateway configuration and key file, then asks the Proxy for model IDs. It does not print the key or send an AI request.

```sh
python3 - "$HOME/.config/codex-hoshikage-gateway/config.toml" <<'PY'
import json, pathlib, sys, tomllib, urllib.request, urllib.error
cfg = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())
key = pathlib.Path(cfg["proxy"]["api_key_file"]).read_text().strip()
url = cfg["proxy"]["base_url"].rstrip("/") + "/v1/models"
req = urllib.request.Request(url, headers={"Authorization": "Bearer " + key})
try:
    with urllib.request.urlopen(req, timeout=15) as response:
        data = json.load(response)
    for model in data["data"]:
        print(model["id"])
except urllib.error.HTTPError as error:
    sys.exit("Proxy HTTP error: " + str(error.code))
except urllib.error.URLError:
    sys.exit("Cannot connect to Proxy. Check its URL, service, and TLS.")
PY
```


One model ID per line means the query succeeded. Copy one available ID into `default_model = "..."` at the top of config.toml and save. An empty list means the Proxy model configuration needs attention. For HTTP 401/403, check the key and access permissions; for connection errors, check the URL, running Proxy, and TLS certificate.

## 8. Start in your terminal

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" check
```

Continue only after the local validation success message. This checks files, not network connectivity. Fix failures using steps 6–7 before proceeding.

Initialize only on first installation. Skip the following command for an existing installation.

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" init
```

A database-initialized message means success. If already initialized, do not delete it to get past the message. Now start the Gateway:

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" run
```

Your configured Cargo installation destination is respected. Test conversations, model changes, stopping, and saved output retrieval. The foreground process keeps running until you press Ctrl+C. No startup shell script or systemd setup is needed for this step.

Use a second terminal to inspect connectivity:

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin status
```

`connected: true` means Discord is connected; `proxy_ready: true` means the Proxy's basic required features are available, not that optional MCP permissions are enabled. Once the Bot is online, send “Hello” in Discord. Mention the Bot in `mention` mode. A reply confirms basic connectivity. Neither `/new` nor working-directory registration is required.

For commands, type `/` in Discord and choose this Bot's suggestion. Try `/status` and `/models`. If suggestions are missing, see step 10.

## 9. Optional: keep it running in the background

After the foreground checks, press Ctrl+C and wait for that Gateway process to exit. From the repository directory:

```sh
install -d "$HOME/.config/systemd/user"
install -m 644 packaging/codex-hoshikage-gateway.service "$HOME/.config/systemd/user/codex-hoshikage-gateway.service"
nano "$HOME/.config/systemd/user/codex-hoshikage-gateway.service"
```

Match the executable in `ExecStart=` to `command -v` from step 6. `%h` means the service user’s home. For a `~/bin` installation, use:

```ini
ExecStart=%h/bin/codex-hoshikage-gateway --config %h/.config/codex-hoshikage-gateway/config.toml run
```

Save and close the editor, then enable the service:

```sh
systemctl --user daemon-reload
systemctl --user enable --now codex-hoshikage-gateway.service
systemctl --user status codex-hoshikage-gateway.service
journalctl --user -u codex-hoshikage-gateway.service -n 50 --no-pager
```

An `active (running)` status means the service is running. Press `q` to leave the status screen. If it says `failed`, inspect the last errors from `journalctl` above. Check logs for keys or personal information before sharing them.

systemd starts the binary directly; no wrapper shell script is needed. The supplied service uses the binary and configuration locations above. If you chose different locations, edit `ExecStart` before enabling it. Run only one instance for a state directory. To keep a user service running without a login session, the host administrator may need to enable lingering for the service account, for example `loginctl enable-linger USERNAME`.

Stop it with `systemctl --user stop codex-hoshikage-gateway.service`; restart it with `systemctl --user restart codex-hoshikage-gateway.service`. Stopping the Gateway service is not a substitute for confirming that a Codex task has stopped. Use `/stop` and inspect the result before planned maintenance.

## 10. Looking after your Gateway

Keep your configuration, state, and `gateway-instance.json` marker. Initial identity settings cannot be changed simply by pointing an existing database at a different user, server, Proxy URL, or state directory. Do not repair startup problems by editing SQLite or removing identity files.

While the Gateway is running, the local service user can inspect it and create a consistent backup:

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin status
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin backup --to /absolute/path/new-backup-bundle
```

Replace the backup path with a new, private destination. Do not copy only a live SQLite file. This bundle backs up Gateway state, not project files, Discord message history, credentials, or lost replies. Preserve configuration and identity files separately with appropriate access controls. Restoring a backup deliberately quarantines old requests instead of resending them; see the [technical recovery design](system-design.ja.md) before performing recovery.

| Symptom | Check |
| --- | --- |
| `check` fails | Check ID/path/model placeholders and any limits you changed; use absolute paths and owner-only credential files |
| Bot stays offline | Service logs, token validity, outbound connectivity, intent configuration |
| Slash commands are missing | Correct application/server installation, command scope, your command permissions, successful Gateway startup |
| Ordinary posts do nothing | Allowed user, server ID, Bot access to the channel, response mode and Bot mention, Message Content Intent |
| Bot cannot create/reply in threads | Channel/category overrides and all six permissions in step 4 |
| Proxy is not ready | Running Proxy, API key, required capabilities and matching contract; `check` alone cannot establish this |
| File rejected | Supported input type, configured limits, actual file content, and Discord's upload limit |
| Request is `UNKNOWN` | Inspect status and reconcile with the Proxy; do not resubmit it to force progress |

All set? Head to the [user manual](user-manual.md) for your first real conversation and a handy command reference.

Discord-specific setup was checked against the linked official documentation on **2026-09-16**. Portal labels may vary by language or later updates.

See [Operations guide](operations.md).

## 11. Optional: inspect MCP operations and allow a tool for one task

Start here after basic conversations work. First verify the selected tools in a test environment. The Proxy [validation record (Japanese)](https://github.com/tanep3/codex-hoshikage-proxy/blob/main/docs/mcp-turn-approval-validation.ja.md) distinguishes completed checks from remaining work.

**Updating the Gateway and Proxy binaries does not turn this feature on.** The Proxy administrator must enable it and choose eligible tools. Discord users do not need to edit configuration, and the Gateway does not need a duplicate switch.

### Agree on what appears in the conversation

Compatible versions display search text and destination URLs alongside the operation description and permission buttons in the original Discord conversation. **Anyone who can read that conversation can see these targets.** Check invitations and channel permissions, even on a personal server. Explain this policy to the user and obtain agreement before enabling it.

Credentials, secret inputs, and arbitrary executable code use private supplemental screens. Unknown secrets cannot all be detected automatically. If your search terms or URLs must not be visible to conversation readers, review this policy before enabling the feature.

There is no extra setting for this display mode. It uses the existing switch below, and Gateway checks whether Proxy supports it. Supported operations are page searches, HTTP(S) navigation, and listing tabs; other operations use supplemental review. Tools already allowed without confirmation do not gain new confirmation prompts.

### Find the active file (on the Proxy machine)

If someone else manages the Proxy, give them this section. Otherwise, find its service in the Proxy machine's terminal:

```sh
systemctl --user list-unit-files '*hoshikage*'
```

If it lists `codex-hoshikage-proxy.service`, inspect its startup settings. Substitute the actual name if different:

```sh
systemctl --user cat codex-hoshikage-proxy.service
```

The output may contain secrets; do not paste it into Discord. Locally inspect `Environment=`, any `EnvironmentFile=`, and any startup wrapper script. Configuration is chosen in this order:

1. If present, `CODEX_HOSHIKAGE_PROXY_CONFIG` names the file.
2. Otherwise, `CODEX_HOSHIKAGE_PROXY_HOME` names the folder containing `config.toml`.
3. Otherwise, use `~/.config/codex-hoshikage-proxy/config.toml` under the Proxy service user's home.

If no user service exists, try `systemctl list-unit-files '*hoshikage*'` for system services. For containers, inspect their launch configuration. Creating a new config file without identifying the active one will not change the running Proxy.

Keep a copy before editing. These commands assume the standard path; replace both paths if different:

```sh
cp -i "$HOME/.config/codex-hoshikage-proxy/config.toml" "$HOME/.config/codex-hoshikage-proxy/config.toml.before-mcp"
nano "$HOME/.config/codex-hoshikage-proxy/config.toml"
```

If asked to overwrite an earlier backup, answer `n` to keep it.

| Proxy setting | Default | What it controls |
| --- | --- | --- |
| `v2.mcp_turn_approval_enabled` | `false` | Set to `true` to enable operation details and task-scoped permissions |
| `v2.mcp_turn_grant_tools` | `{}` | Evaluated MCP server/tool pairs. An empty map means no tools are eligible for task-scoped permission |

Edit **the config.toml actually read by your Proxy service**, not the Gateway config.toml or Codex's own `~/.codex/config.toml`. Add or update these settings inside the existing `[v2]` section; do not duplicate the section.

```toml
[v2]
mcp_turn_approval_enabled = true
mcp_turn_grant_tools = { playwright = ["browser_find"] }
```

This pair is an example evaluated in the Proxy's isolated tests. Check your own server endpoint, tool implementation, and possible operations before selecting tools. Enabling the feature does not automatically grant permission: the user must explicitly choose **この依頼中、このツールを許可 (Allow this tool during this task)**. `browser_evaluate` and `browser_run_code_unsafe` always require individual confirmation, even if added to the map.

### If you do not know which tools to select

Start with `mcp_turn_grant_tools = {}` to enable operation details without task-scoped permissions. Read the server/tool names on the private screen, then have the administrator evaluate the tool's possible operations before adding it. Copying the example does not install an unregistered `playwright` server or add a missing `browser_find` tool. Tool installation belongs to the Proxy/Codex setup.

### Save and restart the Proxy

Do not send new requests during the change. Use `/stop` for running work and check `/status`. Investigate uncertain tasks first; a restart does not prove that they stopped. Tell other connected clients' users about the change.

After saving, run this on the Proxy machine, substituting the service name identified earlier if different:

```sh
systemctl --user restart codex-hoshikage-proxy.service
systemctl --user status codex-hoshikage-proxy.service
journalctl --user -u codex-hoshikage-proxy.service -n 30 --no-pager
```

Continue when it reports `active (running)` without configuration errors. Correct any settings named by startup errors. To revert, restore your configuration copy and restart. For a system service, an administrator should use its normal commands, such as `sudo systemctl restart SERVICE_NAME`; do not create an additional user service.

### Check activation (on the Gateway machine)

“Capabilities” means the features offered by the Proxy. This reads the Gateway settings and key file, then prints only the three relevant entries without putting the key in your command:

```sh
python3 - "$HOME/.config/codex-hoshikage-gateway/config.toml" <<'PY'
import json, pathlib, sys, tomllib, urllib.request, urllib.error
cfg = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())
key = pathlib.Path(cfg["proxy"]["api_key_file"]).read_text().strip()
url = cfg["proxy"]["base_url"].rstrip("/") + "/v2/codex/capabilities"
req = urllib.request.Request(url, headers={"Authorization": "Bearer " + key})
try:
    with urllib.request.urlopen(req, timeout=15) as response:
        data = json.load(response)
    for name in ("mcp_operation_details", "mcp_turn_approval", "mcp_inline_approval"):
        value = data.get(name, {})
        print(name, "enabled=", value.get("enabled", "missing"),
              "profile=", value.get("profile", "missing"))
except urllib.error.HTTPError as error:
    sys.exit("Proxy HTTP error: " + str(error.code))
except urllib.error.URLError:
    sys.exit("Cannot connect to Proxy. Check its URL, service, and TLS.")
PY
```


The first two entries should show `enabled= True profile= native-item-id-v1` for operation details and request-scoped permissions. The third, `mcp_inline_approval`, should show `enabled= True profile= source-conversation-v1` for operation details in the original conversation. If only the third entry is `missing`, Proxy is an older version; the private-screen workflow remains available when the first two entries are enabled. For `False`, check the active settings and restart; for `missing`, check the Proxy version. HTTP 401/403 points to the key or access permissions; connection errors point to the URL or running Proxy.

Finally, make a new Discord request using an eligible tool that requires confirmation. Check that the first card shows the operation, target, and permission buttons together. Verify single-call permission and the task result; use **本人限定で確認 (Review privately)** for operations requiring supplemental review. Then try request-scoped permission with repeated calls and check `/mcp` listing/revocation. Renewed confirmation on the next request is normal. This verifies the user interaction as well as the configuration.

If operation details are missing, check the actual Proxy configuration and restart. If only task-scoped permission is missing, check the server/tool names and the operation's `turn_grant_eligible` and `ineligible_reason`. Redacted or unavailable details can make a listed tool ineligible for a particular call.

For the authoritative settings and full procedure, see the Proxy's [MCP permissions for Discord: user and administrator guide (Japanese)](https://github.com/tanep3/codex-hoshikage-proxy/blob/main/docs/mcp-turn-approval-guide.ja.md). For everyday button usage, see the [user manual](user-manual.md).
