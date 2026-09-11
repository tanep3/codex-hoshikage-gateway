# 導入手順

[English](installation.md) · [ユーザーマニュアル](user-manual.ja.md)

**Proxy API v2対応版が必要です。** Proxyを用意してから、以下の手順でGatewayを導入します。

## 1. Proxyを用意する

[Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy)のAPI v2対応版が必要です。Codex認証・ワーク管理ルート・保存容量・権限はProxy側で設定します。Gatewayには接続URL、APIキー、利用モデルを設定し、cwdやProjectは登録しません。同居・別ホストともHTTP APIで成果物を取得します。別ホストはHTTPSを使用します。

## 2. Discordに作業場所を作りましょう

Discordのデスクトップ版またはブラウザー版で、サーバー一覧の **＋** から自分のサーバーを新規作成し、名前を付けて作成を完了します。個人用サーバーで十分です。コミュニティ機能は不要です。[Discord公式のサーバー作成手順](https://support.discord.com/hc/en-us/articles/204849977-How-do-I-create-a-server)を参照してください。

通常の**テキストチャンネル**、または**フォーラム**を1つ用意しましょう。フォーラムは投稿ごとに会話を分けたいときに便利です。DiscordではCommunityを有効にしたサーバーで、チャンネル追加の「＋」から「フォーラム」を選んで作成します。詳しくは[公式フォーラムFAQ](https://support.discord.com/hc/en-us/articles/6208479917079-Forum-Channels-FAQ)をご覧ください。

専用サーバーを使うか、利用するチャンネルを本人とBotだけが閲覧できるように制限してください。Gatewayの許可ユーザー設定は操作を制限するもので、ほかの閲覧者から投稿を隠す機能ではありません。通常はそのチャンネル内で会話します。任意の `/new` は公開スレッドを追加します。どちらも本人だけが見える非公開会話ではありません。

## 3. Botを作って、投稿を読めるようにしましょう

[Discord Developer Portal](https://discord.com/developers/applications)で専用アプリケーションを新規作成し、名前を付けます。現在は新規アプリにBotユーザーが含まれています。**Bot** ページでトークンを再生成して取得し、手順6で保存します。[公式アプリ作成ガイド](https://docs.discord.com/developers/quick-start/getting-started)を参照してください。

BotトークンはGatewayのDiscord接続用の秘密情報です。Application ID、Public Key、Client Secret、個人のDiscordパスワードとは別物です。Gatewayは起動時に設定サーバー内の当該アプリのコマンドを登録・置換するため、専用アプリを使ってください。

Botページの特権Intent設定で **Message Content Intent** を有効にし、保存します。通常投稿の本文と添付を読むために必要です。この実装はプレゼンスやメンバー一覧のIntentを必要としません。Portalで承認申請が求められた場合は、Discordの手続きを完了してください。[公式のIntent説明](https://docs.discord.com/developers/events/gateway#privileged-intents)を参照してください。

このGateway用アプリのInteractions Endpoint URLは未設定のままにします。Webhook URLや、一般的なBot入門記事にあるHTTPサーバーの構築は不要です。

## 4. Botをサーバーへ招待しましょう

アプリのインストール設定で、サーバー向けの **Guild Install** を有効にします。User Installは使用しません。Discord提供のインストールリンクを選び、サーバーへのインストール用スコープに `bot` と `applications.commands` を設定して保存します。[公式のアプリ導入説明](https://docs.discord.com/developers/resources/application)を参照してください。

Botには、利用するチャンネルとそのスレッドで次の権限を与えます。

| 権限（英語UI名） | Gatewayでの用途 |
| --- | --- |
| View Channels | チャンネル・会話の閲覧 |
| Send Messages | Botのメッセージ送信 |
| Send Messages in Threads | 会話スレッド内での返信 |
| Create Public Threads | `/new` による会話作成 |
| Read Message History | 再起動後を含む依頼の読取・照合 |
| Attach Files | 明示指定した成果物の返送 |

権限名は[Discord公式の権限定義](https://docs.discord.com/developers/topics/permissions)に対応します。この実装にAdministratorやManage Channelsは不要です。Botのロールだけでなく、カテゴリ・チャンネル側の上書き設定も確認してください。本人にもチャンネルの閲覧・投稿・アプリケーションコマンド使用権限が必要です。

インストールリンクを開き、追加先サーバーを選び、要求される権限を確認して承認します。追加するアカウントにはサーバー管理権限が必要です。メンバー一覧にBotが現れれば追加できています。Gatewayを起動するまではオフラインでも構いません。[公式のBot認可フロー](https://docs.discord.com/developers/topics/oauth2#bot-authorization-flow)を参照してください。

## 5. サーバーと本人のDiscord IDを控えましょう

Discordのユーザー設定の詳細設定で開発者モードを有効にします。自分のユーザー／プロフィールのメニューからユーザーID、サーバーアイコンのメニューからサーバーIDをコピーします。チャンネルIDは投稿場所から自動取得します。[公式のID確認方法](https://support.discord.com/hc/en-us/articles/206346498-Where-can-I-find-my-User-Server-Message-ID)を参照してください。

| 取得する値 | 設定項目 |
| --- | --- |
| サーバーID | `discord.guild_id` |
| Botではなく、操作する本人のユーザーID | `discord.allowed_user_id` |

TOMLでは引用符で囲んだ文字列として設定します。名前や招待リンクでは代用できません。

## 6. Gatewayをビルドして、キーを保存しましょう

ソースを置くディレクトリから実行します。

```sh
git clone https://github.com/tanep3/codex-hoshikage-gateway.git
cd codex-hoshikage-gateway
cargo install --path . --locked
install -d -m 700 "$HOME/.config/codex-hoshikage-gateway"
cp config/config.example.toml "$HOME/.config/codex-hoshikage-gateway/config.toml"
chmod 600 "$HOME/.config/codex-hoshikage-gateway/config.toml"
```

`cargo install --path .` でインストールできます。上の例は依存バージョンをCargo.lockにそろえるため `--locked` も付けています。通常の配置先は `~/.cargo/bin/codex-hoshikage-gateway` です。`~/.cargo/bin` にPATHが通っていれば、`codex-hoshikage-gateway --version` で確認できます。`CARGO_HOME` やCargoのインストール先を変更している場合は、以降のパスとサービスの `ExecStart` を実際の配置先に合わせてください。

設定例のコピーは**新規導入用**です。更新時は既存の設定を上書きしないでください。rootではなく、サービスを実行する本人のアカウントで作業します。

Botトークンを非表示の入力欄へ貼り付けます。先頭に `Bot ` を付けず、トークンのみを入力してください。トークンそのものはシェルのコマンド履歴に入りません。

```bash
read -r -s -p 'Discord Bot token: ' gateway_bot_token
printf '\n'
(umask 077; printf '%s\n' "$gateway_bot_token" > "$HOME/.config/codex-hoshikage-gateway/discord-token")
unset gateway_bot_token
chmod 600 "$HOME/.config/codex-hoshikage-gateway/discord-token"
```

続いて、**Proxy APIキー**を専用ファイルへ保存しましょう。これは自分のProxyへ接続するためのキーで、OpenAIのAPIキーではありません。Gatewayにはキーファイルが必要なので、今回のループバック接続でもProxy側に空でないAPIキーを設定しておいてください。

```bash
read -r -s -p 'Proxy API key: ' gateway_proxy_key
printf '\n'
(umask 077; printf '%s\n' "$gateway_proxy_key" > "$HOME/.config/codex-hoshikage-gateway/proxy-key")
unset gateway_proxy_key
chmod 600 "$HOME/.config/codex-hoshikage-gateway/proxy-key"
```

キーはサービス実行ユーザーが所有する通常ファイルに保存し、グループ・他ユーザーには権限を与えません。シンボリックリンクは拒否されます。Git・Discord投稿・スクリーンショットに含めないでください。Botトークンを変更するときはGatewayを停止し、Portalで再生成し、この非表示入力でファイルを更新して再起動します。再生成後は旧トークンでは接続できません。

## 7. Gatewayを設定する

[設定例](../config/config.example.toml)をconfig.tomlへコピーし、DiscordのGuild ID・本人ID・token_file、ProxyのURL・api_key_file、default_modelを設定します。`proxy.contract_version = "2.0"`を使います。`response_mode`は`all`または`mention`です。

容量はサンプルの正の標準値から始めます。tempは `~/.config/codex-hoshikage-gateway/temp` を基本とし、実際の絶対パスを設定します。ここは配信用キャッシュで、Codexの実行先ではありません。state_dirとsocket_pathも自分の環境に合わせます。

## 8. まずターミナルで起動しましょう

```sh
cargo install --path . --locked
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" check
# 新規導入時だけ。既存DBを初期化し直さないでください。
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" init
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" run
```

Cargoのインストール先設定をそのまま使います。会話場所で挨拶し、モデル変更、停止、保存回答・成果物の再取得を検証します。Ctrl+Cで終了するまで、このターミナルで動作します。この段階では起動用シェルやsystemdの設定は不要です。

## 9. 必要ならバックグラウンドで動かしましょう

手動起動で確認できたらCtrl+Cを押し、Gatewayプロセスの終了を待ちます。リポジトリのディレクトリから次を実行します。

```sh
install -d "$HOME/.config/systemd/user"
install -m 644 packaging/codex-hoshikage-gateway.service "$HOME/.config/systemd/user/codex-hoshikage-gateway.service"
systemctl --user daemon-reload
systemctl --user enable --now codex-hoshikage-gateway.service
systemctl --user status codex-hoshikage-gateway.service
journalctl --user -u codex-hoshikage-gateway.service -n 50 --no-pager
```

systemdはバイナリを直接起動するので、起動用シェルは不要です。サービス例は上記のバイナリ・設定配置を使用します。変更した場合は有効化前に `ExecStart` を編集してください。同じ状態領域で複数起動しないでください。ログインしていない間もユーザーサービスを継続するには、ホスト管理者によるlinger設定が必要な場合があります。例: `loginctl enable-linger USERNAME`。

停止は `systemctl --user stop codex-hoshikage-gateway.service`、再起動は `systemctl --user restart codex-hoshikage-gateway.service` です。Gatewayサービスを止めることはCodexの停止確認の代わりにはなりません。計画メンテナンス前は `/stop` で依頼の状態を確認してください。

## 10. 運用中に困ったら

設定、状態領域、`gateway-instance.json` を保持してください。初期化時の識別設定があるため、既存DBを別ユーザー・別サーバー・別Proxy URL・別状態領域へ単純に向け直すことはできません。起動エラーをSQLiteの直接編集や識別ファイルの削除で回避しないでください。

稼働中はサービス実行ユーザーがローカルで状態を確認し、整合したバックアップを作成できます。

```sh
"$HOME/.cargo/bin/codex-hoshikage-gateway" --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin status
"$HOME/.cargo/bin/codex-hoshikage-gateway" --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin backup --to /absolute/path/new-backup-bundle
```

保存先は本人だけがアクセスできる新しいパスに変更してください。稼働中のSQLite本体だけをコピーしないでください。このバックアップはGatewayの状態用で、プロジェクトのファイル・Discord履歴・キー・失われた回答本文は含みません。設定と識別ファイルは別途適切な権限で保管してください。復元時は古い依頼を再送せず隔離します。復元操作の前に[技術設計の復旧規則](system-design.ja.md)を確認してください。

| 症状 | 確認すること |
| --- | --- |
| `check` が失敗する | ID・パス・モデルの仮文字列、変更した上限値、絶対パス、キーファイルの所有者・権限 |
| Botがオフラインのまま | サービスログ、トークン、外向き通信、Intent設定 |
| スラッシュコマンドが出ない | アプリと導入先サーバー、コマンド用スコープ、本人のコマンド権限、Gateway起動成功 |
| 通常投稿に反応しない | 許可ユーザー、登録チャンネル、応答モードとBotメンション、Message Content Intent |
| スレッド作成・返信ができない | カテゴリ／チャンネルの上書き権限、手順4の6権限 |
| Proxyが準備完了にならない | 常駐状態、APIキー、必須機能と契約。`check` の成功だけでは接続確認にならない |
| 添付が拒否される | 対応形式、設定上限、実際のファイル内容、Discordのアップロード上限 |
| 依頼が `UNKNOWN` | 状態確認とProxyとの照合。同じ依頼を再投稿して進めようとしない |

準備ができたら、[ユーザーマニュアル](user-manual.ja.md)へどうぞ。最初の依頼の仕方と、使えるコマンドをまとめています。

Discordの設定手順は **2026-09-11** にリンク先の公式資料で確認しました。表示名は言語やその後のUI更新で変わる場合があります。

[運用ガイド](operations.ja.md)
