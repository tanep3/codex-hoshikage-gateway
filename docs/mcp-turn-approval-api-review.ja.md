# Proxy MCPターン許可API：Gateway接続レビュー

## 0.3の照合結果（2026-09-16）

**判定：接続契約0.3を受け入れる。前回の5指摘は解消。接続契約に実装開始を妨げる未決事項は見つからなかった。** 実装・受入・常駐有効化の完了を意味しない。

照合資料：ProxyのAPI契約0.3、関連要件・設計、interaction-api追補、development-process。Gatewayの既存mcp_ui/proxy_v2/Steer経路も確認した。今回はコード・設定・サービスを変更していない。

| 前回指摘 | 確認結果 |
| --- | --- |
| Capability・JSON・上限 | トップレベル2機能のキー、型、未対応/無効、不明profile、各応答と上限を規定。解消 |
| fallback・旧クライアント | 安全変換できる単発mcp_formと、user_input未宣言による停止を明確に分離。一般user_inputの追加実装は必須ではない。プロセス全体の互換性は受入で確認。解消 |
| 本人限定詳細・表示内容との照合 | 実引数はGET operationのみ。単発も詳細画面経由ではrevision+fingerprintを付ける。古い世代・差替えを拒否。解消 |
| Steer失効 | 現在のv1 Steer経路で失効/入力世代を保存してから上流へ送信。保存失敗時は送らず、送信不明でも旧許可を復活させない。解消 |
| 実利用の改善 | snapshot評価・モデルの実選択・同一商品ランキング作業の確認回数比較・Discordカード数をA11へ具体化。文書上の指摘は解消、試験は未完了 |

### Gateway側の実装設計へ反映する項目

- Capabilityは任意拡張として独立判定し、未対応Proxyの従来動作を維持する。未知profile・不正な型では新しい許可ボタンを出さない。
- 現在のinteraction一覧16件/64KiB検証と、汎用JSON取得4MiB上限をそのまま使えない。有効時の一覧256件/64MiB、operation 256KiB、grant一覧1MiBをエンドポイント別に検証する。他のAPIの上限は広げず、取得並列数とバッファ保持もGateway側で制限する。
- 実引数/コードは本人限定の期限付きページ表示へ。公開カードやSQLite・ログには転記しない。表示版トークンは短いローカルID経由でボタンへ結合し、上限8192 bytesのProxy識別子をDiscord custom_idへ直接埋め込まない。
- approval_contextの本人/会話/Runは送信前に固定し、再起動や設定再読込時に別本人の値へ再生成しない。scope全項目をローカルの依頼・Proxy bindingと照合する。
- 許可作成は既存replyの単発と明示ターン許可を分離し、grant.stateとinteraction.reply_statusを別に監視する。自動適用数を成功件数と説明しない。
- revoke結果不明は同じoperationを照会し、取消時点のin_flight_or_unknown_countを表示する。未登録の確実な404以外で取消を自動再送しない。
- browser_evaluateは0.3では毎回個別確認。A11の代替経路が実証されるまで、今回の連続クリック問題を解決済みと表示しない。

### 工程判定

Proxy側の接続レビューGateは通過。Gatewayは上記を要件・詳細設計・DB・受入条件へ先に反映して整合確認し、その後0.3基準の実装へ進む。仕様変更が必要になればコードより文書を先に改訂する。常駐自動許可は受入前に有効化しない。

---

## 以下は0.1時点の指摘記録（0.3で解消済み）

2026-09-16。対象：Proxy `docs/mcp-turn-approval-api.ja.md` 契約案0.1。
判定：方針合意を維持。操作詳細の本人限定表示、単発とターン許可の分離、設定世代・入力世代、取消の競合境界は妥当。下記の接続仕様を補完して実装する。常駐自動許可の有効化承認ではない。

## 1. CapabilityとJSON契約の確定

Gatewayは存在しない機能のボタンを出さないため、capabilitiesの正確なキー・型・enabled/disabled/未対応の違いを必要とする。操作詳細とターン許可は別機能として判定できるようにする。

次の成功・無効・取得不能の完全なJSON例と必須/nullable項目を提示してほしい。

