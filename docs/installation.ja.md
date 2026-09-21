# 導入・設定手順

[English](installation.md) · [使い方](user-manual.ja.md)

この手順を終えると、自分のDiscordサーバーでBotに話しかけ、Codexへ作業を頼めます。GatewayとCodexは**同じLinuxマシン**で動きます。Proxyのインストール、ProxyのURLやAPIキーは必要ありません。

用意するものは、Discordアカウント、自分がBotを追加できるサーバー、Gatewayを動かし続けるLinuxマシン、Git、Rust/Cargo（**1.98以上**）、Codex CLIです。現在の製品設定は**サーバー1つ・利用者1人**です。コマンド例はLinuxのbash向けです。

## 1. DiscordにBotの居場所を作る

既存のサーバーを使えます。新しく作るなら、Discord左側の「＋」からサーバーを作成します（[Discord公式の作成手順](https://support.discord.com/hc/en-us/articles/204849977-How-do-I-create-a-server)）。個人的な作業なら、自分とBotだけが閲覧できるチャンネルを作ってください。**Gatewayが利用者を1人に制限しても、チャンネル内の会話がほかの閲覧者から隠れるわけではありません。**

テキストチャンネルはチャンネルごとに会話が続きます。フォーラムを使うと投稿ごとに別の会話になります。フォーラムを作るにはDiscordサーバーのコミュニティ機能が必要です（[Discord公式のフォーラム案内](https://support.discord.com/hc/en-us/articles/6208479917079-Forum-Channels-FAQ)）。フォーラムは必須ではありません。

## 2. Discord Botを作ってサーバーへ追加する

1. [Discord Developer Portal](https://discord.com/developers/applications)で **New Application** を選び、名前を付けます。
2. **Bot** ページでBotトークンを発行または再発行し、安全な場所へ一時的に控えます。トークンはBotを操作できる秘密情報です。DiscordのチャットやGitへ貼らないでください。
3. 同じ **Bot** ページの **Privileged Gateway Intents** で **Message Content Intent** を有効にし、変更を保存します。通常の投稿内容を読むために必要です。Discordの条件によっては追加の承認が必要です（[Discord公式の説明](https://docs.discord.com/developers/events/gateway#privileged-intents)）。
4. **Installation** ページで **Guild Install** を有効にします。インストール時のスコープは `bot` と `applications.commands`。Botには **View Channel / Send Messages / Send Messages in Threads / Read Message History / Attach Files** を付けます。アプリのインストールリンクを開き、手順1のサーバーを選んで追加します。画面の名称が違う場合は[Discord公式のアプリ設定ガイド](https://docs.discord.com/developers/quick-start/getting-started)を確認してください。
5. チャンネル固有の権限設定でもBotが閲覧・投稿できるか確認します。スラッシュコマンドを使う本人には **Use Application Commands** が必要です。

## 3. 自分のDiscord IDを控える

Discordの **ユーザー設定 → 詳細設定 → 開発者モード** を有効にします。自分のアカウントを右クリックして **IDをコピー**、サーバーのアイコンを右クリックして **サーバーIDをコピー**。数値を後で使います。チャンネルIDは設定に不要です（[Discord公式のIDの調べ方](https://support.discord.com/hc/en-us/articles/206346498-Where-can-I-find-my-User-Server-Message-ID)）。

## 4. Codexを用意する

Gatewayを動かす**同じLinuxユーザー**で、[Codex CLI公式の導入手順](https://developers.openai.com/ja-JP/docs/codex/cli)に従ってCodexをインストールします。`codex --version` と `command -v codex` で起動と実行ファイルの場所を確認してください。

Gateway専用のCodex設定・ログイン情報を、次の場所へ作ります。ここには認証情報が入るので、他人が読めないようにします。ログイン時はブラウザーでの操作を求められる場合があります。

```bash
mkdir -p "$HOME/.config/codex-hoshikage-gateway/codex-home"
chmod 700 "$HOME/.config/codex-hoshikage-gateway/codex-home"
CODEX_HOME="$HOME/.config/codex-hoshikage-gateway/codex-home" codex login
CODEX_HOME="$HOME/.config/codex-hoshikage-gateway/codex-home" codex login status
```

普段の `~/.codex` とは別の場所です。**普段の `auth.json` をここへコピーしないでください。** 認証の更新時に片方だけ古くなり、Botが回答できなくなることがあります。この専用の場所でログインを完了してください。サーバー上でブラウザーを開けない場合は、同じ `CODEX_HOME` を指定して `codex login --device-auth` を実行し、表示されたURLとコードを自分のブラウザーで使えます。`codex login status` はログイン情報の有無を示すだけで、実際に回答できる保証にはなりません。起動後にDiscordで短い会話を試してください。

CodexのMCPツールを使う場合も、この専用 `CODEX_HOME` に設定してください。通常のCodex設定だけに登録しても、Gatewayからは見えません。認証方法と保存先は[OpenAI公式の認証案内](https://developers.openai.com/ja-JP/docs/auth)も参照してください。

## 5. Gatewayをインストールする

```bash
git clone https://github.com/tanep3/codex-hoshikage-gateway.git
cd codex-hoshikage-gateway
cargo install --path . --locked
command -v codex-hoshikage-gateway
```

Cargoのインストール先を `~/bin` に変えている方は、その設定がそのまま使われます。最後のコマンドで表示された**実際のパス**を、後のサービス設定で使います。見つからない場合はCargoが表示したインストール先を確認し、フルパスで実行してください。

## 6. Botトークンと設定ファイルを置く

トークンは専用ファイルへ保存します。次のコマンドは入力文字を画面に表示しません。Developer Portalで控えた**Botトークン**を貼り付け、Enterを押します。

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

`~/.config/codex-hoshikage-gateway/config.toml` をテキストエディターで開きます。まず `REPLACE_USER` をLinuxのユーザー名に、`REPLACE_UID` を `id -u` で表示される数値に置き換えます。次の欄を確認してください。

| 設定欄 | 入れるもの |
| --- | --- |
| `discord.guild_id` | 手順3でコピーしたサーバーID |
| `discord.allowed_user_id` | 手順3でコピーした自分のユーザーID |
| `discord.response_mode` | `"all"` は自分の全投稿、`"mention"` はBotをメンションした投稿だけに応答 |
| `discord.token_file` | 手順6で保存した `discord-token` の**絶対パス** |
| `codex.command` | `command -v codex` で表示されたCodexの**絶対パス**。例の `.local/bin` と違えば必ず直す |
| `codex.home` | 手順4で作った専用 `codex-home` の絶対パス |
| `codex.workspace_root` | 作業ファイルを置く親フォルダーの絶対パス。会話ごとの子フォルダーは自動作成 |
| `default_model` | 最初に使うモデルのID。例のモデルが使えなければ、使えるモデルに変更 |
| `storage.state_dir` / `temp_dir` / `socket_path` | 会話記録・一時ファイル・管理用socketの保存先。例のユーザー名とUIDを直す |

`codex.workspace_root` は例えば `/home/あなたの名前/work/codex-hoshikage-gateway-workspaces` にできます。ファイルはこの配下に保存され、Discordの `/workspace` でも現在の場所を確認できます。**保存先設定を後から変えても、既存の会話のファイルは自動移動しません。**

最初はサンプルの `[limits]` をそのまま使えます。各値は容量・件数の上限で、`0` は無制限を意味しません。`codex.sandbox = "workspace-write"`、`approval_policy = "on-request"` は通常そのままにします。`network_access = false` はCodexの作業環境からのネットワークアクセスを許可しない設定です。外部サイトへのアクセスが必要な作業では、内容を理解したうえで変更してください。

## 7. 設定を確認して起動する

```bash
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" direct check
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" direct init
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" direct run
```

`direct check` は**ローカルの設定とトークンを確認するだけ**で、Discordへの接続やCodexのログイン成功までは検証しません。`direct init` は初回だけ実行します。`direct run` を動かしたまま、許可したサーバーのチャンネルで「こんにちは」と送ります。`mention` モードならBotをメンションして送ってください。返事が来たら基本設定は完了です。`/models` で使えるモデルのIDを確認し、必要なら `/model` で選べます。

ここで返事がない場合は、まず **サーバーID・ユーザーID・Botトークン・Message Content Intent・チャンネル権限**を確認します。Codexのログイン状態は手順4の `codex login status` で確認します。新しいBotコマンドが見えない場合は、Botが正しいサーバーへ追加されているか確認し、Discordを開き直してください。

## 8. ログアウト後も動かす

手順7の前面起動を **Ctrl+C** で止めてから、付属のsystemdユーザーサービスを設定します。起動用シェルスクリプトは不要です。

```bash
mkdir -p "$HOME/.config/systemd/user"
cp packaging/codex-hoshikage-gateway.service "$HOME/.config/systemd/user/"
systemctl --user daemon-reload
```

コピーした `~/.config/systemd/user/codex-hoshikage-gateway.service` の `ExecStart=` を確認します。初期値の実行ファイルは `%h/.cargo/bin/codex-hoshikage-gateway` です。手順5で `~/bin` など別の場所が表示された場合は、**その絶対パスへ書き換えてください**。行末は `--config %h/.config/codex-hoshikage-gateway/config.toml direct run` です。

```bash
systemctl --user daemon-reload
systemctl --user enable --now codex-hoshikage-gateway.service
systemctl --user status codex-hoshikage-gateway.service
```

`active (running)` を確認し、Discordでももう一度話しかけます。サービスが動かないときは `journalctl --user -u codex-hoshikage-gateway.service -n 50 --no-pager` で直近の記録を確認します。ログアウト後もユーザーサービスを動かす必要があるマシンでは、管理者に `loginctl enable-linger ユーザー名` を依頼してください。**同じ保存先でGatewayを2つ同時起動しないでください。**

導入後の使い方は[ユーザーマニュアル](user-manual.ja.md)、止まった会話やサービスの対処は[運用・復旧ガイド](operations.ja.md)へ進んでください。
