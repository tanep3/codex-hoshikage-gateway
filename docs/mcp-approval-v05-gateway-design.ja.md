# MCP承認 API 0.5 Gateway内部詳細設計

> 責務再整理（2026-09-17）：[Proxyの改訂方針](../../codex-hoshikage-proxy/docs/mcp-approval-layering-revision.ja.md)を受け入れる。全342ツールの意味評価完了を汎用機能の実装開始条件としない。表示・単発承認・ポリシー選択を分離する。以下の0.5固有の意味未評価時の許可禁止、必須policy世代、共通の実装開始条件は後継契約で改訂する対象であり、新設計の確定事項として流用しない。後継wire契約は未確定のため、現行0.5のAPI解釈を黙って変更しない。
版1.1 / 2026-09-17 / Tane Channel Technology

状態：旧0.5の接続仕様に対応する内部設計履歴。後継の正本は [責務分離後の設計](mcp-approval-layered-gateway-design.ja.md)。実装・migration適用・実Discord受入・配備は未実施。正本は[Proxy契約0.5](../../codex-hoshikage-proxy/docs/mcp-approval-full-fix-api.ja.md)（第11〜13節を含む）、[接続レビュー](mcp-approval-full-fix-api-review.ja.md)、要件2.7、システム設計1.7。旧[詳細設計](mcp-turn-approval-gateway-design.ja.md)は0.3／0.4 Responseの互換経路だけに適用する。

## 1. 責務と実装単位

| 単位 | 責務・境界 |
| --- | --- |
| proxy adapter / 新mcp_presentation型 | 0.5 capability、presentation、operation、grantの上限付き取得・型検証。生引数を公開rendererへ渡さない |
| mcp_approval_render | audience付き検証済みdisplayから決定的なDiscord投稿列を作成。ツール別判定・翻訳・安全性推測をしない |
| mcp_approval_store | profile、scope、表示版、ページ・投稿の配信証跡、返信意思をtransactionで保存 |
| mcp_approval_controller | 発見・表示・更新・押下・再照会の状態機械。interaction単位の排他とversion比較 |
| Discord adapter | 更新ACK、本人向け返信、投稿・編集・取得。HTTP結果と配信確定を区別 |
| 既存mcp_grants | profile別一覧・取消。0.5のgrant_policyとavailabilityを検証 |

新規TurnのSemaphoreをこれらの処理で取得しない。Proxyが操作の意味、公開範囲、許可適用、失効を判断し、Gatewayが本人と送信先を検証する。モデル文章や英語の確認文を解析して不足する権限情報を補わない。内部フォームは既存mcp_form経路のまま。

## 2. Response別の契約固定

`ApprovalProfile = Legacy03 | Inline04 | Source05` を送信前にrequestへ固定する。既存0.4宣言記録ありはInline04、それ以外の既存requestはLegacy03として移行し、既存ResponseへSource05を後付けしない。新規requestは0.5 capabilityとdetailsの既知profile・全必須上限を検証してSource05を選ぶ。turn capabilityは依頼中許可の追加条件。旧inline capabilityは0.5選択に使わない。

0.5非対応の場合は既存の対応済み版を選べるが、0.5送信開始後に宣言を削って再送しない。新profile選択後の能力低下では新規送信を保留し、既存operationを照会する。接続復旧でも既存Responseのprofileは変えない。

Source05はscopeの全項目を型付きで保存・照合する。instance/recovery/contextの3値/Response/conversation/workspace/Turn/input/config/server/toolにpolicy_generationとdefinition_generationを含む。server/toolは生引数ではないが公開表示はProxyのdisplayを使う。未知profile・欠落・型不正はその機能を使用不可とし、旧型へ縮退解釈しない。

## 3. 最初の画面と長文

通常readyは元会話へ、Proxyのtitle、fields全件、limitations全件、許可範囲の固定説明を表示する。omissionsが非空なら許可不可。ボタンは「この依頼中、このツールを許可」「今回だけ許可」「拒否」。turn不適格は単発・拒否だけとし、理由を示す。対象外だからという理由で本人向けへ移さない。