- GET interaction/operation。revisionはinteraction revisionと同一か、別の操作詳細revisionか。表示後の引数・秘匿内容の変更でrevisionまたはscope_fingerprintが必ず変化するか。
- replyのoperation応答と、作成済みgrant IDを取得するinteraction応答。operation照会のresource.type/idは従来どおりinteractionか。
- mcp-grants一覧の外側構造、scopeの構造、対象Response/Turn/input_generationの照合項目、各状態のreason。
- revokeの要求body、応答、失敗・通信切断後のoperation照会。Idempotency-Keyによる照会対象のresource.type/id。
- タイムスタンプの形式、各文字列とarguments/応答全体の上限、一覧の全件保証またはpagination。256履歴と従来16件のinteraction一覧上限の関係。

上限の16件が未回答件数だけなら、Gatewayの現行「一覧全件<=16」検証も改定が必要。黙って後続を切り捨てる形式にはしない。

## 2. 対応付け不足時のフォールバック

現Gatewayが宣言するのはmcp_formのみで、一般user_inputは未宣言。契約の「元の個別対話」がtool/requestUserInputのまま公開されるなら、従来の個別承認が出るのではなくunsupported_interactionで停止する可能性がある。

- 安全に変換できる承認は、ターン許可不可の空mcp_formとして単発確認できるのか。
- それすら確定できない場合、どのkind/errorとなるのか。
- user_input対応が必要なら、その条件を明示してほしい。Gatewayは任意のuser_inputを推測でツール承認へ変換しない。

Codex側のfeature変更が未対応クライアントや並列Runの挙動を変えない範囲も、Proxyの隔離実証で確認する。

## 3. 本人限定の詳細画面と許可する内容の結合

Gatewayは公開カードに引数・コードを出さず、「操作内容を確認」から本人限定画面を開く。長文はページ送りし、秘匿・欠落・未取得を明示する。実引数と翻訳/要約を区別し、任意コードを読み取り専用と説明しない。

その画面からの許可は表示済みinteraction revisionとscope_fingerprintへ拘束する。scope_fingerprintが何を含むか（call、実引数の版、設定・入力世代、許可範囲）と、単発時にも表示した操作から変化しない照合方法を明記してほしい。

ツール内フォームや通常の単発確認は従来機能として維持する。詳細未取得時に、確認済みであるかのようにターン限定ボタンを出さない。

## 4. 入力世代とSteerの呼出し経路

現GatewayのSteerは `/v1/codex/turns/{turn_id}/steer`。この経路でも、旧許可失効の受付・入力世代更新が上流Steer送信より先に行われることを確認してほしい。別APIが必要なら移行先を定義する。

新しい通常投稿は別Runへ入り、承認ボタンは入力世代を進めない。失敗・結果不明のSteerで旧許可を復活させない。失効処理を確認できないのにSteerだけ送信して進めない。

## 5. 利用体験の受入は残る

browser_evaluateを常時個別表示する判断は、コードの意味を一般に保証できないという制約に沿う。ただし今回の「同じ確認を5回押す」問題は、このツールではまだ解消しない。

運用者が評価する専用snapshot等の具体的な候補、利用モデルがその経路を選べる条件、商品ページのランキング確認という実作業での確認回数を実証してほしい。単にツール名を設定へ追加することを代替策の完成と扱わない。

## Gateway側で確定できる実装方針

- approval_contextはDiscordのIDをそのまま渡さず、Gatewayインスタンス・認証済み本人・Guild/実会話を使うドメイン分離した不透明識別子へ写像する。run_idは永続化済みGateway依頼ID。対応の正本はGatewayで保持する。これ自体を匿名化やProxy側本人認証と主張しない。
- 公開カードはサーバー・状態・操作入口のみ。実引数/コードは本人限定・期限付き画面。本文・コードをDBやログへ追加保存しない。
- ターン許可の作成は既存replyの永続操作キーを使い、scopeと表示版のfingerprintを監査する。単発acceptは従来bodyのままで昇格させない。
- grant一覧・取消は本人と対象Runを再認可する。取消受付後もin_flight_or_unknown_countを表示し、既実行の取り消しや作業停止とは説明しない。
- Capability未対応では既存単発確認を維持する。具体JSON・revision境界が未確定の間は送信bodyとDB移行を推測で実装しない。
