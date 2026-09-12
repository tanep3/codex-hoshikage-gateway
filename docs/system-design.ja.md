# Codex Hoshikage Gateway システム設計書

版: 1.3 / 2026-09-12 / Tane Channel Technology / MIT
基準: [要件2.3](requirements.ja.md)、[Proxy API v2 契約案0.2](../../codex-hoshikage-proxy/docs/workspace-artifact-api-v2.ja.md)。設計は実装・配備済みの宣言ではない。

## 1. 構成と境界

Rust単一プロセスにDiscord adapter、受付検証、SQLite Store、Scheduler、v2 Proxy adapter、Control、監視、成果物／回答取得、Discord Delivery、Supervisor、管理socketを置く。App ServerはProxyが所有する。Proxy内部crateやホストファイルへの依存は持たない。

旧設計のProject/cwd登録・全体1実行・v1 SSE所有・成功Response継続を本版で置換する。既存テーブルの移行前データは監査・復旧用に保持するが、新規実行のワーク決定に使わない。

## 2. IdentityとDB

| 対象 | 永続化する識別子と情報 |
| --- | --- |
| Discord会話 | 場所ID、選択モデル・revision、pause・revision、受付順序 |
| Proxy binding | instance_id、recovery_generation、検証／隔離状態 |
| Proxy会話対応 | ローカル会話、作成キー、操作ID、conversation_id、workspace_id、状態、初期モデル／共有選択の指紋 |
| 実行 | Message ID一意、request UUID、元要求キー、Proxy会話、operation/response/turn、入力digest、開始境界、停止意思 |
| 制御 | 操作ID、対象と期待revision、送信境界、受付／結果不明 |
| 成果物配信 | 取得操作キー、artifact_id、版・サイズ・ハッシュ、lease_id・期限、送信先、状態 |
| 回答配信 | Response ID、正本のサイズ・ハッシュ、lease_id・期限、Discord各部分の送信状態 |
| Discord配信 | intent、message ID、確認済みdigest、pending digest、結果不明 |

本文・回答・ファイル本体をGateway DBへ保存しない。元投稿はDiscord、確定出力はProxyを正本にする。作成キーと副作用の送信意思はHTTPより先にcommitする。HTTP中にDB transactionを保持しない。識別子をURLへ入れる前に検証する。

schema migrationは既知版のみ1 migration/transactionで適用し、checksum検証・失敗rollback・新しい未知版の書込拒否を行う。受付はmigration後。旧要求のキー・履歴は残し、送信済みと旧送信待ちをv2へ自動転用しない。旧会話は明示移行前に隔離する。

## 3. Proxy adapterと世代

v2操作は `/v2/codex`、認証Bearer、instanceとgenerationヘッダーを使う。最初のbindingはCapabilityを確認して永続化してから公開する。再起動・再接続後は保存bindingと比較し、不一致なら新規受付を止める。接続先変更を新しいbindingとして自動承認しない。

Capabilityは起動・再接続・定期・SENDING直前に検証する。必須機能欠落、recovery_state非readyは送信許可を発行しない。短命の1回限り許可票をDBの設定revisionと照合する。GET→POST間は原子的ではないためProxy側の世代／受付検証も必須。

HTTPはredirect・透過retryを無効、制御と大容量本体は別client／処理枠。JSONとSSEに容量・時間上限。非2xxは機械可読codeとretry.actionだけを解釈し、生エラーは表示しない。操作404を未送信の証明にしない。v1へfallbackしない。

## 4. 会話作成とScheduler

最初の有効投稿に対してローカル会話を作成する。モデルが一意に決まらなければ選択UIを使う。Proxy会話の作成キーを永続化→1回送信→操作／会話照会→readyな対応をcommitする。作成応答喪失はキーで照会し、別キーでワークを作り直さない。実行要求はこのconversation_idを参照する。