private_requiredは公開displayの理由と「自分だけに表示して確認」「拒否」を表示する。個人情報・私信・認証情報・コード等の例外に加え、非機密でもProxyの公開ページ上限を超えたreason=display_too_largeを受理する。後者は「内容が長いため、自分だけに表示して順に確認してください」と案内し、機密情報だとは説明しない。未対応や取得失敗をこの例外へ偽装しない。本人向けは同じDiscord会話のephemeral返信で、DMや新スレッドを作らない。「同じ会話の他の参加者には見えません。開いただけでは実行しません」と説明する。

unavailableは許可なし。retryableなら同じカードに「操作情報を確認しています。操作せずお待ちください」、期限到達後は「確認できませんでした。『再確認』で読み直すか、『拒否』を選んでください」。手動再確認はGETだけで、実行・承認を送らない。非retryableは具体的な固定理由と拒否／運用者への連絡を案内。closedは全ボタンを無効化する。

### 3.1 投稿列の決定的な構築

既存の通常contentとAction Rowを使い、embed必須の権限を追加しない。Discord content上限2000に対し、GatewayはUTF-16単位で最大1900の投稿を作る。固定の通し番号・継続表示を含めて計数する。Proxyの1ページはDiscordの1投稿を意味しない。

1. Proxy上限（4800 units、24fields、各値1000、label/title200、64KiB応答）を先に検証する。
2. 項目をtitle→fields→limitationsの順に並べ、label/value境界・配列の順序を保持する。Gateway固定文とProxy由来の内容を区別する。
3. 値はMarkdownを文字として表示できる決定的なescapeを使い、mention解析はallowed_mentions.parse=[]で無効化する。URLのpreviewを抑制する。バッククォートや制御文字も試験し、入力のコードを改変して実コードと称さない。Proxyの改行・タブ可視化を二重変換しない。
4. escape後、Unicode scalarを壊さず分割する。項目途中では同じ項目の続きと表示し、意味の階層を変更しない。各ページ最大32投稿・変換後合計32768 unitsを内部上限とする。超過は表示不能として許可せず、内容を切り捨てない。
5. 先行投稿には許可ボタンを付けない。最後の投稿に選択肢を置く。全投稿の配信確定前に押されても承認送信しない。通常の短い操作は1投稿で内容と選択肢が揃う。

Proxyがsource_conversationのreadyを返した場合は、その1ページを同じ会話の投稿グループとして表示する。投稿数を理由に本人向けへ逃がさず、許可には全投稿の配信確認が必要。Proxyがdisplay_too_largeでprivate_requiredを返した場合は、requesterの全ページ表示へ進む。GatewayのDiscord投稿分割とは別経路であり、requesterの内容を独自判断で公開へ戻さない。render自体が失敗した場合は固定エラーと拒否にする。

### 3.2 本人向けページ

一度に1ページのみ取得・表示し、最大64ページ、全display JSON合計256KiBを逐次計数する。ページ内の投稿分割は上記と同じ。ナビゲーションは「前へ／次へ／拒否」で、未確認ページを飛び越せない。最終ページかつ全ページの全投稿CONFIRMED時のみ許可2種（適格なもの）を表示する。各ページのtokenは返信用に保存するがcustom_idへ入れない。

ページ切替は同じpresentation IDを指定して再取得し、revision、scope、fingerprint、content_fingerprint、audience、count、期限を一致確認する。変化は全確認記録を無効化し、先頭から再表示。公開と本人向けの確認記録は混ぜない。再表示された古い画面のボタンでは許可できない。

