# Codex Hoshikage Gateway

[English — main documentation](README.md)

**自分のサーバーと作業フォルダーを使い、DiscordからCodexへ仕事を依頼できます。**

「このコードを調べて」「こんな資料を作って」。そんなときは、DiscordからCodexに話しかけてみましょう。**Codex Hoshikage Gatewayは、[Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy)を使って、DiscordとCodexをつなぎます。** パソコンでもスマートフォンでも、依頼から回答の確認、操作の承認、成果物の受け取りまで、いつものDiscordで進められます。

## こんなときに使えます

- 依頼のたびにターミナルを開かず、コード調査・修正・文書作成などをCodexへ頼めます。
- 1つのDiscordテキストチャンネルを1プロジェクトに対応させ、その中のスレッドで会話を分けられます。会話ごとの文脈を保てます。
- 実行中に追加指示を送り、必要なら中断を要求して待機中の依頼も一時停止できます。
- 次の依頼で使うモデルを選び、画像やテキストを添付し、指定した成果物ファイルを取得できます。
- Gatewayの再起動後も既存の会話に戻れます。実行結果が不明な依頼は確認待ちにし、自動で再実行しません。

初版は、**設定した1つのDiscordサーバーで、許可した本人だけが操作する用途**に対応します。Gatewayは自分でホストし、Codexへの接続と実行権限はProxyが担当します。GatewayはProxyの導入・サービス管理を行いません。依頼と回答はDiscordを経由するため、チャンネルの閲覧権限も管理してください。

## GatewayとProxy、2つで動きます

実行ファイル名は `codex-hoshikage-gateway` です。リポジトリのディレクトリで `cargo install --path . --locked` を実行するとインストールできます。`cargo install --path .` だけでも使えます。

使うのは、このGatewayと [Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy) の2つです。ProxyはCodexをOpenAI互換APIから使えるようにするソフトウェア。Gatewayはそこに、Discordのチャンネル・会話スレッド・コマンド・返信の仕組みを加えます。

```text
Discordのあなた ↔ Gateway ↔ Codex Hoshikage Proxy ↔ Codex
```

**Proxyは必須で、別途導入が必要です。** まずは[Proxyの導入手順](https://github.com/tanep3/codex-hoshikage-proxy/blob/main/docs/installation.ja.md)、続いて[Gatewayの導入手順](docs/installation.ja.md)へ進んでください。対応するProxyがすでに動いていれば、それを利用できます。

## まずは、こんな会話から

1. 登録したプロジェクトチャンネルで `/new title:不具合調査` を実行します。
2. 作成されたスレッドに「テストが失敗する原因を調べ、編集する前に説明して」と投稿します。
3. 回答を読み、そのまま相談を続けます。作成を依頼したファイルは `/get path:output/report.txt` で取得できます。

プロジェクトチャンネルへの通常投稿ではCodexは動きません。Gatewayが作成した会話スレッドから依頼します。

## 使ってみましょう

| 資料 | English | 日本語 |
| --- | --- | --- |
| サーバー・Bot・キー・設定・起動 | [Installation guide](docs/installation.md) | [導入手順書](docs/installation.ja.md) |
| 会話・コマンド・添付・復旧時の使い方 | [User manual](docs/user-manual.md) | [ユーザーマニュアル](docs/user-manual.ja.md) |

Ubuntuのホスト、対応する常駐Proxy、Botを追加できるDiscordアカウント、ソースからビルドするためのRustが必要です。導入手順書ではDiscord側の準備から説明しています。

## この版について、先にひとこと

**初期開発版（0.1.0）です。** DiscordとProxyを模した自動試験は通過していますが、実Discord・実Codexの受入試験と長時間の常駐確認は残っています。まずは検証用プロジェクトで試してみてください。本番での動作確認はこれからです。Botのコマンド説明・応答は現在日本語です。英語資料の提供はBot画面の英語対応を意味しません。

現在のProxy契約では、Gatewayのメモリから失われた回答本文を再取得できません。Codexの作業が完了していても、Discordへの回答が欠ける場合があります。欠けた回答を補うために作業を再実行することはありません。復旧時の扱いは[ユーザーマニュアル](docs/user-manual.ja.md)を参照してください。

開発者向けに[実装・検証状況](docs/implementation-status.ja.md)、[要件定義](docs/requirements.ja.md)、[システム設計](docs/system-design.ja.md)を用意しています。

## ライセンス

Copyright © 2026 **Tane Channel Technology**。[MIT License](LICENSE)で配布します。依存ソフトウェアのライセンスも適用されます。[依存ライセンス情報](packaging/dependencies.md)を参照してください。
