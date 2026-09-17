# MCP許可 Gateway詳細設計（合意API 0.3／インライン0.4）

> 0.3／0.4互換Response専用。新規0.5の設計は [内部詳細設計](mcp-approval-v05-gateway-design.ja.md) に置換する。旧renderer制限・本文digest経路を0.5へ流用しない。

2026-09-16。実装基準はProxy mcp-turn-approval-api.ja.md 0.3とmcp-inline-approval-api.ja.md 0.4。工程は本文・DB・受入の確定後に実装、モック受入、実Proxy/Discord受入。常駐有効化は別工程。

## 元の会話で承認するUI：追加契約の接続設計

要件2.5 UI-01〜UI-06、システム設計1.5に従う。Proxy側レビューの追加条件（公開判定・公開表示そのものへの対応付け・長文例外）を受け入れる。公開用APIは0.4 R-01対応版を接続レビュー済み。具体HTTP・型・上限は [接続レビュー](mcp-inline-approval-api-review.ja.md) とProxy契約を基準にする。

### 最初のカード

通常のカードは操作説明・対象・ツールと許可範囲を本文に示し、下部に「この依頼中、このツールを許可」「今回だけ許可」「拒否」を置く。ターン限定対象外は単発・拒否だけ。管理用の許可一覧・取消ボタンを混在させない。常時許可される操作に新しい承認要求は作らない。

公開用情報がない場合は、Gatewayで実引数から要約を生成しない。公開できない理由を示し、必要な場合だけ本人限定の補足確認・拒否へ進む。長文は、操作内容と選択肢が同じDiscordカードに収まるかを送信前に評価する。上限超過時に自動切詰めして許可ボタンを残さない。

### 表示と押下の順序

1. 認可済みの依頼・Proxy binding・interactionに対し、公開用情報を取得する。
2. 本文・componentsを生成し、表示版を実呼出しへ結び付ける情報とそのdigestを保存する。引数本文は保存しない。
3. 既存Deliveryの同一キーで送信する。送信結果不明では照合し、別POSTを作らない。
4. 押下時は認証済み本人、Guild、元会話、カードmessage ID、確実に配信されたdigestを照合する。Proxyから現表示を再取得し、内容・省略・承認可否・実行範囲・期限が一致することを確認する。
5. 既存の冪等返信へ、確定した追加契約の公開表示トークンを付ける。Proxyがそのトークンと実呼出しを再検証する。単発許可からターン許可へ暗黙昇格しない。
6. 古いカードは許可せず、現カードの確認または再表示の操作を案内する。新しい内容を自動許可する意味にはしない。

### 接続レビューで確定した項目

追加Capability/profile、公開GETの応答型・サイズ・文言の生成責任、表示トークンとscope fingerprintの関係、返信body、トークン期限・再起動後の扱い、未評価ツール・長文・秘匿の理由、対象外ツールの表示、Proxy側の競合拒否、旧Proxy互換。具体契約を照合済み。公開rendererはbrowser-find-v1／browser-navigate-v1／browser-tabs-list-v1だけを認識し、未知のものでは直接許可しない。

以下は追加契約未対応Proxyに必要な0.3互換経路。公開用情報がない場合の実引数非公開は維持する。

## 実行対応とCapability

既存requests.idをrun_idにする。principal/channelはschema_meta.instance_uuidと認証済み本人/Guild/実会話からドメイン分離したSHA-256識別子とし、送信前にmcp_run_contextへ固定する。これは匿名化保証ではない。既存依頼へ後付けしない。詳細またはターン許可Capabilityが対応profileで有効な場合だけapproval_contextを送る。開始後は設定再読込でcontextを置換しない。

Capabilityはmcp_operation_detailsとmcp_turn_approvalを独立検証し、native-item-id-v1のみ対応。型・定数上限・disclosure不正は該当拡張を無効とする。詳細APIが有効なら公開カードは操作詳細を表示せず、本人限定画面への入口だけ出す。既存内部フォームは従来経路を維持する。

## 本人限定の詳細と返信

公開ボタン「操作内容を確認」から本人限定画面を開く。毎操作でGuild・本人・依頼の会話・Proxy bindingを照合する。操作詳細はGETで取得し、interaction/Response/Turn/revision、scopeの全実行対応を検証する。生引数は一時メモリのみ。JSONをコードブロックのページ単位で本人限定表示し（本文中のバッククォートより長い囲みを使い、コードの記号を改変しない）、原文、秘匿、取得不能を区別する。任意コードから安全性を推定しない。

mcp_detail_viewsへ短いローカルID、利用者、interaction、表示revision、fingerprint、scopeのdigest、期限を保存する。本文は保存しない。ページ送りでも再取得し、同じ表示版と照合。古い画面の押下を新しい版へ移さない。実引数が取得できない場合は許可を出さず、拒否と停止を案内する。秘匿ありはターン許可不可と明記し、単発の判断は本人に委ねる。

本人限定画面に「今回だけ許可」「この依頼中、このツールを許可」「拒否」。ターン許可は両Capability・適格性・context・秘匿なし・危険ツール除外を照合し、引数変更も含むこと、最大10分/Runまで、Steerで失効することを表示する。両acceptはexpected_scope_fingerprintを送る。ターンのみgrant_scope=turn_toolを追加。送信前に既存mcp_interactionsへ操作キー・body digest・選択scopeを永続化し、結果不明は元operationを照会する。新しいキーで再送しない。