Discord公式のcontent上限とinteraction応答期限・ephemeralの扱いを基準とする：[Message](https://docs.discord.com/developers/resources/message)、[Interactions](https://docs.discord.com/developers/interactions/receiving-and-responding)（2026-09-17確認）。本製品はGuildへ導入したBotが対象で、user-installのみの別構成をこの投稿数設計の対象としない。interaction tokenは最大15分の有効期間内でも、表示自体の短い期限を超えて使わない。初期ACKは3秒以内、Gateway目標1秒以内。Proxy照会より先に行う。公開の押下はtype 6更新ACK、本人向けを新規に開く操作はtype 5＋flags 64、その後本人向けに編集する。成功の追加メッセージは出さない。

## 4. 永続データ（schema 9予定）

SQLの実装は別工程。下記の論理schemaをmigration 009の基準とする。既知8→9のみtransactionで追加し、既存表・操作履歴を削除しない。checksum・未来版拒否・受付前migrationは既存規則を維持する。

| 表 | 保存項目・制約 |
| --- | --- |
| mcp_approval_profiles | request_id PK/FK、profile CHECK(03/04/05)。送信意思保存前に確定、以後不変 |
| mcp_approval_views | id PK（ランダム短ID）、interaction_local_id FK、audience、viewer_id、revision、scope_json、scope_fingerprint、presentation_id/fingerprint、content_fingerprint、page_count、expires_at、state、active CHECK(0/1)、version、boot_id、next_poll_ms、poll_deadline_ms、error_code。viewer_idは常に認可本人の非null ID（公開版も同じ）。activeはinteraction/audience/viewerごとに最大1版の部分UNIQUE |
| mcp_approval_pages | view_id FK、page_index、page_token、state、part_count、display_bytes。PK(view_id,page_index)、同一viewのtoken UNIQUE。indexは0〜63、part_countは1〜32 |
| mcp_approval_parts | view_id/page_indexの複合FK、part_index、message_id、channel_id、state、attempt_id UNIQUE、render_mac、boot_id。PK(view_id,page_index,part_index)。CONFIRMEDはmessage_id必須 |
| mcp_approval_decisions | id PK、interaction_local_id FK、active CHECK(0/1)、view_id FK nullable（拒否は不要）、discord_interaction_id UNIQUE、action、operation_key UNIQUE、revision、scope_fingerprint、presentation_fingerprint、approval_view、page_tokens_json、state、operation_id、error_code。返信bodyはこれらの非本文情報から一意に再構成。interactionごとのactive=1は部分UNIQUE |
| mcp_grant_recordsの追加列 | profile、grant_policy_json、availability_json。旧版は追加列null、新版は全項目必須。scope_jsonは世代を含む完全型 |

本文・生引数・コード・フォーム入力・Discord interaction tokenはDB・ログ・一時ファイルへ保存しない。scope等のIDは既存の保護されたDBに保存し、UIへ内部値を露出しない。render_macはプロセスごとのランダム鍵によるHMACで、秘密値を辞書照合できる無鍵digestを作らない。鍵は永続化しない。既存Deliveryの無鍵本文digest経路へ0.5カードを通さない。

declineのdecisionはview_id/scope_fingerprint/presentation_fingerprint/approval_view/page_tokens_jsonをnull可とし、acceptのdecisionは必須とするCHECK制約を置く。unavailable等で存在しないpresentation ID/token/scopeはnullで保存し、readyへ遷移する前に必須項目を検証する。state・audience・actionは閉じたenum。FK有効、更新はversion条件付き。期限順・interaction状態順のindexを追加する。本文付きAPI応答をエラーへ丸ごと出力しない。終了viewのtokenとrender_macは返信operationが確定し再照会不要となってから削除できる。未確定operationの照合メタデータを定期掃除で消さない。通常終了後は24時間で表示用メタデータを掃除し、決定ID・scope・最終状態の監査記録は既存の状態廃止方針へ従う。

## 5. 表示・配信の状態機械

View: FETCHING → RENDERING → DELIVERING → READY。途中でWAITING（取得待ち）、STALE、UNAVAILABLE、CLOSEDへ移れる。READYはProxy readyだけでなく全表示確定・未失効・同一版を満たすローカル状態。新revision到着時は旧viewをSTALEにしてから新viewを作る。

Part: PREPARED → SENDING（DB commit）→ CONFIRMED。通信断はUNKNOWN、確定拒否はFAILED。SENDINGの再起動復旧はUNKNOWN。DiscordへPOSTした結果不明を新規POSTで補わない。message IDが分かる公開投稿はGETでauthor/channel/内容/componentsを再描画予定と比較できる場合だけ確定。ID不明は既存の確実な送信相関証拠で特定できなければUNKNOWNのまま。直近の似た文章だけで決めない。

同じviewの配信は1worker、同じinteractionの版切替と押下は短いローカル排他＋DB versionで直列化。ネットワークawait中にDB transactionを保持しない。送信中に版が変わった遅着結果は旧viewの配信履歴にだけ記録し、新viewをREADYにしない。

既知messageのPATCH結果不明もGET照合が先。古い内容と新しいボタンを混ぜない。新旧view間で本文投稿を再利用する場合も新しい版の全内容を再確認し、旧decisionを移植しない。整理時は所有確認付き削除、削除失敗でも古いボタンはDBで拒否する。

## 6. 押下とProxy返信

custom_idは `ma5:<view-local-id>:<action>`、操作種別はonce/turn/decline/private/next/prev/recheck。秘密token・scope・引数は入れない。共通処理とaction別の条件を分離し、許可専用の検証を拒否やページ移動へ適用しない。

### 6.1 共通処理と読取操作

ACK後、本人・Guild・実会話・カードmessage IDとローカルinteractionの対応・Response profile・Proxy bindingを照合する。既存decisionと現在のinteraction状態を確認し、同じ押下は既存結果を返す。別のdecisionが受理済み・結果不明・送信中なら新しい返信を作らない。解決済みは実状態を表示する。

private/next/prev/recheckは取得と表示だけを行う。privateは公開カードと本人の対応、Proxyの本人向け表示の条件を確認する。next/prevは該当viewの版とページ範囲、既述の順番を確認するが、全ページの表示完了は要求しない。recheckは同じinteractionをGETし、許可や実行を送らない。読み直して内容が変われば旧確認を無効化する。

### 6.2 拒否（decline）

最新interactionが拒否可能なpendingで、競合するdecisionがない場合、最新revisionに対する拒否意思を保存する。presentationの取得成功、READY、非null scope/fingerprint、ページtoken、全ページ／全投稿の配信完了は要求しない。catalog_loading、unavailable、scope=null、最初のページだけ表示済み、後続ページ取得失敗でも同じ経路を使う。表示の期限切れをinteraction自体の期限切れと混同しない。

transactionでinteractionの対応と未決定を再確認し、固定operation_keyと拒否bodyを保存する。view_idと許可用フィールドはnullでよい。現在のinteractionを取得できない場合は新しい返信を送らず「拒否をまだ確認できません。接続回復後にもう一度確認してください」と案内する。送信後の通信断は結果不明としてoperationを照会し、拒否成功と表示しない。

```json
{
  "expected_revision": 1,
  "response": {"action": "decline", "content": null}
}
```

### 6.3 許可（once/turn）

1. 最新presentationを取得。全scope・revision・audience・fingerprint・世代・期限と全ページtokenを検証。原文を新旧の異なるsnapshotから合成しない。最新actionsがfalseなら旧READYを使わない。
2. 単発はallow_once、依頼中はそれに加えてallow_turn_tool・tool_policy.turn_eligible・turn capabilityを要求。許可範囲とlimitationsを表示済みであることを照合する。
3. transactionでactive view/version、全配信CONFIRMED、未決定を再確認し、返信意思・固定operation_key・全tokenを保存する。
4. 単発の正式な返信bodyは次のとおり。DB列のscope_fingerprint／presentation_fingerprintを、そのまま送信キー名にしない。

```json
{
  "expected_revision": 1,
  "expected_scope_fingerprint": "opaque-scope",
  "approval_view": "source_conversation",
  "expected_presentation_fingerprint": "opaque-presentation",
  "expected_page_tokens": ["opaque-page"],
  "response": {"action": "accept", "content": {}}
}
```

依頼中許可はこのbodyへ `"grant_scope":"turn_tool"` だけを追加する。requesterならapproval_viewをrequesterにし、その表示版の全ページtokenを順に渡す。Idempotency-KeyはHTTP headerで送る。DBにある同じ決定から、常に同じキー・同じbodyを再構成する。

### 6.4 送信と結果照会（許可・拒否共通）

POST前にSENDINGをcommitする。不明は同じキーのoperationを照会。未登録404も自動再送せず保留し、既存復旧方針で扱う。Proxyが不受理を確定したstale等は当該試行をREJECTEDにし、最新状態に対する新たな明示押下だけを別試行として認める（不受理確定した旧行のactive=0と新行作成を同じtransactionで行う）。拒否の再試行に許可用の表示確認を要求しない。

decisionの旧行は同じ表に保持し、operation_key/discord_interaction_idのUNIQUEは過去試行を含む。状態はPREPARED/SENDING/UNKNOWN/ACCEPTED/RESOLVED/REJECTED。不受理が確定したREJECTEDだけactiveを解除できる。結果不明・受理済みのdecisionを解除して新試行を作ってはならない。拒否・取消・Stopとの最終競合判断はProxyの送信意思境界が正本。

正常受付は元カードを処理中へ更新、解決確認後に既存の集約方針へ従う。許可送信完了とツール成功を同一視しない。元の会話へ回答を継続し、承認による新会話・新Threadは作らない。

## 7. 定期照会・Supervisor・資源上限

操作照会専用Semaphoreは4、同一interactionの取得は1件へ集約。待機キュー最大256、超過対象はDBのnext_pollで後回しにし、要求ごとにtaskを作らない。Stop・拒否・取消用の制御枠は別に2とし、表示取得や新規Turn枠が埋まっていても進める。これはGateway内部既定値であり利用者の新設定を増やさない。

retryableはProxyのretry_after_ms以上（通常2秒）で照会。ローカル自動照会は初回から60秒またはinteraction期限の早い方まで。同じviewのGETで延長しない。通信障害は2/4/8秒、最大10秒のバックオフ、429はRetry-Afterを優先。期限後もinteractionの終了監視は継続するが内容の高速再取得は止め、手動の再確認で新しい有界照会期間を開始できる。

カタログrefreshは同じカードを更新する。grant.state=activeかつavailability=refreshingは「許可は有効・接続先情報を更新中」であり、Gatewayからacceptを送らない。正常更新後の適用はProxyに任せ、interaction解決を監視する。

64KiB/ページ、256KiB/display合計、operation256KiB、grant一覧1MiBの応答上限をstream読取時に適用。ページ全件をRAMへ展開せず逐次取得。表示worker全体の一時メモリ予算8MiBを設け、確保できなければ待機させる。HTTP body・加工後文字列を同時に無制限保持しない。

長寿命controller/sweeperをSupervisorで監視。異常終了時は新しい許可UIをDEGRADEDにし、保存済みdecisionを照会する。task再起動でPOSTを繰り返さない。独立sweeperを5秒周期で動かし、view期限切れと孤立SENDINGを回収する。DB worker故障は既存のプロセス停止方針へ従う。

## 8. 再起動・世代不一致・権限取消

Gateway再起動では全viewを再検証待ちにし、永続READYだけで承認を通さない。publicは同じProxy版を再取得・再描画し、保存された全messageをGET比較して現在のプロセス鍵のMACへ置き換えた場合だけ復帰。配信前POST結果不明は前節の証拠が必要。

requesterは過去の配信記録を監査用に残すが確認済み進捗は再利用しない。新しい本人の「自分だけに表示して確認」から新ローカルviewを作り、全ページを再表示する。ephemeral webhook tokenはメモリのみで期限内に限り使用する。プロセス再起動やtoken期限切れで通常チャンネルへの公開送信へ切り替えない。

Proxy再起動・正式復元・binding不一致は旧表示を無効化し、既存operation照会を先に行う。許可再作成・実行再送をしない。Guild/会話/本人権限が失われたら取得・配信・許可を停止し、private表示を別場所へ転送しない。削除されたカードは表示証拠に使わず、再表示を案内する。

## 9. 一覧・MCP以外の承認

/mcpは従来どおり本人向け。0.5 grant_policyの通常操作と常時個別操作、availabilityの更新待ち／無効理由を表示する。scopeの世代を落とさず保存し、Responseをまたいで集約した許可として扱わない。件数は許可適用の送信意思数で、ツール成功回数ではない。取消は既存の冪等operation照会経路を使用する。

G-01対策：command文字列950 units、ファイル8件等の既存切捨てを「表示完了」にしない。MCP以外はProxyが与える承認内容を完全性付きの別型にし、既存公開範囲を拡張しない。全件を上記の投稿分割で表示できた場合のみ単発承認可能。内容が欠落・未知の追加権限・容量超過なら理由と拒否を表示する。MCP専用tokenやturn grantを非MCP承認へ流用しない。

## 10. 受入と実装順

| ID | 境界・合格条件 |
| --- | --- |
| G05-01 | 0.3/0.4/0.5混在、capability不正・消失。既存profile不変、新版宣言を削った再送なし |
| G05-02 | 通常3択・個別2択・private例外・未対応・loadingを区別。tabs以外にも共通rendererを適用。非機密のdisplay_too_largeは長文理由でrequesterへ進む |
| G05-03 | 1900/2000境界、4800表示、24fields、64ページ、長文単一値、Unicode・Markdown・mention。公開1ページの複数投稿とrequester複数ページを別々に検証。切捨て・意図しない通知なし |
| G05-04 | 全ページ／全投稿CONFIRMED前、別人・別会話・別message・別audience・別世代・未知scopeで許可送信0回。pendingならscope=null・ページ未取得・後続ページ失敗でも拒否可 |
| G05-05 | POST/PATCH前後のcrash、配信応答喪失、途中ページ失敗、版変更。未知結果からの自動再投稿・自動許可なし |
| G05-06 | 二重押下、once/turn競合、拒否/Stop/Steer競合、operation応答喪失。返信意思・上流適用最大1回。once/turn/requester/declineのJSONキー・null・省略を厳密照合し、拒否に許可用token不要 |
| G05-07 | public再照合、private全ページ再表示、Proxy再起動、復元、token期限切れ、権限取消。旧READYの無条件復活なし |
| G05-08 | grant一覧の新世代・policy・availability、正常30秒更新で追加クリックなし、失敗後は旧grant非復活 |
| G05-09 | schema8→9 rollback・再実行・未来版拒否。旧Response/decision不変、DB/logに本文・token URL・無鍵秘密digestなし |
| G05-10 | worker panic/timeout、256待機、4取得枠飽和でも拒否・Stop可能。catalog_loading中も全ページ検証を通さず拒否可。通信不通時は拒否成功としない。期限sweeperが独立作動 |
| G05-11 | 非MCPの長コマンド・9件以上の変更、情報欠落。全部を示すか許可なし。部分表示での承認なし |
| G05-12 | 実Proxy全カタログ＋実モデル＋許可済み実Discord先で、初回表示→本人押下→実行→回答まで確認 |

実装順：型とStore/migration→renderer/Discord投稿列→controller/返信/復旧→grant一覧と非MCP完全性→Mock/クラッシュ試験→実Proxy/Discord受入→英日利用者文書→配備。要件・設計・Proxy機能単位の詳細設計の整合確認が実装開始条件であり、本書の作成だけで製品コードの先行実装を始めない。

## 11. Proxyレビューへの対応（GD-01〜03）

[Proxyレビュー](../../codex-hoshikage-proxy/docs/mcp-approval-v05-gateway-design-review.ja.md)の3点を受け入れ、1.1で修正した。GD-01は第3・3.1・3.2節とG05-02/03、GD-02は第6.1/6.2節・DB null条件とG05-04/06/10、GD-03は第6.3節の完全JSON例とG05-06に対応する。ボタン名は「自分だけに表示して確認」へ統一した。文書上の修正であり、製品コードの修正・受入成功・常駐反映を意味しない。
