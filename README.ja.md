# Codex Hoshikage Gateway

[English](README.md)

**DiscordからCodexに話しかけて、作業を頼み、成果物を受け取ろう。**

[Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy)と組み合わせて使うDiscord Botです。ターミナルを開かずに、質問、作業依頼、モデル変更、承認や停止をDiscordから操作できます。

チャンネルやフォーラム投稿ごとに会話を続けられます。作業フォルダーの登録は不要。Proxyが会話のワークを用意します。応答は「全投稿」か「メンション時だけ」を設定できます。初版は、指定したDiscordサーバーの許可ユーザー1人用です。

API v2では、Proxyが成果物と確定回答の保存版を管理します。Gatewayは受け取るファイルの選択とDiscordへの配信を担当します。配信トラブルを理由にAIの作業を自動でやり直すことはありません。

対応Proxyで生成したPNGは、その会話へ自動添付します。画像を受け取るための `/get` 操作は不要です。

- [導入手順](docs/installation.ja.md)：Discordサーバー・Botの用意、キーの設定、インストール。
- [ユーザーマニュアル](docs/user-manual.ja.md)：会話・モデル・停止・ファイルの使い方。
- [実装・検証状況](docs/implementation-status.ja.md)：今使える経路と残作業。

**Proxy API v2が必要です。** 検証済みの範囲は[実装・検証状況](docs/implementation-status.ja.md)に記載しています。

Rustで実装し、`cargo install --path . --locked`でインストールできます。既存のCargoインストール先設定を使用します。Botの表示言語は現在日本語です。

[MIT License](LICENSE) — Copyright (c) 2026 Tane Channel Technology

[運用ガイド](docs/operations.ja.md)