受付はMessage ID重複排除、VALIDATING予約、本文・添付検証、QUEUED確定の順。検証期限は独立sweeperが回収し、workerの後着結果は状態/versionで拒否。同一会話は先行予約もFIFO障壁とする。

Schedulerは全体2件、同一会話1件をDB transactionで取得する。Proxyのworkspace_idが同じ既知会話も待機し、包含関係とv1他クライアント競合はProxyに任せる。実行枠取得前に入力をDiscordから再取得・digest照合し、送信前にSENDINGと一意キーをcommitする。モデルはその境界で固定する。

202のresource IDを保存する。Response状態を照会し、accepted、dispatching、startedを区別する。Gatewayのネットワーク切断でProxyを中断しない。受理済み依頼がProxy側で進むこととGatewayの再送を混同しない。Gatewayは送信結果不明な実行を自動再POSTしない。

## 5. 状態機械

`RECEIVED → QUEUED → SENDING → RUNNING → COMPLETED / FAILED / CANCELLED`。
`RUNNING ↔ APPROVAL_REQUIRED`、送信境界以降の停止は`CANCEL_REQUESTED`、判定不能は`UNKNOWN`。SENDING中のProxy accepted/not_startedは開始確認前として維持する。

Proxy rejectedは開始拒否、cancelled/not_startedは開始前取消、interruptedは開始後中断。実行結果と保存・配信結果は別に扱う。UNKNOWNは自動再実行せず、正しい対象の現在照会でのみ訂正する。管理解除は実行結果を書き換えない。

初回の確定失敗・中断後でもProxy会話がreadyなら同じ会話で続ける。会話unavailable/recovery_blockedは明示新規会話・管理確認を案内する。後着通知が新しい会話・モデルrevisionを上書きしないよう、対象とsequenceを照合する。

## 6. 停止と制御

`/stop`は会話pauseと停止対象キーを先にcommitする。Proxy会話ID＋元要求キーで `/stops` を呼び、停止操作キーも永続化する。Turn ID待ちにしない。送信結果不明の停止は同じキーで照会し、同じ不変要求を使う再試行だけを許可する。Gateway再起動後も停止意思を処理する。

取消予約・開始中・interrupt_pending・unknown・already_terminalをResponse状態と併せて解釈する。pause解除は既存の取消を撤回しない。停止対象の次のTurnへ転送しない。

Steer・承認はv2対象のTurnを既存v1制御APIに渡すが、instance/generationヘッダーも付ける。期待Turn／Thread／承認期限を照合し、v1の非冪等な操作を無条件再送しない。制御操作は実行枠・コピー枠に依存しない。

## 7. SSEと確定回答

`/responses/{id}/events`を独立監視し、snapshot、delta、gap、terminal、output_ready/failedを処理する。ID不一致やgap後のdeltaは最終正本にしない。監視が落ちてもResponse照会を継続し、対象をRUNNINGのまま放置しない。

output_ready後は有限リースを取得し、保存JSONを長さ・ハッシュで検証する。output配列の回答テキストを取り出し、秘密を除去してDiscordの文字数に合わせて分割する。暫定表示から確定表示へ更新する。再起動・キャッシュ消失時も同じResponseの正本を取得する。output unavailable/failed/expired/corruptを明示し、AIを再実行しない。

schema 3は期限付き `selection_menus` と配信照会の `attempts` / `next_attempt_at` を追加する。schema 1→2は旧実行を隔離し、2→3は既存v2待機を維持する。各migrationは独立transactionとchecksum検証を行う。

## 8. 成果物UI・配信

`/get`は会話の登録済み一覧をページング表示し、選択値をProxy artifact_idへ照合する。別会話・別ユーザー・旧世代のコンポーネントを拒否する。共有一覧は明示操作。`/get path:`は保存操作を先に作り、操作キー・予約IDをDBへ保存して照会する。作成中や失敗を配信可能と表示しない。

