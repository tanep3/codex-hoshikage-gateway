# 導入手順

[English](installation.md) · [ユーザーマニュアル](user-manual.ja.md)

この手順では、**DiscordでBotに話しかけるとCodexが答える状態**を作ります。画像やファイルを受け取ったり、必要な操作をDiscord上で許可したりできるようになります。

まず基本の会話を動かし、その後でバックグラウンド起動やMCPの許可機能を設定しましょう。MCPは、ブラウザー操作などの外部ツールをCodexから使う仕組みです。通常の会話を始めるだけなら、MCPの追加設定は後回しにできます。

最初の導入は手順1〜8を順番に進めます。手順9はバックグラウンド起動、手順10は困ったときの確認先、手順11はMCPの追加設定です。日常の使い方は[ユーザーマニュアル](user-manual.ja.md)へどうぞ。

## 作業を始める前に

このガイドはUbuntu/Linuxで、サービスを動かす本人のアカウントを使う手順です。基本操作にはrootを使いません。

| 作業する場所 | ここで行うこと |
| --- | --- |
| Discordアプリ／ブラウザー | サーバーを作る、Botを招待する、最後に話しかける |
| Discord Developer Portal | Botを作り、Bot用の接続キーを発行する |
| Gatewayを動かすLinuxのターミナル | Gatewayをインストールし、設定して起動する |
| Proxyを動かすマシン | Codexの実行環境と、必要ならMCPの許可機能を設定する |

GatewayはDiscordとのやり取りを担当し、[Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy)がCodexを実行します。2つは同じマシンでも別々のマシンでも構いません。設定ファイルも別々です。

用意するものは、Discordアカウント、Botを追加できるDiscordサーバー、API v2対応のProxy、Git、Rust/Cargo、Python 3.11以降です。このソースのビルドにはRust **1.98以降**が必要です。Pythonは以下の接続確認例に使います。

Linuxのターミナルで次を実行し、それぞれバージョンが表示されることを確認してください。

```sh
git --version
rustc --version
cargo --version
python3 --version
```

