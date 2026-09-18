# Installation and setup

[日本語](installation.ja.md) · [User manual](user-manual.md)

When you finish this guide, you can talk to Codex through a Bot in your Discord server. The Gateway and Codex run on the **same Linux computer**. No Proxy installation, Proxy URL, or Proxy API key is required.

You need a Discord account, a server where you can add a Bot, an always-on Linux computer, Git, Rust/Cargo **1.98 or newer**, and Codex CLI. This release supports **one Discord server and one authorized user**. Shell examples use bash.

## 1. Give the Bot a place to live

You can use an existing server. To create one, select **+** in Discord's server list ([Discord's server guide](https://support.discord.com/hc/en-us/articles/204849977-How-do-I-create-a-server)). For personal work, make a channel that only you and the Bot can view. **Restricting who can operate the Gateway does not hide messages from other channel viewers.**

A text channel has one continuing conversation; each forum post has its own. Forum channels require Discord's Community feature ([Discord's forum guide](https://support.discord.com/hc/en-us/articles/6208479917079-Forum-Channels-FAQ)). You do not need a forum to get started.

## 2. Create the Discord Bot and add it to your server

1. Open the [Discord Developer Portal](https://discord.com/developers/applications), choose **New Application**, and name it.
2. On **Bot**, create or reset the Bot token and keep it temporarily in a safe place. This token controls your Bot. Never paste it into Discord chat or Git.
3. On the same **Bot** page, turn on **Message Content Intent** under **Privileged Gateway Intents**, then save. This lets the Bot read ordinary messages. Discord may require additional approval in some cases ([Discord's intent documentation](https://docs.discord.com/developers/events/gateway#privileged-intents)).
4. Under **Installation**, enable **Guild Install**. Select the `bot` and `applications.commands` scopes. Give the Bot **View Channel, Send Messages, Send Messages in Threads, Read Message History, and Attach Files**. Open the app's install link and select the server from step 1. If labels have changed, follow [Discord's app setup guide](https://docs.discord.com/developers/quick-start/getting-started).
5. Check channel-specific permissions too. Your own account needs **Use Application Commands** for slash commands.

## 3. Copy your Discord IDs

Enable **User Settings → Advanced → Developer Mode** in Discord. Right-click your account and **Copy User ID**. Right-click the server icon and **Copy Server ID**. Keep both numbers for step 6. You do not need to configure channel IDs ([Discord's ID guide](https://support.discord.com/hc/en-us/articles/206346498-Where-can-I-find-my-User-Server-Message-ID)).

## 4. Install and sign in to Codex

On the Linux computer, use the **same Linux account** that will run the Gateway. Install Codex using the [official Codex CLI guide](https://learn.chatgpt.com/docs/codex/cli). Check `codex --version` and `command -v codex`.

Give the Gateway its own private Codex settings and sign-in. Your browser may open during sign-in. Keep this directory private because it can contain credentials.

```bash
mkdir -p "$HOME/.config/codex-hoshikage-gateway/codex-home"
chmod 700 "$HOME/.config/codex-hoshikage-gateway/codex-home"
CODEX_HOME="$HOME/.config/codex-hoshikage-gateway/codex-home" codex login
CODEX_HOME="$HOME/.config/codex-hoshikage-gateway/codex-home" codex login status
```

This is separate from your usual `~/.codex`. If you want MCP tools, configure them in this dedicated `CODEX_HOME`; registering them only in your normal Codex settings will not make them available to the Gateway. See [OpenAI's authentication guide](https://learn.chatgpt.com/docs/auth) for sign-in and credential storage.

## 5. Install the Gateway

```bash
git clone https://github.com/tanep3/codex-hoshikage-gateway.git
cd codex-hoshikage-gateway
cargo install --path . --locked
command -v codex-hoshikage-gateway
```

If you configured Cargo to install into `~/bin`, that choice is respected. Keep the **actual path** printed by the last command for the service setup. If the command is not found, check the install destination printed by Cargo and use the full path.

## 6. Save the Bot token and edit the configuration

Save the Bot token in its own file. The following prompt does not display what you type. Paste the token from the Developer Portal and press Enter.

```bash
mkdir -p "$HOME/.config/codex-hoshikage-gateway"
chmod 700 "$HOME/.config/codex-hoshikage-gateway"
read -r -s -p 'Discord Bot token: ' gateway_bot_token
printf '\n'
(umask 077; printf '%s\n' "$gateway_bot_token" > "$HOME/.config/codex-hoshikage-gateway/discord-token")
unset gateway_bot_token
cp config/config.example.toml "$HOME/.config/codex-hoshikage-gateway/config.toml"
chmod 600 "$HOME/.config/codex-hoshikage-gateway/config.toml"
```

Open `~/.config/codex-hoshikage-gateway/config.toml` in a text editor. Replace `REPLACE_USER` with your Linux username and `REPLACE_UID` with the number printed by `id -u`. Then check these fields:

| Setting | What to enter |
| --- | --- |
| `discord.guild_id` | Server ID from step 3 |
| `discord.allowed_user_id` | Your User ID from step 3 |
| `discord.response_mode` | `"all"` answers all your messages; `"mention"` answers only messages that mention the Bot |
| `discord.token_file` | **Absolute path** to the `discord-token` file from this step |
| `codex.command` | **Absolute path** printed by `command -v codex`; correct the sample `.local/bin` path if necessary |
| `codex.home` | Absolute path to the private `codex-home` created in step 4 |
| `codex.workspace_root` | Absolute path of the parent folder for your work. Per-conversation folders are created automatically |
| `default_model` | Initial model ID; change the example if your Codex installation does not offer that model |
| `storage.state_dir` / `temp_dir` / `socket_path` | Conversation state, temporary files, and management socket. Correct the sample username and UID |

For example, `codex.workspace_root` can be `/home/YOUR_NAME/work/codex-hoshikage-gateway-workspaces`. Files go underneath this folder. Discord's `/workspace` shows the location for the current conversation. **Changing the setting later does not move an existing conversation's files.**

You can leave the sample `[limits]` values alone at first. They are size and count limits; `0` does not mean unlimited. Normally leave `codex.sandbox = "workspace-write"` and `approval_policy = "on-request"` as shown. `network_access = false` prevents network access from Codex's working environment. Change it only when you understand that your tasks need it.

## 7. Check and start the Gateway

```bash
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" direct check
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" direct init
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" direct run
```

`direct check` validates **local settings and the Bot token file**. It does not prove that Discord is connected or Codex sign-in works. Run `direct init` only once. Leave `direct run` open and say “Hello” in a channel in the authorized server. If you chose `mention`, mention the Bot. A reply confirms basic setup. `/models` shows available model IDs; `/model` lets you choose one.

No reply? Check the **server ID, user ID, Bot token, Message Content Intent, and channel permissions** first. Check Codex sign-in with `codex login status` as in step 4. If slash commands are absent, make sure the Bot was installed in the correct server and reopen Discord.

## 8. Keep it running after logout

Stop the foreground run from step 7 with **Ctrl+C**. Then install the included systemd user service. No launcher shell script is needed.

```bash
mkdir -p "$HOME/.config/systemd/user"
cp packaging/codex-hoshikage-gateway.service "$HOME/.config/systemd/user/"
systemctl --user daemon-reload
```

Open `~/.config/systemd/user/codex-hoshikage-gateway.service` and check `ExecStart=`. Its sample binary path is `%h/.cargo/bin/codex-hoshikage-gateway`. If step 5 showed `~/bin` or another location, **replace it with that absolute path**. The rest of the line must end in `--config %h/.config/codex-hoshikage-gateway/config.toml direct run`.

```bash
systemctl --user daemon-reload
systemctl --user enable --now codex-hoshikage-gateway.service
systemctl --user status codex-hoshikage-gateway.service
```

Look for `active (running)` and try talking to the Bot again. For startup problems, run `journalctl --user -u codex-hoshikage-gateway.service -n 50 --no-pager`. If your host must keep user services running after logout, ask its administrator to enable lingering with `loginctl enable-linger USERNAME`. **Do not run two Gateway instances against the same state directory.**

Continue with the [user manual](user-manual.md). For stalled conversations or service recovery, see [Operations](operations.md).
