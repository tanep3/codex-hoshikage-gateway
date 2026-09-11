# Codex Hoshikage Gateway

[English — main documentation](README.md)

**自分のサーバーと作業フォルダーを使い、DiscordからCodexへ仕事を依頼できます。**

Codex Hoshikage Gatewayは、Discord Botと常駐中の `codex-hoshikage-proxy` をつなぐソフトウェアです。パソコンやスマートフォンのDiscordから依頼し、回答を確認し、承認要求に応答し、成果物を受け取るまでを一つの会話で行えます。

## できること・使うメリット

- 依頼のたびにターミナルを開かず、コード調査・修正・文書作成などをCodexへ頼めます。
- 1つのDiscordテキストチャンネルを1プロジェクトに対応させ、その中のスレッドで会話を分けられます。会話ごとの文脈を保てます。
- 実行中に追加指示を送り、必要なら中断を要求して待機中の依頼も一時停止できます。
- 次の依頼で使うモデルを選び、画像やテキストを添付し、指定した成果物ファイルを取得できます。
- Gatewayの再起動後も既存の会話に戻れます。実行結果が不明な依頼は確認待ちにし、自動で再実行しません。

初版は、**設定した1つのDiscordサーバーで、許可した本人だけが操作する用途**に対応します。Gatewayは自分でホストし、Codexへの接続と実行権限はProxyが担当します。GatewayはProxyの導入・サービス管理を行いません。依頼と回答はDiscordを経由するため、チャンネルの閲覧権限も管理してください。

## 利用イメージ

1. 登録したプロジェクトチャンネルで `/new title:不具合調査` を実行します。
2. 作成されたスレッドに「テストが失敗する原因を調べ、編集する前に説明して」と投稿します。
3. 回答を読み、そのまま相談を続けます。作成を依頼したファイルは `/get path:output/report.txt` で取得できます。

プロジェクトチャンネルへの通常投稿ではCodexは動きません。Gatewayが作成した会話スレッドから依頼します。

## はじめるには

| 資料 | English | 日本語 |
| --- | --- | --- |
| サーバー・Bot・キー・設定・起動 | [Installation guide](docs/installation.md) | [導入手順書](docs/installation.ja.md) |
| 会話・コマンド・添付・復旧時の使い方 | [User manual](docs/user-manual.md) | [ユーザーマニュアル](docs/user-manual.ja.md) |

Ubuntuのホスト、対応する常駐Proxy、Botを追加できるDiscordアカウント、ソースからビルドするためのRustが必要です。導入手順書ではDiscord側の準備から説明しています。

## 現在の状態

**初期開発版（0.1.0）です。** DiscordとProxyを模した自動試験は通過していますが、実Discord・実Codexの受入試験と長時間の常駐確認は残っています。管理された環境で評価する段階であり、本番動作確認済みのリリースではありません。Botのコマンド説明・応答は現在日本語です。英語資料の提供はBot画面の英語対応を意味しません。

現在のProxy契約では、Gatewayのメモリから失われた回答本文を再取得できません。Codexの作業が完了していても、Discordへの回答が欠ける場合があります。欠けた回答を補うために作業を再実行することはありません。復旧時の扱いは[ユーザーマニュアル](docs/user-manual.ja.md)を参照してください。

開発者向けに[実装・検証状況](docs/implementation-status.ja.md)、[要件定義](docs/requirements.ja.md)、[システム設計](docs/system-design.ja.md)を用意しています。

## ライセンス

Copyright © 2026 **Tane Channel Technology**。[MIT License](LICENSE)で配布します。依存ソフトウェアのライセンスも適用されます。[依存ライセンス情報](packaging/dependencies.md)を参照してください。