## 一覧と取消

/mcpはそのDiscord会話の最新Responseの許可一覧を本人限定で表示する。承認カードには許可一覧の入口を置かない。一覧は最大16件。全scopeをRunの保存済みcontextと照合し、active/pending/suspended/revoked/expiredを区別する。application_countは送信意思の件数として表示する。

mcp_grant_recordsは短いローカルID、request_id、grant_id、scope JSON、state、expiry、件数を保持。取消はmcp_grant_revokesへ固定キーを保存してからPOST {}。POST結果不明はoperationをGET照会し、未登録404でも安全側に自動再送しない（確認不能と案内し、一覧を再表示した際に元operationを再照会する）。別キーでの二重取消を作らない。in_flight_or_unknown_countを保存・表示し、取消が実行済み操作を戻す意味ではないと案内する。Run終端後も一覧から取消結果を照合できる。リロード・再起動後も同じDBを利用する。

## DB schema 7

mcp_run_context(request_id PK/FK, principal_id, channel_id, run_id)。mcp_detail_views(id PK, interaction_local_id FK, viewer_id, revision, fingerprint, scope_digest, expires_at)。mcp_grant_records(id PK, request_id FK, grant_id, scope_json, state, expires_at, application_count, UNIQUE(request_id,grant_id))。mcp_grant_revokes(grant_local_id PK/FK, operation_key UNIQUE, state, in_flight_count nullable)。mcp_interactionsにgrant_scope nullableを追加。既存回答・配信記録は変更しない。秘密引数/フォーム本文/Discord tokenは新テーブルに保存しない。旧schemaはtransactionで前進し、未知schemaは従来どおり拒否する。

## 上限と復旧

通常v2 JSON上限4MiBを維持し、operationのみ256KiB、grant一覧1MiB、interaction一覧は拡張有効時64MiB/256件、従来64KiB/16件。大きな一覧の取得はApp共有Mutexで1件に制限し、処理中も保有する。新規実行枠とは独立。未対応profileは従来上限へ。未知state・必須欠落・不一致では許可しない。

Steerは既存v1経路を維持。Proxyが失効→入力世代更新→送信を保証し、Gatewayは表示版の再照合で古い押下を拒否する。停止・再起動・世代変更をローカル推測で成功としない。

## 受入対応

API契約A01〜A11を基準に、Gateway単体/Mockで以下を検証する：Capability欠落/無効/型不正、opaque context固定、全scopeの不一致拒否、本人限定表示/公開・DB非漏洩、単発とターンbody差、危険ツール不可、表示後の世代変更、二重クリック/応答喪失、一覧と取消の型・target・キー、schema6→7、上限変更。実上流のID照合・実ブラウザ代替と確認回数・実Discord UIは未実証のまま明記する。設定既定値と常駐サービスは変更しない。

## 0.4実装の確定詳細（schema 8）

- `mcp_inline_runs(request_id PK/FK)` に新規実行のsource_conversation宣言を送信前に固定する。0.4 capabilityと0.3 details対応の両方を検証できた依頼だけを登録し、既存Responseへ後付けしない。受付POSTへapproval_presentationを追加する。送信時に非対応なら宣言を削って再送せず停止する。
- `mcp_inline_views` はローカルid、interaction_local_id、presentation_id、presentation_fingerprint、revision、snapshot_digest、expires_at、activeを保持する。1 interactionの現表示は1版。他版に切り替える前に旧版をinactiveとする。本文は保存しない。Discord message IDと本文/components digestは既存deliveriesのmcp_actionに保持する。
- operationとpresentationのID/revision/scope fingerprint/audience/contextを照合。全必須フィールド・enum・actions・非空文字列・UTF-16上限・field件数・認識済みrendererを検証する。公開用GETは32KiBまで。未知／不正応答では旧表示を無効化し、固定の確認不能案内と拒否だけへ切り替える。
- 表示値は可変長コードフェンス内に原文を置き、Markdown・mention・リンク解釈を防ぐ。allowed_mentionsは無効。装飾込み2000 UTF-16 unitsを超えたら補足確認へ切り替え、許可可能な説明を切り詰めない。
- `mi:<local_view>:once/turn` のボタンは本人/Guild/thread/message ID、active版、Proxy再照会のsnapshot、既存DeliveryのCONFIRMED digestをすべて照合する。承認bodyは既存0.3にapproval_viewとexpected_presentation_fingerprintを追加する。既存の操作キー永続化・排他・結果照会を共用し、二重送信しない。
- 成功押下はDiscordの更新ACKだけ返し、公開カードは既存監視ループが更新する。失敗時だけ本人限定で再確認を案内し、公開カードの説明をエラー文で上書きしない。一覧操作は `/mcp` に分離する。
- 旧Proxy・公開方式未宣言の既存Responseは0.3経路。秘匿・未評価ツール・長文は本人限定の補足、unavailableは許可なし。既存フォームは変換しない。

実装後の検証は通常3択／2択、追加宣言、機密非漏洩、旧版・別message・別利用者、長文・未知型、未確定配信、二重押下・復旧、schema 7からの移行を対象とする。
