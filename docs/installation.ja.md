# 導入手順書

[English](installation.md) · [製品概要](../README.ja.md) · [ユーザーマニュアル](user-manual.ja.md)

DiscordからCodexへ話しかけられるように、順番に準備していきましょう。Proxyを用意し、サーバーにBotを追加して、Gatewayを起動するところまで案内します。コマンドはGatewayを置くUbuntuホストのBashで実行してください。準備ができたら、日常の使い方は[ユーザーマニュアル](user-manual.ja.md)へどうぞ。

対象は開発版0.1.0のソース導入です。まず検証用プロジェクトで以下の確認を行ってください。実サービスの受入試験とリリース用パッケージの検証は未完了です。

## 1. まずはProxyとホストを用意しましょう

**このGatewayには [Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy) が必要です。** Codexへの接続をProxyが、Discordでの操作をGatewayが担当し、それぞれ別のサービスとして動きます。

まだProxyを導入していなければ、先に[Proxyの導入手順書](https://github.com/tanep3/codex-hoshikage-proxy/blob/main/docs/installation.ja.md)へ進んでください。Codexの準備・認証、Proxyの設定、サービス起動まで案内されています。起動できたら、接続URL・APIキー・作業フォルダー・モデルIDを確認して、ここへ戻ってきてください。すでに動いている場合は、以下の条件を確認して先へ進めます。

用意するものはこちらです。

- Gatewayのファイルを所有し、サービスを実行するUbuntuユーザー。
- Git、Cビルドツール一式、**1.98以上のRust/Cargo**。記録済みの開発検証はRust 1.98.1で実施しています。未導入なら[Rust公式の導入案内](https://www.rust-lang.org/tools/install)に従ってください。
- [Control API契約 **1.0**](https://github.com/tanep3/codex-hoshikage-proxy/blob/main/docs/control-api.ja.md) と必須のGateway向け機能に対応した、常駐中の [codex-hoshikage-proxy](https://github.com/tanep3/codex-hoshikage-proxy)。接続URL、APIキー、許可された作業フォルダー、利用可能なProvider付きモデルIDを確認します。一般的なResponses APIだけでは足りません。
- ホストからDiscordへの外向き通信と、Proxyへの接続。
- 対象Discordサーバーの所有者、またはアプリを導入・管理できるDiscordアカウント。

初回はGatewayとProxyで同じホスト上の既存作業フォルダーを使ってください。Codexへの接続、作業先の許可、承認方針、ネットワーク利用は事前にProxy側で設定します。Gatewayの設定でProxyの権限を拡張することはできません。

設定例のProxy URLは `http://127.0.0.1:4040` ですが、実際の接続先を確認してください。ループバックはGateway自身のホストを指します。GatewayはDiscordへWebSocketとRESTで接続するため、公開Webサーバー・受信用ポート・Interactions Endpoint URLは不要です。

## 2. Discordに作業場所を作りましょう

Discordのデスクトップ版またはブラウザー版で、サーバー一覧の **＋** から自分のサーバーを新規作成し、名前を付けて作成を完了します。個人用サーバーで十分です。コミュニティ機能は不要です。[Discord公式のサーバー作成手順](https://support.discord.com/hc/en-us/articles/204849977-How-do-I-create-a-server)を参照してください。

最初のプロジェクト用に、例えば `project-demo` という通常の**テキストチャンネル**を作ります。プロジェクトごとに1チャンネル必要です。このGatewayではフォーラム・ボイスチャンネルをプロジェクト用には使いません。チャンネルの種類と公開範囲は[公式サーバー設定ガイド](https://support.discord.com/hc/en-us/articles/33023827550359-Discord-Server-Setup-Guide)で確認できます。

専用サーバーを使うか、プロジェクトチャンネルを本人とBotだけが閲覧できるように制限してください。Gatewayの許可ユーザー設定は操作を制限するもので、ほかの閲覧者から投稿を隠す機能ではありません。通常はそのチャンネル内で会話します。任意の `/new` は公開スレッドを追加します。どちらも本人だけが見える非公開会話ではありません。

## 3. Botを作って、投稿を読めるようにしましょう

[Discord Developer Portal](https://discord.com/developers/applications)で専用アプリケーションを新規作成し、名前を付けます。現在は新規アプリにBotユーザーが含まれています。**Bot** ページでトークンを再生成して取得し、手順6で保存します。[公式アプリ作成ガイド](https://docs.discord.com/developers/quick-start/getting-started)を参照してください。

BotトークンはGatewayのDiscord接続用の秘密情報です。Application ID、Public Key、Client Secret、個人のDiscordパスワードとは別物です。Gatewayは起動時に設定サーバー内の当該アプリのコマンドを登録・置換するため、専用アプリを使ってください。

Botページの特権Intent設定で **Message Content Intent** を有効にし、保存します。通常投稿の本文と添付を読むために必要です。この実装はプレゼンスやメンバー一覧のIntentを必要としません。Portalで承認申請が求められた場合は、Discordの手続きを完了してください。[公式のIntent説明](https://docs.discord.com/developers/events/gateway#privileged-intents)を参照してください。

このGateway用アプリのInteractions Endpoint URLは未設定のままにします。Webhook URLや、一般的なBot入門記事にあるHTTPサーバーの構築は不要です。

## 4. Botをサーバーへ招待しましょう

アプリのインストール設定で、サーバー向けの **Guild Install** を有効にします。User Installは使用しません。Discord提供のインストールリンクを選び、サーバーへのインストール用スコープに `bot` と `applications.commands` を設定して保存します。[公式のアプリ導入説明](https://docs.discord.com/developers/resources/application)を参照してください。

Botには、プロジェクトチャンネルとそのスレッドで次の権限を与えます。

| 権限（英語UI名） | Gatewayでの用途 |
| --- | --- |
| View Channels | プロジェクト・会話の閲覧 |
| Send Messages | Botのメッセージ送信 |
| Send Messages in Threads | 会話スレッド内での返信 |
| Create Public Threads | `/new` による会話作成 |
| Read Message History | 再起動後を含む依頼の読取・照合 |
| Attach Files | 明示指定した成果物の返送 |

権限名は[Discord公式の権限定義](https://docs.discord.com/developers/topics/permissions)に対応します。この実装にAdministratorやManage Channelsは不要です。Botのロールだけでなく、カテゴリ・チャンネル側の上書き設定も確認してください。本人にもチャンネルの閲覧・投稿・アプリケーションコマンド使用権限が必要です。

インストールリンクを開き、追加先サーバーを選び、要求される権限を確認して承認します。追加するアカウントにはサーバー管理権限が必要です。メンバー一覧にBotが現れれば追加できています。Gatewayを起動するまではオフラインでも構いません。[公式のBot認可フロー](https://docs.discord.com/developers/topics/oauth2#bot-authorization-flow)を参照してください。

## 5. 3つのDiscord IDを控えましょう

Discordのユーザー設定の詳細設定で開発者モードを有効にします。自分のユーザー／プロフィールのメニューからユーザーID、サーバーアイコンのメニューからサーバーID、プロジェクトチャンネルのメニューからチャンネルIDをコピーします。[公式のID確認方法](https://support.discord.com/hc/en-us/articles/206346498-Where-can-I-find-my-User-Server-Message-ID)を参照してください。

| 取得する値 | 設定項目 |
| --- | --- |
| サーバーID | `discord.guild_id` |
| Botではなく、操作する本人のユーザーID | `discord.allowed_user_id` |
| スレッドではなく、プロジェクト用テキストチャンネルのID | `projects.channel_id` |

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

## 7. あなたの環境をGatewayに教えましょう

テキストエディターで `$HOME/.config/codex-hoshikage-gateway/config.toml` を編集します。[設定例の全体](../config/config.example.toml)を基準にしてください。

**例の `/home/tane/...` はすべて自分の絶対パスへ変更します。** TOML内では `~` や `$HOME` は展開されません。ホームは `printf '%s\n' "$HOME"`、ユーザーの数値IDは `id -u` で確認できます。

| 設定項目 | 入力する内容 |
| --- | --- |
| `discord.guild_id`、`allowed_user_id` | 手順5のID |
| `discord.response_mode` | `"all"`：許可本人の全投稿に応答（既定）。`"mention"`：Botへの明示メンション時だけ応答 |
| `discord.token_file` | `discord-token` の絶対パス |
| `proxy.base_url` | 常駐Proxyの接続URL |
| `proxy.api_key_file` | `proxy-key` の絶対パス |
| `proxy.contract_version` | 現在の実装は `"1.0"` |
| `storage.state_dir` | Gateway専用の永続状態ディレクトリ |
| `storage.temp_dir` | 専用の一時ディレクトリ。他アプリと共用しない |
| `storage.socket_path` | 例: `/run/user/YOUR_UID/codex-hoshikage-gateway/admin.sock`。`YOUR_UID` を本人の数値IDへ変更 |
| `projects.id` | 固有で変更しないUUID。Ubuntuでは `cat /proc/sys/kernel/random/uuid` で生成可能 |
| `projects.name` | わかりやすいプロジェクト名 |
| `projects.channel_id` | プロジェクトのテキストチャンネルID |
| `projects.cwd` | Proxyが許可した既存作業フォルダーの絶対パス |
| `projects.default_model` | 実際に利用できるProvider付きモデルID。仮文字列を置換する |
| `projects.lifecycle` | `"ACTIVE"` |

初回は本人のアカウントで専用保存ディレクトリを権限 `0700` で作ります。設定例と同じ配置を自分のホームで使う場合は次のとおりです。

```sh
install -d -m 700 "$HOME/.local/state/codex-hoshikage-gateway"
install -d -m 700 "$HOME/.local/state/codex-hoshikage-gateway/temp"
```

プロジェクトを増やす場合は `[[projects]]` ブロックを追加します。IDとチャンネルを重複させず、作業フォルダーの実体が同一・親子関係にならないようにしてください。初期化後はプロジェクトID・チャンネル・フォルダーの対応を維持します。

### 容量の設定は、まずそのままでOK

設定例には、すぐ試せる標準値を入れてあります。**最初は変更しなくて大丈夫です。** 大きなファイルを扱いたいなど、必要になったときだけ調整しましょう。

| 上限項目 | サンプルの標準値 | 意味 |
| --- | --- | --- |
| `attachments` | 4 | 1依頼の添付件数 |
| `attachment_bytes` | 8 MiB | 入力添付1件の最大バイト数 |
| `input_bytes` | 16 MiB | 入力合計の最大バイト数。画像をエンコードした入力も収まる必要がある |
| `text_bytes` | 256 KiB | 本文または個々のテキスト添付のUTF-8最大バイト数 |
| `image_pixels` | 1,600万画素 | 画像1枚を展開したときの最大画素数 |
| `artifact_bytes` | 8 MiB | `/get` で返送するファイルの最大バイト数 |
| `temp_bytes` | 64 MiB | 共有する一時ファイル領域の容量（バイト） |
| `output_bytes` | 1 MiB | 1依頼の回答を保持する最大バイト数 |
| `output_total_bytes` | 8 MiB | 回答保持領域全体のバイト数 |
| `delivery_retention_secs` | 900秒（15分） | 配信用の回答データをメモリに保持できる秒数 |
| `queue_conversation` | 5 | 1会話の待機依頼上限 |
| `queue_global` | 20 | 全体の待機依頼上限 |
| `validation_secs` | 120秒 | 入力検証の予約期限（秒） |

1 MiBは1,048,576バイト、1 KiBは1,024バイトです。サンプルにはバイト数の整数を記載しています。件数・個別容量・合計容量は同時に適用されるので、8 MiBのファイル4件を一度に送れるという意味ではありません。画像のエンコード後のサイズも合計上限に含みます。

調整する場合は正の整数を使ってください。`0` は標準値への切替や無制限を意味せず、設定エラーになります。`temp_bytes` は `input_bytes` と `artifact_bytes` のそれぞれ以上、`output_total_bytes` は `output_bytes` 以上、全体の待機上限は会話単位以上が必要です。同時実行は2固定です。

これらはGateway側の標準値です。Discord・Proxy・モデル側の制限も適用されます。一時領域64 MiBは16 MiBの入力予約2件と8 MiBの成果物を収められる設定ですが、プロセス全体のメモリ上限ではありません。全環境での性能・ファイル受入を保証する値ではないため、困ったときに環境に合わせて調整してください。

## 8. 起動して、ひとこと話しかけてみましょう

```sh
"$HOME/.cargo/bin/codex-hoshikage-gateway" --config "$HOME/.config/codex-hoshikage-gateway/config.toml" check
"$HOME/.cargo/bin/codex-hoshikage-gateway" --config "$HOME/.config/codex-hoshikage-gateway/config.toml" init
"$HOME/.cargo/bin/codex-hoshikage-gateway" --config "$HOME/.config/codex-hoshikage-gateway/config.toml" run
```

`--config` はサブコマンドより前に置きます。`check` はローカルの設定・キーファイルの検証であり、DiscordやProxyへの接続試験ではありません。`init` は新規導入時に1回だけ実行し、DBと設定ディレクトリ内の識別マーカー `gateway-instance.json` を作ります。`run` には初期化済みの状態が必要です。

許可した本人でDiscordへ入り、登録チャンネルで次を確認します。`/new` は不要です。mentionモードなら通常投稿にBotへのメンションを付けてください。

1. `/status` で準備状態とモデルを確認する。
2. 「短い挨拶を返してください。ファイルは変更しないでください」と依頼し、受付と回答を確認する。
3. 続けて投稿し、会話の文脈が継続することを確認する。
4. 検証用workspaceで、添付、`/get`、`/stop` → `/resume`、モデル選択、該当する場面で承認・Steerを確認する。中断受付だけで実行停止済みとは判断しない。

準備状態が正常にならなければ、Proxy接続・キー・モデル・機能の不一致を解消してから依頼してください。エラー回避のために初期化を繰り返したり、状態領域を削除したりしないでください。

## 9. バックグラウンドで動かしましょう

手動起動で確認できたらCtrl+Cを押し、Gatewayプロセスの終了を待ちます。リポジトリのディレクトリから次を実行します。

```sh
install -d "$HOME/.config/systemd/user"
install -m 644 packaging/codex-hoshikage-gateway.service "$HOME/.config/systemd/user/codex-hoshikage-gateway.service"
systemctl --user daemon-reload
systemctl --user enable --now codex-hoshikage-gateway.service
systemctl --user status codex-hoshikage-gateway.service
journalctl --user -u codex-hoshikage-gateway.service -n 50 --no-pager
```

サービス例は上記のバイナリ・設定配置を使用します。変更した場合は有効化前に `ExecStart` を編集してください。同じ状態領域で複数起動しないでください。ログインしていない間もユーザーサービスを継続するには、ホスト管理者によるlinger設定が必要な場合があります。例: `loginctl enable-linger USERNAME`。

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
