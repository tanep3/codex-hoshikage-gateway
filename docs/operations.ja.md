# 運用・復旧ガイド

[English](operations.md) · [導入手順](installation.ja.md) · [利用者向けの使い方](user-manual.ja.md)

Gatewayを設置した人向けの手順です。普段の会話、停止、結果不明の会話の復旧はDiscordでできます。まず[ユーザーマニュアル](user-manual.ja.md)の `/status`・`/cancel`・`/stop`・`/recover` を確認してください。

## サービスが動いているか確認する

```bash
systemctl --user status codex-hoshikage-gateway.service
journalctl --user -u codex-hoshikage-gateway.service -n 50 --no-pager
```

`active (running)` でなければ、まず `ExecStart` の**実行ファイルのパス**と `direct run` を確認します。設定を直した後は以下で再起動します。サービスを止めると実行中の作業は中断されるため、先にDiscordで `/status` を確認してください。

```bash
systemctl --user daemon-reload
systemctl --user restart codex-hoshikage-gateway.service
```

Botがオンラインでも返事がない場合は、Discordのサーバー・ユーザーID、Botの権限、Message Content Intent、`response_mode` を確認します。`mention` ならメンション付きで試します。Codexの認証はGateway専用のhomeで確認します。

```bash
CODEX_HOME="$HOME/.config/codex-hoshikage-gateway/codex-home" codex login status
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" direct check
```

`direct check` は外部接続を試しません。設定ファイルを編集しただけでは稼働中サービスへ反映されないため、必要な変更後は再起動してください。MCPツールもGateway専用の `codex-home` に設定します。

## 会話が結果不明で止まったら

本人がDiscordの同じ会話で `/recover` を開くのが通常の復旧方法です。画面に、旧作業を再実行せず記録を残すこと、未送信の待機依頼を取り消すこと、新しいCodex文脈から再開することが表示されます。本人が確認ボタンを押すまでは解除しません。作業ファイルとDiscord履歴は残ります。

まだ実行中なら `/recover` は解除しません。`/status` で確認し、必要なら `/stop` を使ってください。`/resume` は一時停止を解除するだけで、結果不明を解消しません。

Discordでの復旧が何度も失敗するときだけ、運用者はサービス停止後に次の一覧を確認できます。

```bash
systemctl --user stop codex-hoshikage-gateway.service
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" direct holds
```

`direct abandon` は最終手段です。Codexの残存プロセスと対象の作業フォルダーを確認したうえで、一覧の `request_id` と `generation` を指定し、**必ず別の新しいバックアップ先**を `--backup-to` に指定します。旧依頼を成功扱いにせず占有だけ解除し、新しい文脈で次の依頼を受けます。元の作業がまだ動いている可能性があれば実行しないでください。

```bash
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" direct abandon --request-id REQUEST_ID --generation GENERATION --reason "調査結果と解除理由" --backup-to /安全な場所/新しいバックアップ名 --accept-risk
systemctl --user start codex-hoshikage-gateway.service
```

## 設定変更とバックアップ

設定の正本は `~/.config/codex-hoshikage-gateway/config.toml`、Botトークンは別の `discord-token`、Codexの認証・MCP設定は専用 `codex-home` です。`state_dir` には会話や配信の記録があります。`workspace_root` にある作業ファイルも別に保存が必要です。どちらか一方だけをバックアップしても、会話とファイルを一緒に戻せません。

バックアップ時はGatewayを停止し、**設定、トークン、codex-home、state_dir、workspace_root** を所有者だけが読める保存先へまとめてコピーします。秘密情報が含まれるため公開ストレージやGitには置かないでください。復元では、古い実行記録を単純に戻すと「送信済みの作業を未送信」と誤認する可能性があります。バックアップを戻す前に運用状況を確認し、SQLiteや識別ファイルを手作業で書き換えないでください。

`workspace_root` の設定を変えても、**既存会話の保存先は移動しません**。新しい会話から新しい親フォルダーを使います。`state_dir` やBotのサーバー・利用者IDを既存DBのまま別物へ付け替えないでください。初期化コマンド `direct init` を復旧目的で繰り返し実行しないでください。