見つからないコマンドがあれば先に導入してください。Rust/Cargoは[Rust公式の導入案内](https://www.rust-lang.org/tools/install)を参照できます。Ubuntuでビルド用ツール等が足りない場合は、管理者権限のあるアカウントで次を実行します。

```sh
sudo apt update
sudo apt install git build-essential pkg-config libssl-dev python3 nano
```

以下のコマンド欄はLinuxのBashで実行します。先頭にプロンプトの記号を付けず、コード部分だけをコピーしてください。`$HOME` は実行ユーザーのホームフォルダーに置き換わります。一方、**GatewayのTOML設定内では `~` や `$HOME` を使わず、絶対パスを記入します。** `nano` で編集するときは、Ctrl+O、Enterで保存し、Ctrl+Xで終了します。

## 1. Proxyを用意する

[Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy)のAPI v2対応版が必要です。Codex認証・ワーク管理ルート・保存容量・権限はProxy側で設定します。Gatewayには接続URL、APIキー、利用モデルを設定し、cwdやProjectは登録しません。同居・別ホストともHTTP APIで成果物を取得します。別ホストはHTTPSを使用します。

Proxyがまだなければ、先に[Proxyの導入手順](https://github.com/tanep3/codex-hoshikage-proxy/blob/main/docs/installation.ja.md)を完了してください。管理者から「接続URL」「Proxy APIキー」「使えるモデル」を確認しておくと、この後の設定がスムーズです。Botトークンとは別のキーなので取り違えないでください。

## 2. Discordに作業場所を作りましょう

Discordのデスクトップ版またはブラウザー版で、サーバー一覧の **＋** から自分のサーバーを新規作成し、名前を付けて作成を完了します。通常のテキストチャンネルで使うなら、個人用サーバーで十分です。コミュニティ機能は不要です。[Discord公式のサーバー作成手順](https://support.discord.com/hc/en-us/articles/204849977-How-do-I-create-a-server)を参照してください。

通常の**テキストチャンネル**、または**フォーラム**を1つ用意しましょう。フォーラムは投稿ごとに会話を分けたいときに便利です。DiscordではCommunityを有効にしたサーバーで、チャンネル追加の「＋」から「フォーラム」を選んで作成します。詳しくは[公式フォーラムFAQ](https://support.discord.com/hc/en-us/articles/6208479917079-Forum-Channels-FAQ)をご覧ください。

専用サーバーを使うか、利用するチャンネルを本人とBotだけが閲覧できるように制限してください。Gatewayの許可ユーザー設定は操作を制限するもので、ほかの閲覧者から投稿を隠す機能ではありません。通常はそのチャンネル内で会話します。任意の `/new` は公開スレッドを追加します。どちらも本人だけが見える非公開会話ではありません。

## 3. Botを作って、投稿を読めるようにしましょう

[Discord Developer Portal](https://discord.com/developers/applications)で専用アプリケーションを新規作成し、名前を付けます。現在は新規アプリにBotユーザーが含まれています。**Bot** ページでトークンを再生成して取得し、手順6で保存します。[公式アプリ作成ガイド](https://docs.discord.com/developers/quick-start/getting-started)を参照してください。

「Botトークン」は、このBotとしてDiscordへ接続するためのパスワードに相当する秘密情報です。Application ID、Public Key、Client Secret、個人のDiscordパスワードとは別物です。Gatewayは起動時に設定サーバー内の当該アプリのコマンドを登録・置換するため、専用アプリを使ってください。

Intentは「どの情報をDiscordからBotへ渡すか」の設定です。Botページの特権Intent設定で **Message Content Intent** を有効にし、保存します。通常投稿の本文と添付を読むために必要です。この実装はプレゼンスやメンバー一覧のIntentを必要としません。Portalで承認申請が求められた場合は、Discordの手続きを完了してください。[公式のIntent説明](https://docs.discord.com/developers/events/gateway#privileged-intents)を参照してください。

このGateway用アプリのInteractions Endpoint URLは未設定のままにします。Webhook URLや、一般的なBot入門記事にあるHTTPサーバーの構築は不要です。

## 4. Botをサーバーへ招待しましょう

スコープは「何のためにアプリを導入するか」、権限は「Botが何をできるか」の設定です。アプリのインストール設定で、サーバー向けの **Guild Install** を有効にします。User Installは使用しません。Discord提供のインストールリンクを選び、サーバーへのインストール用スコープに `bot` と `applications.commands` を設定して保存します。[公式のアプリ導入説明](https://docs.discord.com/developers/resources/application)を参照してください。

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

以下は**初めて導入する場合**の手順です。すでに導入済みなら、既存のソースフォルダーから `cargo install --path . --locked` を実行し、設定例のコピーは飛ばしてください。既存configを上書きしません。

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

rootではなく、サービスを実行する本人のアカウントで作業します。

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

### インストール先を確認する

```sh
command -v codex-hoshikage-gateway
codex-hoshikage-gateway --version
```

1行目に実行ファイルの場所、2行目にバージョンが出れば準備できています。場所は控えておきましょう。手順9のsystemdで同じものを指定します。`command not found` なら、`cargo install` の最後に表示された配置先を確認し、そのフォルダーをPATHへ追加するか、実行ファイルを絶対パスで指定してください。`~/bin` を使っている人は、わざわざ `~/.cargo/bin` へ変える必要はありません。

キー入力の間、文字や「＊」が画面に出なくても正常です。貼り付け後にEnterを押してください。キーの中身を表示して確認する必要はありません。

## 7. 自分の環境に合わせて設定しましょう

手順6でコピーしたファイルを開きます。**既存のGatewayを更新している場合はコピーし直さず、今使っている設定を編集します。**

```sh
nano "$HOME/.config/codex-hoshikage-gateway/config.toml"
```

最初に、別のターミナルで次を実行して自分のホームフォルダーとユーザー番号を確認します。

```sh
printf 'ホームフォルダー: %s\n' "$HOME"
id -u
```

例えばホームが `/home/hanako`、番号が `1001` なら、サンプル中の `/home/tane` をすべて `/home/hanako` に、`/run/user/1000/` を `/run/user/1001/` に変更します。これは例です。表示された自分の値を使ってください。

| 設定項目 | 記入する内容・調べ方 |
| --- | --- |
| `default_model` | 最初に使うモデル。次の手順で一覧を取得し、表示されたIDをそのまま記入 |
| `discord.guild_id` | 手順5でコピーしたサーバーID |
| `discord.allowed_user_id` | 手順5でコピーした自分のユーザーID。BotのIDではない |
| `discord.response_mode` | `"all"` は本人の通常投稿すべてに応答。`"mention"` はBotをメンションした投稿だけに応答 |
| `discord.token_file` | 手順6で保存した `discord-token` ファイルの絶対パス |
| `proxy.base_url` | Proxyの接続先。Gatewayと同じマシンでポート4040なら `"http://127.0.0.1:4040"`。末尾に `/v1` や `/v2` は付けない |
| `proxy.api_key_file` | 手順6で保存した `proxy-key` ファイルの絶対パス |
| `proxy.contract_version` | `"2.0"` のままにする |
| `storage.state_dir` | Gatewayの会話・配信状態を保存する場所。ホーム部分を直して使う |
| `storage.temp_dir` | ファイルをDiscordへ渡すための一時置き場。Codexが作業する場所ではない |
| `storage.socket_path` | 管理コマンドとの連絡に使うローカルファイルの場所。ユーザー番号を直して使う |

別マシンのProxyへ接続するとき、`127.0.0.1` は使えません。それはGateway自身のマシンを指します。Proxy管理者からHTTPSの接続URLを教えてもらってください。LAN内でも、このGatewayは別ホストへの平文HTTP接続を受け付けません。

`[limits]` はまずサンプルの値をそのまま使ってください。例えば入力添付は1件8 MiB、1依頼4件が標準値です。自分で全部の上限を決め直す必要はありません。0は無制限の意味ではなく、設定エラーになります。

### 接続先とモデルを確認する

次のコマンドはGatewayの設定とキーファイルを読み、Proxyへモデル一覧を問い合わせます。APIキーそのものは表示しません。**AIへの依頼は送信しません。**

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

モデルIDが1行ずつ出れば問い合わせ成功です。利用するIDを1つ選び、config.tomlの先頭にある `default_model = "..."` の引用符内へ記入して保存します。一覧が空ならProxyのモデル設定を確認してください。HTTP 401/403はキーやアクセス許可、接続エラーはURL・Proxyの起動状態・証明書を確認する手掛かりになります。

## 8. まずターミナルで起動しましょう

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" check
```

「設定・認証ファイルのローカル検証に成功しました」と出たら次へ進みます。これはファイルの検証で、接続成功の意味ではありません。失敗したら手順6・7を見直してください。

次の初期化は新規導入時だけです。既存環境では飛ばしてください。

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" init
```

「状態DBを初期化しました」と出れば成功です。「初期化済み」ならDBを消さず、既存環境として扱います。続いて起動します。

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" run
```

Cargoのインストール先設定をそのまま使います。会話場所で挨拶し、モデル変更、停止、保存回答・成果物の再取得を検証します。Ctrl+Cで終了するまで、このターミナルで動作します。この段階では起動用シェルやsystemdの設定は不要です。

別ターミナルで次を実行すると接続状態を確認できます。

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin status
```

`connected: true` はDiscord接続、`proxy_ready: true` はProxyの基本機能が利用可能という意味です。MCPの機能ONとは別です。DiscordでBotがオンラインになったら「こんにちは」と送ってください。`mention` モードならBotをメンションします。返事が届けば基本の接続は成功です。`/new` や作業先登録は不要です。

コマンドはDiscord入力欄で `/` を打ち、このBotの候補から選びます。`/status` と `/models` も試してください。候補が出ない場合は手順10を確認します。

## 9. 必要ならバックグラウンドで動かしましょう

手動起動で確認できたらCtrl+Cを押し、Gatewayプロセスの終了を待ちます。リポジトリのディレクトリから次を実行します。

```sh
install -d "$HOME/.config/systemd/user"
install -m 644 packaging/codex-hoshikage-gateway.service "$HOME/.config/systemd/user/codex-hoshikage-gateway.service"
nano "$HOME/.config/systemd/user/codex-hoshikage-gateway.service"
```

手順6の `command -v` で確認した実行ファイルに `ExecStart=` を合わせてください。`%h` は実行ユーザーのホームです。`~/bin` を使う場合の例：

```ini
ExecStart=%h/bin/codex-hoshikage-gateway --config %h/.config/codex-hoshikage-gateway/config.toml run
```

保存して閉じたら有効化します。

```sh
systemctl --user daemon-reload
systemctl --user enable --now codex-hoshikage-gateway.service
systemctl --user status codex-hoshikage-gateway.service
journalctl --user -u codex-hoshikage-gateway.service -n 50 --no-pager
```

状態が `active (running)` なら起動しています。状態画面を閉じるには `q` を押します。`failed` なら、上の `journalctl` の最後に出るエラーを確認してください。ログを人に渡す場合は、キーや個人情報が含まれていないか先に確認します。

systemdはバイナリを直接起動するので、起動用シェルは不要です。サービス例は上記のバイナリ・設定配置を使用します。変更した場合は有効化前に `ExecStart` を編集してください。同じ状態領域で複数起動しないでください。ログインしていない間もユーザーサービスを継続するには、ホスト管理者によるlinger設定が必要な場合があります。例: `loginctl enable-linger USERNAME`。

停止は `systemctl --user stop codex-hoshikage-gateway.service`、再起動は `systemctl --user restart codex-hoshikage-gateway.service` です。Gatewayサービスを止めることはCodexの停止確認の代わりにはなりません。計画メンテナンス前は `/stop` で依頼の状態を確認してください。

## 10. 運用中に困ったら

設定、状態領域、`gateway-instance.json` を保持してください。初期化時の識別設定があるため、既存DBを別ユーザー・別サーバー・別Proxy URL・別状態領域へ単純に向け直すことはできません。起動エラーをSQLiteの直接編集や識別ファイルの削除で回避しないでください。

稼働中はサービス実行ユーザーがローカルで状態を確認し、整合したバックアップを作成できます。

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin status
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin backup --to /absolute/path/new-backup-bundle
```

保存先は本人だけがアクセスできる新しいパスに変更してください。稼働中のSQLite本体だけをコピーしないでください。このバックアップはGatewayの状態用で、プロジェクトのファイル・Discord履歴・キー・失われた回答本文は含みません。設定と識別ファイルは別途適切な権限で保管してください。復元時は古い依頼を再送せず隔離します。復元操作の前に[技術設計の復旧規則](system-design.ja.md)を確認してください。

| 症状 | 確認すること |
| --- | --- |
| `check` が失敗する | ID・パス・モデルの仮文字列、変更した上限値、絶対パス、キーファイルの所有者・権限 |
| Botがオフラインのまま | サービスログ、トークン、外向き通信、Intent設定 |
| スラッシュコマンドが出ない | アプリと導入先サーバー、コマンド用スコープ、本人のコマンド権限、Gateway起動成功 |
| 通常投稿に反応しない | 許可ユーザー、サーバーID、Botがそのチャンネルを閲覧できるか、応答モードとBotメンション、Message Content Intent |
| スレッド作成・返信ができない | カテゴリ／チャンネルの上書き権限、手順4の6権限 |
| Proxyが準備完了にならない | 常駐状態、APIキー、必須機能と契約。`check` の成功だけでは接続確認にならない |
| 添付が拒否される | 対応形式、設定上限、実際のファイル内容、Discordのアップロード上限 |
| 依頼が `UNKNOWN` | 状態確認とProxyとの照合。同じ依頼を再投稿して進めようとしない |

準備ができたら、[ユーザーマニュアル](user-manual.ja.md)へどうぞ。最初の依頼の仕方と、使えるコマンドをまとめています。

Discordの設定手順は **2026-09-16** にリンク先の公式資料で確認しました。表示名は言語やその後のUI更新で変わる場合があります。

[運用ガイド](operations.ja.md)

## 11. 追加機能：MCPの操作内容を確認し、この依頼中だけ許可する

基本の会話ができてから進めます。最初は試験用の環境で対象ツールの動作を確認してください。Proxy側の[検証記録](https://github.com/tanep3/codex-hoshikage-proxy/blob/main/docs/mcp-turn-approval-validation.ja.md)では、確認済みの範囲と未完了項目を区別しています。

**GatewayとProxyの更新だけでは、この機能はONになりません。** Proxyの管理者が、機能スイッチと対象ツールを設定します。Discordを使う方の設定変更や、Gateway側への同じスイッチの追加は不要です。

### 最初のカードに何を表示するかを確認する

対応版では、検索語やアクセス先URLを、操作の説明と許可ボタンと一緒に元のDiscord会話へ表示します。**その会話を読める人には操作対象も見えます。** 本人専用サーバーでも、招待した人やチャンネルの閲覧権限を確認してください。利用者にこの表示方針を説明し、了承を得た環境で有効にします。

認証情報・秘密入力・任意の実行コードは本人限定の補足画面で扱います。ただし、未知の秘密をすべて自動検出できるわけではありません。機密の検索語やURLを会話の閲覧者に見せられない運用では、有効にする前に表示方針を見直してください。

この表示方式のために新しい設定を足す必要はありません。以下の既存スイッチを使い、GatewayがProxyの対応状況を確認して切り替えます。対象はページ内検索・HTTP(S)ページ移動・タブ一覧です。未対応の操作は補足確認へ進みます。もともと確認なしで使えるツールに、新しい確認を増やす機能ではありません。

### 編集するファイルを探す（Proxy側のマシン）

別の人がProxyを管理している場合は、この節を管理者へ渡してください。自分で管理している場合は、まずProxyのターミナルでサービス名を探します。

```sh
systemctl --user list-unit-files '*hoshikage*'
```

`codex-hoshikage-proxy.service` があれば、次で起動設定を確認します。別名なら名前を置き換えてください。

```sh
systemctl --user cat codex-hoshikage-proxy.service
```

表示には秘密情報が含まれる可能性があるので、そのままDiscordへ貼らないでください。`Environment=`、`EnvironmentFile=`、起動シェルがあればそのファイルをローカルで確認します。設定ファイルは次の優先順位で決まります。

1. `CODEX_HOSHIKAGE_PROXY_CONFIG` があれば、そこに指定されたファイル。
2. それがなく `CODEX_HOSHIKAGE_PROXY_HOME` があれば、そのフォルダーの `config.toml`。
3. 両方なければ、Proxy実行ユーザーの `~/.config/codex-hoshikage-proxy/config.toml`。

ユーザーサービスがなければ `systemctl list-unit-files '*hoshikage*'` でシステムサービスも調べられます。コンテナならその起動設定を確認します。設定先が分からないまま新しいファイルを作っても、動いているProxyには反映されません。

編集前に設定をコピーして残しましょう。次は標準パスの例です。別のパスだった場合は両方置き換えてください。

```sh
cp -i "$HOME/.config/codex-hoshikage-proxy/config.toml" "$HOME/.config/codex-hoshikage-proxy/config.toml.before-mcp"
nano "$HOME/.config/codex-hoshikage-proxy/config.toml"
```

上書きを尋ねられて、以前の控えを残したい場合は `n` と答えてください。

| Proxyの設定項目 | 初期値 | 何を設定するか |
| --- | --- | --- |
| `v2.mcp_turn_approval_enabled` | `false` | `true` で操作詳細APIとターン限定許可を有効にする |
| `v2.mcp_turn_grant_tools` | `{}` | 評価済みのMCPサーバー名とツール名。空ならターン限定許可の対象はない |

設定するのは、**Proxyサービスが実際に読み込むconfig.toml**です。Gatewayのconfig.tomlやCodex自身の `~/.codex/config.toml` ではありません。既存の `[v2]` 内へ追記・変更してください。同じセクションを重複追加しないでください。

```toml
[v2]
mcp_turn_approval_enabled = true
mcp_turn_grant_tools = { playwright = ["browser_find"] }
```

これはProxy側の隔離試験で評価した組合せの例です。自分の接続先とツールの実装・可能な操作を確認して対象を選んでください。`true` にするだけで勝手に許可されることはなく、利用者が「この依頼中、このツールを許可」を選んで初めて許可が作成されます。`browser_evaluate` と `browser_run_code_unsafe` は対象一覧へ追加しても毎回個別確認です。

### 対象ツールが分からない場合

まず `mcp_turn_grant_tools = {}` のままで操作詳細だけを有効にできます。この状態ではターン限定許可ボタンは出ません。本人限定画面に出るサーバー名・ツール名を確認し、管理者がそのツールで可能な操作を調べてから対象へ追加します。例の `playwright` が未登録だったり `browser_find` が提供されていなければ、設定例をコピーしてもツールは使えるようになりません。ツール自体の導入はProxy/Codex側で行います。

### 保存してProxyを再起動する

切替中は新しい依頼を送らないでください。作業中なら `/stop` で停止し、`/status` で結果を確認します。状態不明の依頼は、再起動で解決したことにせず先に調査します。ほかの接続クライアントの利用者にも切替を知らせてください。

設定を保存したら、Proxy側で実行します。前に調べたサービス名が異なる場合は置き換えます。

```sh
systemctl --user restart codex-hoshikage-proxy.service
systemctl --user status codex-hoshikage-proxy.service
journalctl --user -u codex-hoshikage-proxy.service -n 30 --no-pager
```

`active (running)` と表示され、設定読込のエラーがなければ次へ進みます。失敗したらログに出た項目を直してください。元に戻す場合は控えの設定を戻し、Proxyを再起動します。システムサービスなら管理者が `sudo systemctl restart サービス名` など、その起動方法に合った操作を行います。ユーザーサービスを追加で立ち上げる必要はありません。

### 機能ONを確認する（Gateway側のマシン）

「Capability」はProxyが提供する機能の一覧です。次のコマンドはGatewayの設定とキーファイルを読み、必要な3項目だけを表示します。キーをコマンドへ直接貼り付ける必要はありません。

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


最初の2項目が `enabled= True profile= native-item-id-v1` なら、操作詳細と依頼中の許可がONです。3項目目の `mcp_inline_approval` が `enabled= True profile= source-conversation-v1` なら、元の会話に操作内容を表示するAPIも使えます。3項目目だけが `missing` なら旧版のProxyです。最初の2項目が使える場合は、本人限定画面での確認を継続できます。`False` なら読まれた設定と再起動を、`missing` ならProxyの対応版かを確認してください。HTTP 401/403はキー・アクセス許可、接続エラーはURL・Proxyの起動状態を確認します。

最後にDiscordで、確認が必要な対象ツールを使う新しい依頼を出します。最初のカードに操作内容・対象と許可ボタンが並んでいるかを確認し、単発許可と結果表示を確認してください。本人限定の補足が必要な操作では「本人限定で確認」へ進みます。複数回使う依頼ではターン限定許可を選び、`/mcp` の一覧・取消も確認します。次の依頼で再び確認されることは正常です。これで初めて、設定だけでなく利用者の操作まで確認したことになります。

操作詳細が出ない場合は実際のProxy設定と再起動を、ターン限定許可だけが出ない場合はサーバー名・ツール名と、その操作の `turn_grant_eligible`／`ineligible_reason` を確認してください。秘匿・取得不能などにより、対象ツールでも選択肢が出ない場合があります。

設定仕様と詳しい手順は、Proxyの[Discordで使うMCPの許可：利用者・管理者ガイド](https://github.com/tanep3/codex-hoshikage-proxy/blob/main/docs/mcp-turn-approval-guide.ja.md)を参照してください。利用者の操作は[ユーザーマニュアル](user-manual.ja.md)にまとめています。
