# Codex Hoshikage Gateway システム設計書

> 2026-09-17 責務再整理を適用。[後継Gateway内部設計](mcp-approval-layered-gateway-design.ja.md)を現行の設計方針とする。汎用表示・単発承認と明示ポリシーを分離し、全342ツールの意味評価を汎用機能の着手条件から外す。API 0.6第12〜13節まで接続合意済み。具体的な型・DB・処理順序は[0.6実装詳細](mcp-approval-v06-implementation.ja.md)を正本とする。旧0.5のwireを黙って変更せず、以下の旧版固有部分は互換仕様・履歴として扱う。

版: 1.10 / 2026-09-17 / Tane Channel Technology / MIT
基準: [要件2.10](requirements.ja.md)、[Proxy API v2 契約案0.2](../../codex-hoshikage-proxy/docs/workspace-artifact-api-v2.ja.md)。設計は実装・配備済みの宣言ではない。

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

### 6.1 `/cancel` の対象確定と永続化

Storeの優先制御キューでtransactionを開始し、operations.interaction_idで重複を検出する。同じ操作なら保存済み対象・判定を返し、再選択しない。現在会話のadmissionsをsequence降順に検索し、VALIDATINGまたは未送信RECEIVED/QUEUEDの最新1件を選ぶ。VALIDATINGはREJECTED・version増加・user_cancel_before_send、確定済み待機はCANCELLED・dispatch_eligible=0・イベント記録とする。finalizeは既存status検証、begin_sendは既存state検証で後着処理を拒否する。

未送信がなければ未解放holdの対象にstop_requestedを保存する。対象なしも含めoperations(kind=cancel, decision=waiting/active/empty, target_request_id)を同じtransactionで保存する。スキーマ追加は不要。pauseとpause_revisionは変更しない。

Discordの取消は実行枠の外側の優先制御として扱い、worker待ちの前に取消意思を保存する。処理workerは同じ操作IDから結果を取得し、activeの場合だけ既存interruptへ接続する。再起動時は既存stop_requested監視で継続する。送信境界以降は単にCANCELLEDへ書き換えず、Proxyの確定結果を照合する。取消操作が再送されても別のTurnへ転送しない。

2026-09-17 検証: 取消の重複・検証中取消・送信境界競合・UNKNOWN保持と再起動・Discord制御経路・完了履歴45件後の監視を自動試験で確認。全134件成功、実環境用6件は未実行。Clippy（警告をエラー扱い）、format、差分検査も通過。当時は常駐未反映。同日20:04 JSTに常駐反映し、実Proxy＋模擬Discordのcancel試験と、常駐DB上の実Discord cancel受付・中断完了も確認。[統合記録](mcp-approval-v06-implementation.ja.md)を参照。

## 7. SSEと確定回答

状態監視は未終端の実行だけを選ぶ。会話がVERIFYINGになっても過去のCOMPLETEDは監視対象に戻さない。会話状態は複数依頼で共有されるため、過去の完了記録が監視batch上限を占有して現行UNKNOWNを飢餓状態にしないことを受入条件とする。v2はResponseと会話の照合後に終端状態を保存し、確定回答・画像・配信の復旧は別監視を継続する。

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

### 2026-09-15 承認・最終回答・処理中表示

承認発見処理はDB登録のみを行い、単一のapproval_uiタスクが3秒間隔で内容・期限・判断状況を再照会して同一カードを更新する。Proxyへの決定再送や会話作成は行わない。確定回答はanswer、途中表示はdraftの別配信キーとし、answer確認後に所有確認・送信結果照合を通してdraftを削除する。7秒間隔の独立typingタスクはSENDING/RUNNINGかつ承認待ちでない会話だけを対象とし、API失敗を実行失敗にせず、3秒で打ち切る。未対応の承認詳細を読めない場合は承認ボタンを出さない。