選択メニューはランダムなtokenに会話・kind・scope・binding・候補・opaque cursor・10分の期限を保存する。Discordへは候補の添字だけを渡し、クリック時に保存候補と現在の認可を再照合する。cursorはURLクエリとしてエンコードする。期限切れを掃除し、同時有効メニューは1000件までとする。共有選択は会話作成行へのINSERTで自動割当と排他し、既存の対応をUPDATEしない。

`/new` はスレッド内なら親チャンネルを取得し、Guildと種別を再検証する。フォーラムには最初のメッセージを含め、mentionを無効化する。タグ必須の場合は作成POST前にDiscordの標準投稿UIへ案内する。仕様根拠: [Discord公式 Channels API](https://docs.discord.com/developers/resources/channel#start-thread-in-forum-or-media-channel)。

配信はDBの待機記録→有限リース→本体取得→検証済み私有temp→送信意思commit→Discord送信→結果commit。リース期限・ハッシュ・IDを保持し、元パスへfallbackしない。再送と新版取得をUIで分ける。Range再開も同じID・ETagに限定する。

保持監視は独立したcritical taskとして15秒周期で動作し、ダウンロード待ちの影響を受けない。Proxyのserver_timeとleaseのmax_hold_untilを使い、残期間が延長窓の半分以下なら延長する。延長窓はdelivery_retention_secsを60秒〜1日に収めた値とし、最大期限を超えない。元hold_untilに対応する操作キーと本文を永続化し、不明時は新しい期限のPOSTを作らず照会する。延長後のGETで期限を確認する。解放もGETでreleased/expiredを確認してから配信完了扱いにする。配信取得の失敗は永続backoffで最大5分間隔に抑え、成功した照会も次回時刻を更新して他の待機を飢餓状態にしない。

期限切れ、延長拒否、容量不足、権限撤回、GC・切断を通常のエラー経路として扱う。確認済み配信のキャッシュは削除し、リースを解放する。Proxy原本は削除しない。Discord POST/PATCHの結果不明は保存したmessage IDや照合情報で解決し、別投稿で埋め合わせない。再配信直前もDiscord認可を検証する。

schema 4は配信の`retry_of`・`shared_workspace`と管理照合票を追加する。`/retry`は10分の選択／確認画面を使う。再送元をSUPERSEDEDにして新しい配信IDを作成するtransactionと、配信側の読取ロックを組み合わせる。元のPOST/PATCH結果と監査記録は消さない。本文の正本はProxyのまま。期限切れEXPIRED、保存失敗FAILED、権限撤回BLOCKEDと通常の通信待機を区別する。

成果物・回答の取得は最大4並列、共有容量予約内で実施する。転送が途中で切れた場合はメモリ内の検証前prefixを保持し、最大4回・全体300秒以内でRange再開する。再起動でprefixが消えた場合は同じ保存版を先頭から取得する。HTTP 206・Content-Range・ETag・長さと最終ハッシュを照合し、変化した版を継ぎ足さない。初回GETの失敗をAI実行や原本captureで埋め合わせない。

起動時はインスタンスロック取得後に、専用temp内の`delivery-UUID`形式・実行UID所有・通常ファイル・単一リンクの孤立キャッシュだけを回収する。symlinkや利用者の別名ファイルは触らない。確定回答が暫定表示より短い場合、余剰パートの送信結果を照合してから、保存した自分のmessage IDだけを削除する。DELETEの結果不明は同じIDの再照会で扱う。

## 9. 管理・Supervisor・復元

DB worker、Discord接続、Scheduler、Capability、Control、監視・配信sweeperをSupervisorで監視する。critical taskのpanic／予期せぬreturn／channel断は受付停止・プロセス異常終了とし、systemdで再起動する。個別workerは対応を照会し、不明ならUNKNOWNとする。

設定reloadは全体検証・世代単位で適用する。応答モード・容量・既定モデルは新規操作に適用。Proxy接続・認証変更はCapability失効・保存binding照合。Discord token・Guild・temp・状態ディレクトリは再起動を必要とし、初期化identityやstate_dirは管理移行なしに変更しない。

正式backupはSQLite整合バックアップとschema/version/instance/time/checksumのmanifestを使う。restoreはDB外マーカー・隔離・監査付きrelease。Proxy復元世代の変更も新規受付を止め、未完了操作・回答・成果物・leaseを照合する。世代の自動更新で復旧済みと扱わない。

Proxy UNKNOWN占有の解除はProxy運用者の管理CLIへ案内する。Gateway admin abandonはProxy占有を解除できない。管理解除時に旧会話を停止・新会話必須とし、元要求を再送不可にする。解除済みUNKNOWNも古い照会順に監視対象へ残し、遅着RUNNINGでholdを再取得する。旧会話隔離・明示再利用・遅着実行再検出を照会し、既存の別要求を勝手に停止しない。

管理照合票は現在binding・対象ID一覧・サイズ／ハッシュ・遠隔の機械状態だけをDBへ保存する。本文は保存しない。最大8並列でGET照会し、転送エラーを欠損と解釈しない。accept時に期限・ローカル対象一覧・遠隔世代を再検証し、transactionでbinding更新・旧要求の再送禁止・会話pause・監査を確定する。404の旧IDを再採番しない。運用手順は[Operations](operations.md)／[日本語](operations.ja.md)。

v1の実行開始・SSE所有・継続判定クライアントは削除した。v1 HTTPはモデル一覧とv2契約で指定されたTurn制御だけに残す。旧Project型・テーブルは設定の既定モデル継承とDBの参照整合／監査のため保持し、cwdは新しい実行先に使わない。旧会話は新スレッド／投稿へ明示移行する。

## 10. 実装と検証の順序

1. v2 client、binding・会話・操作の永続化、schema migration、Fake Proxy契約試験。
2. 実行・停止・監視・モデル・承認のv2接続、競合とクラッシュ試験。
3. 成果物／回答リース、取得・選択・配信復旧、容量・期限・権限試験。
4. 旧設定・DB移行、英日資料、全テスト・clippy・fmt。
5. Proxy実装版との結合、実モデル登録、別ホスト・Discord障害試験、標準値の実測、配備。

Proxy側の実装完了前でもFake Proxyで独立して実装できる。実際のイベント形状やエラー契約と相違が判明した場合は契約側と調整する。Fake成功だけで本番導入可能とはしない。進捗・制約は[実装状況](implementation-status.ja.md)に記録する。

受付transaction内で、Proxy会話記録なし・過去RequestがCOMPLETED/FAILED/CANCELLEDのみ・有効holdなしを確認し、継続不可のローカル会話をNEWへ戻す。Response/Thread参照は外し、モデル・Discordの場所・依頼履歴・Message ID重複防止を保持する。直近の適用済み停止操作が未再開ならpauseを保持する。予約失敗・重複投稿では初期化を確定しない。

## 生成画像の自動配信設計

[確定した生成画像配信仕様](generated-image-delivery-contract.ja.md)を適用する。通常の画像作成依頼に対し、Proxyが帰属を確定して登録したPNGを依頼元へ自動添付する。回答本文とは独立した監視・配信状態を持ち、schema 5のwatch/item/claimで永続化・重複抑止する。一般成果物の無差別送信は許可しない。/getは必須ではなく、明示再添付は /retry で確認する。

### 2026-09-13 操作・進捗表示の補正

実行要求送信中は依頼ID別のRAIIガードを保持し、通常監視の受理照会と競合させない。異常終了・キャンセル時はガードを解放し、従来の照会・UNKNOWN保護を継続する。画像のpendingが実行終了確認から10秒続く、または未配信画像を発見したとき、既存notice IDを更新して準備状況を表示する。画像なし確定・配信完了・失敗で同じ案内を更新する。UNKNOWN解消後は所有確認済みの警告投稿を削除し、正常復帰の投稿はしない。削除記録は監査用に保持する。
