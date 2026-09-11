# 運用ガイド

[English](operations.md) · [導入](installation.ja.md) · [使い方](user-manual.ja.md)

## 送信に失敗したとき

`/status` で配信の状態を確認できます。通信の一時障害は同じ保存版を照会します。Discordへの送信結果が不明な場合、勝手に別の投稿を作りません。

`/retry` で保存版を選ぶと確認ボタンが出ます。重複表示の可能性を確認して押すと、同じID・内容を再送します。元ファイルから最新版を取りたい場合は `/get path:...` です。保存期限切れ・破損・権限撤回は再送だけでは直りません。

## Proxyがバックアップから復元されたとき

通常の再起動では世代は変わりません。正式な復元で世代が変わるとGatewayは新規実行を止めます。Gatewayと同じOSユーザーで次を実行します。

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin proxy-inspect
```

照合件数、見つからない対象、旧世代・新世代を確認します。照合票は5分間有効です。Proxy自身の復元保留はProxy運用者が先に解除してください。Gatewayはその権限を持ちません。

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin proxy-accept --review-token REVIEW_TOKEN --reason "確認内容と判断理由" --accept-risk
```

再照合が一致した場合だけ新世代を受け入れます。会話のpauseと旧要求の再送禁止は残ります。UNKNOWNは成功や停止済みに変わりません。新しい依頼を始めるときは状態を確認し、旧待機が隔離されている会話は `/new` へ移ります。別instanceへの付替えはこの手順では行えません。

## 永久に解消しないUNKNOWN

まずProxy運用者が実行・残存プロセス・ワークを調査します。Proxyの `admin execution-hold inspect/release` は[Proxy運用手順](https://github.com/tanep3/codex-hoshikage-proxy/blob/main/docs/v2-operations.ja.md)に従います。

Proxyでの監査解除後、Gatewayの `admin status` から依頼IDとholdのgenerationを確認します。

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin abandon --request-id REQUEST_ID --generation HOLD_GENERATION --reason "Proxy側の解除と調査結果" --accept-risk
```

これは停止の代わりではありません。旧会話は隔離したまま、新しい会話の `/workspace` で明示的に共有先を選びます。遅れて実行中と判明した場合、Gatewayも占有を再取得します。

Gateway自身のバックアップ復元は `restore --from ...` と `admin recovery release` の別手順です。Proxy世代の受入だけでGatewayの復元保留を解除しません。全ディスクが一緒に巻き戻った場合まで、必ず検出できるとは保証しません。