Typing APIの表示寿命は10秒。仕様根拠: [Discord公式 Channels Resource](https://docs.discord.com/developers/resources/channel#trigger-typing-indicator)。

承認コンポーネントはDiscordのDEFERRED_UPDATE_MESSAGE（type 6）で受け付け、承認UIタスクだけが元カードを更新する。正常時はinteraction返信を追加しない。失敗・タイムアウト時のみephemeral followupを送り、元カードを直接上書きしない。

## MCPフォーム設計（2026-09-15追加）

実行枠外のSupervisorタスクが1秒周期でResponseのinteractionsを照会する。mcp_interactions（schema 6）はProxy instance/世代/接続先、Response/会話/ワーク、interaction ID/revision、要求digest、期限、送信意思・操作キー・結果を保持する。要求本文・回答本文は保存しない。Bot再起動後は元カードを更新し、入力途中の回答だけは再入力とする。送信意思のある回答はoperationとinteractionの照会のみで復旧し、POSTを再送しない。

承認文面は長文でも分割して省略せず表示し、すべての本文配信が確認できてから操作ボタンを出す。空フォームは今回許可/拒否。非空フォームは本人専用の項目一覧から入力し、最後に明示送信する。最大32項目をページ分けし、enumは最大64候補を番号で選択できる。長いstringは最大3入力欄に分けて結合し、8192 bytesとUnicode文字数の制約を検証する。defaultsは説明だけで自動採用しない。入力途中の値は期限付きメモリにのみ保持する。

Discordのmodalは初回応答で開き、その他の入力操作はephemeral、空フォームの許可・拒否はdeferred updateで元カードのみ更新する。公式仕様: https://docs.discord.com/developers/interactions/receiving-and-responding 。実Discordの結合受入は利用者による許可/拒否操作と実MCPの結果照合を別途必要とする。

Discord受信境界ではSerenityのInteraction列挙型から取得したkindをJSONのtypeへ明示的に保存する。Serenityの再シリアライズだけではtypeが落ちるため、生のDiscord JSONを直接渡すモックだけでは検証しない。ボタンとmodalの実ライブラリ往復を回帰試験に含める。MCP監視はschema 6導入以降の依頼または既存MCP記録がある依頼を対象とし、照会失敗は30秒間隔へ抑える。

## リソース配信エラーの案内

resource-error noticeは初回文面を固定せず、resource_deliveriesと回答のdeliveriesを再照会して表示する。POST_PENDING/PATCH_PENDINGでは取得障害より送信結果不明の案内を優先し、重複の可能性を確認する /retry を案内する。DELIVERED/RELEASE_PENDING/SUPERSEDEDではnoticeの所有確認・結果照合を経て削除し、監査記録を保持する。照合不能な投稿は勝手に再投稿・削除しない。

承認UIはファイル対象 `paths: string[]` を解釈する。対象一覧が空または不正、あるいは `grantRoot` による追加範囲を解釈できない場合は承認ボタンを出さない。ボタンと説明には同一の承認可否判定を使う。

## MCPターン限定許可の設計境界（2026-09-16）

[双方合意の調整案](proxy-mcp-turn-approval-proposal.ja.md)を適用する。既存requests.idをGatewayの1依頼識別子としてDiscord本人/会話とProxy Response/Turnへ照合する。許可の状態機械・API・依頼内容世代・許可fingerprint・TTLは双方合意のAPI 0.3に従う。Gatewayにローカル自動acceptは追加しない。SteerはProxyによる許可失効受付→依頼内容世代更新→送信の境界を必要とし、Gatewayのローカル失効だけでは保証したと扱わない。

既存APIとの互換範囲：確認原文と取得情報の限界を区別する表示、今回だけ許可の明示、正常に解消した確認カードの集約。mcp_interactionsのstate=resolved・action=accept・operation_state=succeededをすべて満たす場合だけ、requests.id単位のmcp_summary配信へ件数をまとめる。これはacceptされた承認またはフォーム回答の記録であり、ツール成功の表示ではない。個別mcp_actionからボタンを外し、summaryの配信を確認してからmcp_description/mcp_actionを既存Deliveryの所有確認・結果照合付き削除で除去する。途中失敗・再起動は同じ配信キーで回復する。拒否・期限切れ・不明は個別表示を保持する。本文や引数をSQLiteへ追加保存しない。

常駐移行前の実証Gate：信頼できる操作情報の取得、run/Turn/依頼内容世代照合、許可作成/取消のat-most-once送信と照会復旧、利用者別UI認可、並列/停止/Steer/設定変更/再起動試験。実証未完了の常駐Proxyでは機能を無効に保つ。Gatewayのターン限定ボタンは対応Capabilityと操作単位の適格性の双方を満たす検証環境で受け入れる。

MCPカードの集約完了は本文・操作カードの削除確認まで閉じない。UNKNOWNから後日解消した場合も整理開始時にclosed=0へ戻し、作業終了時の期限監視は確認済みresolvedの整理を優先する。全削除完了前にinteraction_scan_doneへ進めない。Deliveryの送信結果照合は保存済みpending_digestの内容だけを確認し、呼出し側の最新digestと一致しなければ送信完了を返さない。

合意API 0.3の検証手順と必要項目は [MCPターン限定許可の受入表](mcp-turn-approval-acceptance.ja.md) に整理する。

### Proxy合意契約0.3の反映

### 0.4互換Responseの承認カード（0.5は次節の詳細設計へ置換）

1. Proxy adapterが公開用表示を取得し、interaction・Response/Turn・実行範囲・表示の版を検証する。本人限定operation.argumentsとは別の型で扱い、公開カードrendererへ生引数を渡さない。公開APIは合意0.4のGET presentation、追加Capability、approval_presentationと返信用の表示トークンに対応する。具体境界は [接続レビュー](mcp-inline-approval-api-review.ja.md) に記録し、検索語・アクセス先URL等の操作対象を元会話へ表示することは利用者承認済み。認証情報・秘密入力・任意コードを除外した3種類のrendererとツール別規則を0.4のR-01対応版で確認し、Gateway接続レビューを完了した。
2. Rendererは公開用の操作・対象・制限、許可範囲の説明、選択肢を同じカードに組み立てる。直接承認可能で表示が省略なしに収まる場合だけ許可ボタンを付ける。ターン限定適格性と公開可能性は別条件。長文・秘匿・未知形式は理由と補足確認／拒否へ切り替える。本文だけ先に送り後から許可ボタンを別投稿しない。
3. 表示記録にはローカルID・対象interaction・revision・公開表示に結び付く不透明トークン・scope/本文とcomponentsのdigest・期限・送信先を保持する。公開文章や本人限定実引数の本文はDBへ保存しない。期限は元interaction/call以内かつ最大10分。表示が変われば旧版を無効化し、トークンをcustom_idや本文へ出さない。具体schemaは以下の詳細設計で定義する。
4. 既存DeliveryのPOST/PATCH送信結果照合を使い、同じ表示digestの配信成功を確認した場合だけ、その版の押下を受け付ける。送信結果不明を新しいカードで補わない。再起動後も配信記録とProxyの現表示を照合してから再開する。
5. 押下時はGuild・本人・元会話・カードのmessage ID・保存した版を確認し、Proxyへ表示を再照会する。公開説明・省略状態・直接承認可否・トークンが一致した場合だけ、単発／ターンを明示した返信を送る。Proxy自身も公開表示への対応を検証する。古い版は許可せず再確認を案内する。
6. 例外の補足画面では現行の本人限定経路を使い、実引数と公開要約を混同しない。許可一覧・取消は `/mcp` へ分離する。内部フォーム・秘密入力は今回の直接承認へ変換しない。

以下の0.3経路は追加契約未対応時の互換設計。新カードの要件と、既存Proxyに対する表示可能範囲を混同しない。

契約書：`../codex-hoshikage-proxy/docs/mcp-turn-approval-api.ja.md`。操作詳細はGET interaction/operationで取得し、本人限定画面へ表示する。公開確認カードにコードを埋め込まない。approval_contextのprincipal/channelはGateway側で不透明識別子へ写像し、run_idは既存の永続依頼IDを使う。新しい操作は既存reply + grant_scope/expected_scope_fingerprint、一覧はResponseのmcp-grants、取消はgrantのrevokeという境界に合わせる。Capabilityの具体キー・完全な応答型・表示版の照合・取消照会・一覧件数は [接続レビュー](mcp-turn-approval-api-review.ja.md) で解消し、schema 7と接続コードへ反映する。現Gatewayのv1 SteerにもProxyの許可失効が適用されることを接続Gateとする。

API 0.3の [Gateway詳細設計・DB・受入](mcp-turn-approval-gateway-design.ja.md) を接続実装の基準とする。文書上の旧「API待ち」は経緯であり、この合意版の未決事項ではない。

## MCP承認0.5 内部設計（2026-09-17）

[内部詳細設計1.1](mcp-approval-v05-gateway-design.ja.md)を新profileの正本とする。型付き全scope、共通renderer、ページと投稿の二段階配信確認、schema9の表示・返信証跡、最新presentationでの押下検証、有界workerと独立sweeper、公開再照合と本人向け再表示を定義した。旧3renderer whitelist、本文digest、部分表示でも許可可能な経路を0.5で再利用しない。

新しい状態はCodex実行状態と独立させる。表示が失敗してもAIを再実行せず、許可返信結果不明はoperation照会を優先する。grantのactiveとavailabilityを分離し、カタログ正常更新は既存許可をProxyが再評価する。Gatewayは自動acceptを発行しない。

要件UI-01〜12と受入G05-01〜12を実装単位へ対応付けた。schema9は設計のみで適用前。後継契約と機能単位の詳細設計・受入条件が揃った範囲から実装する。全ツールの意味評価完了を一括の開始条件としない。実Discord受入とマニュアル更新は後工程とし、設計完成を動作確認済みと扱わない。

ProxyレビューGD-01〜03を反映：公開1ページのDiscord分割と、Proxy指定display_too_largeによるrequester全ページ表示を分離する。拒否はinteraction/revisionを正本とする独立分岐であり、scope・表示token・全ページ確認を前提にしない。許可bodyはexpected_scope_fingerprint／expected_presentation_fingerprintを正式名とし、詳細設計1.1の完全例から構成する。案内ボタンは「自分だけに表示して確認」。

## 責務分離後の内部構成

[後継内部設計1.0](mcp-approval-layered-gateway-design.ja.md)に従い、ExecutionBinding・DisplaySnapshot・PolicySelection・EffectivePolicy・ApprovalEligibilityを独立した内部型とする。表示先、単発可否、依頼中可否を一つのbooleanに統合しない。未評価操作の汎用表示を、ツール別説明registryの完成に依存させない。

schema9は[0.6実装詳細](mcp-approval-v06-implementation.ja.md)のmcp_v06_runs/views/pages/parts/decisionsとして確定した。旧0.5のschema9案は使用しない。要求profile・要求ポリシー・実効ポリシー・作成grantを別に保持し、未選択の世代を捏造しない。既存DBと旧Responseの意味を維持する。配信証跡、拒否独立、結果不明の照会、Supervisorの保証は継承する。


### API 0.6 実装との対応

- `mcp_v06`：版別契約型、nullable scope、選択と実効policy、presentation/全ページ証跡、endpoint別容量制限、schema9 Store。
- `mcp_v06_ui`：公開投稿と本人向けページの配信、許可・拒否の独立検証、ACK先行、返信意思のtransaction、固定キーの照会、古い投稿の閉鎖。通常は操作内容の最後の投稿へボタンを付ける。
- `proxy_v2`/`mcp_grants`：送信前のprofile・policy固定、要求body、Responseの準備結果照会、既存stop-by-request、許可範囲の保存・一覧・取消。
- `application`/`commands`：0.6ボタンのControl経路、設定準備・設定隔離待ちの利用者向け案内。停止・拒否・取消は新規Turn枠を使用しない。

`mcp_approval_v06` が利用可能な場合、新しいRunだけ0.6を使用する。既存Runの形式をcapability更新だけで変更しない。広告された0.6の型が不正なら新規実行の準備確認を失敗させ、旧形式へ黙って変更しない。選択済みpolicyが利用不能なら外して再実行しない。

実装詳細・受入記録は [0.6実装詳細](mcp-approval-v06-implementation.ja.md)。0.5文書中の旧schema案・wire・本人向け前後移動方式は同書で置換し、汎用表示に意味評価を必須とする制約は採用しない。

## 承認待機時間の補正（2026-09-17、Proxy調整中）

カタログのTTLと利用者の承認待機を分離する。カタログの更新中状態を、公開不能や恒久的な未評価へ置換しない。押下時の現在照合と送信意思の永続化は維持する。表示tokenの期限とinteraction本体の保持期限は別管理し、長時間離席後は元操作がまだ待機中であることを確認して再表示へ導く。許可未送信・送信結果不明を区別するエラー案内と再表示操作は、[Proxy修正依頼](proxy-mcp-v06-review-delay-fix.ja.md)で契約を調整してから実装する。現行の30秒キャッシュ挙動に合わせて利用者へ即時押下を要求しない。

### API 0.6第14節への接続補正（2026-09-17）

カタログ更新中／取得失敗のGETはcatalog_loading/catalog_failedとして表示し、通常監視で2秒以降に再照会する。押下直前のGETは最大3回、2秒間隔で再取得し、同じ表示証拠が復元された場合だけ元の明示選択を続行する。許可POSTの自動再送はしない。409 catalog_loading/catalog_failedは許可未送信が確定しているため、決定記録をREJECTEDとして旧送信意思を閉じ、新たな明示選択・拒否を可能にする。結果不明の操作と混同しない。エラー表示には許可未送信と、元の画面の「再確認」または更新後のボタンを選ぶ手順を示す。
