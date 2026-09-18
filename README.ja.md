# Codex Hoshikage Gateway

[English](README.md)

**外出先でも、DiscordからCodexに話しかけて作業を頼めます。** 調査やファイル・画像の作成を頼み、同じ場所で会話を続けられます。実行の許可、モデルの変更、作業の停止、止まった会話の復旧もDiscordから操作できます。

Gatewayは自分のLinuxマシンで動き、Codex App Serverを自分で起動します。**別のProxyサービス、ProxyのURL、Proxy APIキーは不要です。** テキストチャンネルやフォーラムの投稿ごとに、会話と作業フォルダーを自動で分けます。現在はDiscordサーバー1つと許可した利用者1人向けです。チャンネルを閲覧できる人には会話も見えるので、個人的な作業には非公開のチャンネルを使ってください。

Botは「本人の全投稿に応答」か「メンションした投稿にだけ応答」を選べます。生成した画像は会話に届き、ほかのファイルは `/get` で受け取れます。許可が必要な操作はDiscordで本人が判断します。結果が分からない作業を勝手にやり直すことはありません。

- [導入・設定手順](docs/installation.ja.md) · [English](docs/installation.md)
- [ユーザーマニュアル](docs/user-manual.ja.md) · [English](docs/user-manual.md)
- [運用・復旧ガイド](docs/operations.ja.md) · [English](docs/operations.md)

Rust製です。このリポジトリを取得したら `cargo install --path . --locked` でインストールできます。[Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy)は、共有HTTP APIが必要なアプリ向けの別製品です。Gatewayの利用には必要ありません。

[MITライセンス](LICENSE) · Copyright © 2026 Tane Channel Technology
