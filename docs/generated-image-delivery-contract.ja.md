# 生成画像の自動配信

更新: 2026-09-12
基準: [Proxy生成画像API契約](../../codex-hoshikage-proxy/docs/generated-image-api.ja.md)。以下は確定契約に合わせたGatewayの実装仕様。

## 利用者の操作と責務

普通に画像作成を依頼すれば、登録成功したPNGを依頼元のDiscordへ自動添付する。回答テキストが空、保存失敗、先に配信完了のいずれでも画像監視を継続する。画像が成功したときに /get、/new、cwd登録は要求しない。

Proxyが対象Turnから画像を発見・帰属確定・不変保存する。Gatewayはv2の生成画像一覧、成果物、リース、本体取得のみを使用し、Codexの保存先を探索しない。回答中のパス・Markdown、共有ワークの全ファイル、v1画像APIを自動送信の根拠にしない。

## 確定API

`features.generated_image_artifacts` と `features.response_generated_images` がともにtrueのとき有効。基本のcontract_versionは2.0のまま。追加機能未対応でも回答配信と手動 /get は維持する。

`GET /v2/codex/responses/{response_id}/generated-images` の全件スナップショットを使用する。Response・会話・workspace・instance・復元世代を保存記録と照合する。画像本体はartifact_idから取得する。

- pendingの空配列は画像なしではない。completeかつ空配列だけが画像なし確定。
- completeは登録判断の確定であり全件成功ではない。項目を追加できないが、Proxyが保存済みmanifestからfailed/unknownをreadyへ修復できる。
- image_id・ordinalは固定、readyのartifact_idは不変。項目消失、重複ID/ordinal、revision後退、同revisionの内容変更は拒否する。
- 全体unknownでも帰属確定済みready項目は配信できる。総件数の確定とは表示しない。
- 対応前のResponseに対する409 generated_images_not_trackedは追跡対象外として保持する。古い画像を探し直したり、AIを再実行したりしない。
- スナップショットのローカル上限は64項目・128 KiB。黙って切り捨てない。Proxyの既定は16項目、画像1件10 MiB、登録照合600秒。

## 監視・期限

Supervisorが独立した生成画像タスクを監視する。新規実行のSemaphoreは使わない。永続RequestにResponse IDが確定するとwatchを作成する。SSEの画像変更イベントは照会時刻を早める補助であり、通知がなくても定期照会する。

通常5秒、同じrevisionや照会不調は最大60秒までバックオフする。実行中のexpires_at=nullをエラーや画像なしと解釈しない。Gatewayが実行終了を最初に確認してから、Capabilityのgenerated_images_settle_secondsを使って自動監視期限を固定し、一覧のexpires_atが先ならそちらを使う。再起動で期限を延ばさない。

期限後はTIMED_OUT、410はEXPIRED、権限拒否はBLOCKED。空配列や配信成功へ置き換えない。管理用 `admin reconcile --request-id ...` は画像照合も再開する。これはGETによる再確認であり、AI実行・画像再生成・新しいcaptureは行わない。一覧が失効していれば復元できない。

## schema 5と重複防止

- generated_image_watches: request ID、送信先、Proxy帰属、revision/digest、メタデータのみのsnapshot、期限、バックオフ、監視状態。
- generated_image_items: image_id、ordinal、登録状態、artifact_id、配信ID、初回発見時刻。watch/image_idとwatch/ordinalに一意制約。
- artifact_delivery_claims: instance/generation/artifact_id/送信先を一意にして、手動取得と自動配信を同じ配信へ関連付ける。
- resource_deliveriesにimage_request_id・image_ordinalを追加し、既存の本体取得・有限リース・キャッシュ・配信処理を利用する。

snapshot検証、watch/item保存、claim取得、配信起票は一つのtransaction。同じ通知・GET・再起動で新たな自動配信を起票しない。/getが先に配信した場合もその配信IDを参照する。ハッシュ一致だけで異なる生成画像を統合しない。

/getは既に送信中・配信済みなら重複添付しない。再添付は /retry の確認画面から別の試行IDで行う。元の試行と未知結果は残し、未配信項目は新しい明示試行へ関連付ける。

## 取得・表示

1投稿1画像とし、元依頼への返信参照と「画像1」等で対応を示す。画像のordinalを優先し、既知の先行項目が未準備なら初回発見から最大30秒待機する。この待機時刻は永続化する。待機前にリースを確保する。未知の先行項目を後から発見する場合もあるため、完全な送信順序を保証するとは説明しない。

ダウンロード前と送信直前にDiscordの場所と、Botのロール・チャンネル上書きを計算した閲覧・添付・履歴・送信権限を再確認する。フォーラム投稿は親チャンネルの上書きとスレッド送信権限を使う。Proxy側は取得時にワーク権限を検証する。自動画像ではartifactのResponse/workspace/MIMEも照合し、サイズ・SHA-256・PNG形式・寸法・画素数・上限付きデコードを検証する。ファイル名に制御文字やパス区切りを許さない。送信のMIMEはimage/png。原本を無断で縮小・変換しない。

送信上限はGatewayのartifact_bytes（標準8 MiB）とBot送信経路の保守的な20 MiB上限の小さい方。常に1添付なのでリクエスト全体25 MiB以内に収める。Interactionのattachment_size_limitや個人Nitroの上限をBot投稿へ流用しない。Discordが容量・形式・権限で明示拒否した場合は失敗記録を残す。/getなら必ず送れるとは案内しない。HTTPの未知結果・5xxではPOST_PENDINGを維持し、自動で別投稿しない。

/stopは画像配信の取消ではない。失敗・中断が送信直前に確定していれば「作業途中の画像」と表示する。画像成功時には不要な完了投稿を足さない。一部失敗・登録unknown等は同じ状態投稿を更新し、実行失敗と画像配信失敗を区別する。

Discord資料: [公式Reference](https://docs.discord.com/developers/reference)、[公式Referenceソース](https://github.com/discord/discord-api-docs/blob/main/developers/reference.mdx)、[Create Message](https://docs.discord.com/developers/resources/message)、[Permissions](https://docs.discord.com/developers/topics/permissions)。古い10 MiB表記と現在の20 MiB表記を混同しない。

## 復元

Proxyの正式復元では既存の管理照合を先行し、画像一覧も照合対象にする。watchの元世代・配信IDは保持し、管理受入を別フィールドに記録する。世代変更で新しい自動配信を起票しない。画像IDやrevisionの照合ができない場合は保留する。既存のDiscord未知結果照合と有限リースを利用する。

## 検証

単体・結合試験は tests/generated_images.rs、実Discord試験は tests/live_proxy_v2.rs の明示実行専用 gateway_real_generated_image_delivery。実Codexで生成したPNGを常駐Proxyが自動登録し、Gatewayが許可先へ送信、Discordの添付を再取得してハッシュと画像デコードを確認した。

画像配信試験で使った生成依頼は一度だけ送信した。全失敗パターンを実Discordへ故意に起こす試験ではない。設定・稼働DBを手動で変更する必要はなく、初期化し直さない。
