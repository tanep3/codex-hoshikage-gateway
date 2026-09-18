# Gateway専属Codex App Server — 内部システム設計

版0.2 / 2026-09-18 / Tane Channel Technology

状態：目標構成の内部設計案。[目標要件](requirements-direct-app-server.ja.md)に対応。切替時の新規文脈とDB実行方式の境界を確定。現行常駐サービスは未変更。

## 1. 配置と依存方向

```text
systemd --user
  └─ Gateway process
       ├─ Discord adapter ──表示・配信──┐
       ├─ Application / Scheduler ─────┼─ Store（SQLite、状態正本）
       ├─ Approval coordinator ────────┤
       ├─ Content store ────────────────┤
       └─ Codex execution ── Codex transport ── stdio ── Codex App Server child
```

Discord adapterはCodex transportを呼ばない。Codex transportはDiscord、成果物ファイル、SQLiteの構造を知らない。Applicationが明示的な内部コマンドとイベントで両者を接続する。Approval coordinatorは上流の要求に対する返信権限を持つが、意味ベース委任など任意の判断方式とは別にする。Content storeは保存済みデータの正本を持ち、Discord deliveryは配信記録の正本を持つ。

## 2. モジュール境界と取り込むコード

| 内部モジュール案 | 所有するもの | Proxyからの参考実装 | 移さないもの |
| --- | --- | --- | --- |
| `codex_transport` | 子プロセス起動、初期化、stdin/stdout、JSON-RPC ID、通知配信、終了監視 | `runtime.rs`の純粋なtransport処理、`runtime/wire.rs` | HTTP route、Proxy設定型、Proxyの実行policy選択 |
| `codex_execution` | thread/turnと依頼の対応、モデル、Steer/interrupt、実行排他、監視 | `v2/engine.rs`、`v2/coordination.rs`のCodex固有部分 | ProxyのHTTP応答型・クライアント別認証 |
| `approval` | 実call・引数・表示証跡・拒否・単発返信・失効 | `approval_manager.rs`、`v2/interactions.rs`等の上流対応付け | Discordコンポーネント描画と旧0.5/0.6 HTTP互換API |
| `content` | 最終回答、画像、成果物の不変保存、容量、保持 | `v2/images.rs`、`v2/files.rs`、`v2/retention.rs`の安全な処理 | HTTP Range／lease APIをそのまま公開する実装 |
| `store` | Gateway SQLite migration、トランザクション、外部ファイルとの整合 | Proxy v2 store／backupの規則 | Proxy DBをそのままGateway DBとして開く処理 |
| `discord`／`delivery` | 本人認可、メッセージ、添付、配信照合 | 既存Gateway実装 | Codex RPCの解析 |

コピーするコードは、既存Proxyに将来のGateway仕様を追加しなくても保守できる形へ移す。同じ修正が両製品に必要になった時だけ、通信層の小さい共通crate化を検討する。Proxy製品全体をGatewayのRust依存にしない。

## 3. Codex transportの契約

- Gatewayの設定に基づきCodexコマンドを起動し、stdin/stdoutをパイプする。stdoutの一行をJSON-RPCとして読む。プロトコル出力とログを混同しない。
- methodを先に判定し、App Server起点の要求・通知を、Gateway起点の要求への応答と混同しない。IDは上流の表現を保持し、`result:null`も成功として扱う。
- 通知の消費者が遅い／欠落した場合は成功として捨てず、対象Turnの照会・UNKNOWN処理へ通知する。子プロセス退出・パイプ切断はSupervisorへ送る。
- transportは任意の上流承認を自動で受諾しない。返信はapprovalが確定した実call IDと内容だけを渡す。
- 起動時はバージョンと必要なメソッドを確認する。対応不明ならreadyにせず、既存のDiscord配信や停止状況の確認は可能な範囲で維持する。
- プロセスの生存と応答可能性を分ける。initialize、Turnの送信・応答、承認返信、stdoutの読取りには独立した有限の待機と監視を設ける。単なる処理中・モデルの長い思考をstdout停止と誤判定しないよう、待機中の対象状態照会と監視期限を組み合わせる。期限切れ時は状態と送信境界をExecution／Approvalへ通知し、transport単独で再送・成功化しない。

App Serverの実行数・設定隔離はtransportの単体試験直後に実機で検証する。ひとつの子プロセスに2つのRunがある場合、MCP設定・承認方針・Codex homeのグローバル状態が交差しないことが条件。異なる会話・モデル・承認待ちを重ね、通知と停止対象も照合する。満たせなければRunごとの子プロセス等へ設計を確定し直してから上位機能を移植する。複数子プロセスでも外部HTTPサービスは作らない。

実装初期の検証では、単一App Serverで2つの実Turnを同時実行し、thread／turn／回答の混線がないことを確認した。また、子プロセスを終了して新しい子プロセスから同じCodex threadを再開し、後続Turnを完了できた。一方、単一プロセス内でMCP設定・承認方針まで隔離できる証拠はまだない。製品構成は**同時最大2つ、Runごとに専属子プロセス**とし、プロセス全体の設定状態をRun間で共有しない。会話継続では保存済みthreadを新しい子プロセスからresumeする。これは実行状態の永続化や承認の復旧を保証したという意味ではなく、それぞれ別の受入試験を要する。

## 4. 実行と永続化の境界

1. Discordから本人・場所を検証し、Message IDで受付を永続化する。本文・添付の同一性を確認する。
   入力の検証結果は既存Gatewayの受付用形式で保持し、送信直前にApp Serverの`UserInput`（`text`／`image`）へ厳密に変換する。Proxy向け`role`／`content`形式をそのまま`turn/start`へ渡さない。変換不能な添付は送信前に拒否し、推測で別種の入力へ変えない。
   本人・Guild・チャンネルを検証したメッセージは、添付のネットワーク検証より先にMessage IDで予約する。同一イベントの再配信は既存予約を返し、新たなCodex依頼を作らない。検証失敗は予約を拒否状態へ確定させ、後着のworkerが送信できないようにする。
2. Schedulerが同一会話1、全体2の枠を取得し、Codex実行先とモデルを確定する。受付順とworkの排他をDBで確認する。
3. thread/turnへ送る前に、対象、入力digest、送信意思をcommitする。境界を越えた後のcrashでは「送信されなかった」と推測しない。
4. Codex transportから得たthread/turn IDと通知を実行記録へ反映する。SSE/HTTPイベントへ変換する必要はない。進捗表示は揮発してもよいが、確定状態と承認要求は復旧可能にする。
5. 終了は実行結果、最終回答の保存結果、成果物／画像保存結果、Discord配信結果をそれぞれ記録する。保存失敗は成功したTurnを再実行する理由にしない。

内部呼出しに置き換えても、DB commit→実行送信間の障害は残る。App Serverの照会から「絶対に送信していない」と証明できない状態はUNKNOWNとする。停止／取消／承認は新規Turn枠の外で処理する。

ハング時の扱いは送信境界で分ける。initialize中の停止はruntimeをreadyにせず、新規Turnの送信を禁止する。Turn開始要求の書込み後に応答が止まれば、到達を推測せず対象依頼をUNKNOWNとして照会・保護する。承認返信後の停止はその承認操作を結果不明として同じ上流要求へ照会し、許可を別IDで送らない。stdout詰まりや通知欠落では影響したTurnを照会し、確定できないものだけUNKNOWNへ置く。いずれもハングした子プロセスをkillして元の依頼を再実行することはしない。

## 5. 承認と制御

Codex transportは上流の承認要求を `ApprovalRequest { app_server_request_id, thread_id, turn_id, call_id, full_arguments, definition_generation }` のような論理型で渡す。approvalが保存した対象とDiscordの表示証跡を照合し、本人の明示選択を得てからtransportへ返信する。実際の上流形式に存在しない項目を捏造せず、欠ける場合は必要なイベント連結を実証する。

実装では上流要求IDを文字列／数値のまま保持し、現在のRunのthread／turnと照合する。MCP elicitationで上流にturn IDが無い場合のみ、Run専属子プロセスで一致したthreadへ紐付ける。要求本文とfingerprintは承認の本人向け表示へ渡すが、通常の公開投稿やログへ生の引数を出さない。単発のコマンド／ファイル変更承認は、上流が提示した選択肢だけを返信する。MCP入力・権限要求・動的ツールには別の返信schemaを適用し、単発承認の`decision`を流用しない。

本人向け承認詳細は、Discordの1メッセージ本文上限で打ち切らず、実引数の全文をUTF-8境界で固定長のページに分ける。各ページは本人限定のinteraction responseで表示し、ページ番号と前後ボタンを付ける。ページ操作では同じinteraction card、本人、会話、fingerprintを再照合し、先頭から順に全ページの表示が成功した事実をcardの揮発状態へ記録する。最終ページまで表示する前は、上流の`accept`が可能でも許可ボタンを出さず、古いボタンからの許可もサーバー側で拒む。拒否はページ閲覧と独立して常に利用可能とする。cardの解決・Steer・Run終了時はページ状態も破棄し、内容をGateway DBや公開Discord投稿へ保存しない。

MCPツール確認が `mcpServer/elicitation/request` の `mode=form`・空のobject schemaとして届く場合は、`_meta.codex_approval_kind=mcp_tool_call`、serverName、message、実引数、現在のRunのthread／turnを検証する。本人限定画面には上流の操作文面と実引数を表示し、実引数全体を表示できない場合は許可ボタンを出さない。完全に確認した一つのRPC IDへの「今回だけ許可」は `{ "action":"accept", "content":{} }`、「拒否」は `{ "action":"decline", "content":null }` を返信する。`persist` の提示候補は表示・判断の参考に留め、返信に永続許可指定を含めない。形式不明の要求に対するJSON-RPCエラーを利用者の「拒否」と表示しない。既存の送信意思・fingerprint・Run照合・UNKNOWN保護を同じく適用する。

許可可能性と拒否可能性は別々に判定する。MCP elicitationの拒否は実引数やschemaを完全に取得できなくても同じ上流RPC要求へ`decline`を返せる。コマンド／ファイル承認では上流の`availableDecisions`を使い、提示されない許可をボタンにしない。回答schemaが未対応でJSON-RPCエラーしか返せない形式には「未対応の確認を終了」と表示し、実際の拒否成功とは区別する。

未知のApp Server要求methodは、上流RPC IDに対し`-32601`を一度だけ返す。返信結果が不明ならRunをUNKNOWNとして保護する。既知の承認要求をCodexが`serverRequest/resolved`で解決したらDBを更新し、同じ要求のDiscord承認カードを無効化してボタンを除く。画面更新の失敗だけを理由にAIを再実行しない。

依頼中の同種ツール許可は、App Serverのセッション永続許可をそのまま使わず、GatewayがRun actor内で保持する。App Serverのセッション許可は実機で同一ツールの連続確認を減らせると確認したが、同じTurnへのSteer時に取り消す契約がないためである。actorは`item/started`の`mcpToolCall`からサーバー・ツール・実引数・item IDを取り、同一Turnの`mcpServer/elicitation/request`のサーバー・実引数と一意に照合する。確認文面や到着順だけからツールを決めない。選択は永続監査へ記録し、適用中の許可集合は揮発状態とする。後続の各実callは別々の上流RPC IDと送信意思を保存してから`accept`し、再送しない。`browser_run_code_unsafe`、任意コード評価・実行、秘密入力等の個別確認必須操作を候補から除く。Steer・停止・取消をactorが受け付けた時点で許可集合を失効させ、Run終了・プロセス障害では破棄する。証拠が曖昧なら通常の個別確認へ戻す。

動的ツール呼出し`item/tool/call`は現行Codex schemaで`itemId`ではなく`callId`を持つ。上流要求IDと`callId`の両方を照合し、実引数の表示と実行結果の対応付けに使う。methodごとの必須フィールドを共通形と推測せず、稼働バイナリのschemaで確認する。

`direct_interactions`はGateway依頼ID、上流RPC ID、method、thread／turn、表示fingerprint、提示された判断候補、返信状態を保存する。判断を送る前に`PENDING→SENDING`をcommitし、送信結果が不明なら`UNKNOWN`として再送しない。Gateway再起動で旧子プロセスに紐付く`PENDING`は`UNAVAILABLE`、`SENDING`／未解決`SENT`は`UNKNOWN`へ移し、同じ上流RPC IDへの自動再返信を禁止する。上流の`serverRequest/resolved`で同じ要求を照合できた場合だけ解決へ訂正する。完全な実引数は現状メモリ上の表示用データであり、再起動後に古い承認を再表示・再許可する根拠にはしない。

拒否は表示取得に依存しない。Steerは新しい利用者入力世代を作り、以前の依頼中許可を失効させてから上流へ送る。/stopは待機列のpauseとactive Turnへの中断を区別する。/cancelは現行仕様どおり、直近待機1件、なければactive依頼を対象とし、queue全体をpauseしない。返信送信結果不明時は同一上流要求を調べ、新しい承認として再送しない。

直接接続での`/stop`・`/cancel`・Steerは、Discord操作ID、Gateway依頼ID、Codex thread／turn IDを同じDB transactionで固定してから上流へ送る。送信意思を記録した後の通信失敗は`UNKNOWN`であり、Discordイベント再配信による再送は禁止する。すでに終端したTurnへの制御や、停止要求後のSteerも送信前に拒否する。中断RPCの受付とTurnの中断完了は別の状態として表示する。

生きているTurnはRun単位のactorが専属子プロセスと一緒に所有する。actorが上流通知、承認要求、Discordからの制御コマンドを直列化し、承認UIへは上流要求IDと完全な操作内容を含む内部イベントを渡す。終端通知を取り落としても同じthread／turnを照会する。照会だけが遅いときは実行を再送せず、子プロセスの切断・不正な要求では対象依頼をUNKNOWNへ保護する。actorの異常終了もSupervisorが検知し、RUNNINGのまま放置しない。

actorから表示層へのイベント送信は非待機とし、表示層の停止で承認・停止の処理を詰まらせない。Codex終端は確認できても回答保存を一定期間確定できない場合は結果不明として保護する。保存済みの終端結果に対してDiscord配信だけが失敗した場合は配信待ちとしてactorを終了し、保存済み版の復旧経路へ渡す。

意味ベース委任はapprovalへ後付けできる判断providerの一つとする。標準の手動承認を動かしてから要件を再評価する。LLM、対象証拠、委任範囲、サイト固有知識をCodex transportやDiscord adapterへ組み込まない。

## 6. 回答・画像・成果物

Content storeは保存物とメタデータをGateway管理領域へ書き、確定回答・不変画像・不変成果物をそれぞれIDで参照する。元のCodex出力が後で変更されても、保存済み版を別内容へ差し替えない。安全な原本読取り、特殊ファイル・リンク・サイズ・ハッシュ検証、容量予約、保持期限、バックアップの対象を設計する。Discordの投稿上限や送信先をContent storeへ入れない。

確定回答テキストは `state_dir/direct-answers/<Gateway依頼ID>.txt` に排他的に保存し、ハッシュと容量をSQLiteの `direct_answers` に記録する。ファイルの同期が済んでから、DBでTurn終端を確定する。ファイルだけ残る障害は再照合対象とし、回答本文を失ったのに「配信済み」としない。保存済み版と異なる内容で同じ依頼IDを上書きしない。画像と一般成果物も別ディレクトリに不変保存し、DBの参照・バックアップ検証・Discord配信状態を分ける。内部試験だけで製品受入としない。

Discordへ再配信する際は、依頼の会話IDと送信先を照合し、`direct_answers` の容量・ハッシュを確認して同じ保存版を読む。既存の `deliveries` の送信意思・nonce・照合を利用し、送信不明を理由にCodex Turnを再実行しない。画像だけの回答は空のテキスト投稿を作らず、画像の独立配信経路へ渡す。

画像の自動配信は、そのTurnに帰属すると確認できた生成結果だけを対象にする。未発見と「画像なし確定」を区別する。回答本文が空でも画像を配信できる。Discord送信結果不明は元の保存版・送信先・メッセージIDで照合する。キャッシュを失っても期限内の保存版から復旧し、Codexを再実行しない。

一般成果物は、Codexに公開する登録用dynamic toolの実callを、その専属Runのthread／turn／会話workspaceへGateway内部で拘束する。モデルにはworkspace IDや認証情報を渡さない。相対パスの原本をdescriptor-relativeに開き、リンク・mount逸脱・特殊ファイル・変更中の原本を拒否し、容量内の内容を不変保存してから登録成功を返す。重複call IDは同じ登録記録を照合し、別内容へ差し替えない。利用者の `/get` はこの会話の登録済み成果物一覧、または明示した相対パスの安全な保存版を扱う。回答文中のパスやワーク全体の走査結果を送信指示とみなさない。Discord送信意思、nonce、送信結果は成果物保存状態から独立して永続化し、不明な送信を自動再試行しない。

実装中の直結経路では、`thread/read`が対象Turnの`itemsView=full`を返した場合に限り、`imageGeneration`項目のID・順序・PNGヘッダー・容量を検証する。保存済みPNGは`state_dir/direct-images`に不変ファイルとして置き、`direct_image_inventories`と`direct_generated_images`へ結果を保存してから依頼の終端を確定する。画像の登録失敗は個別の`UNKNOWN`として残し、回答の実行結果を書き換えない。バックアップ形式2にはDBが参照する確定回答・画像・一般成果物を含める。Discord配信は保存物ごとに送信意思とnonceを永続化し、HTTP応答を失った場合は既存投稿を照合するまで再アップロードしない。模擬Discordでは経路を接続済みだが、実Discord表示・再起動を含む製品受入は未実施。

## 7. 設定・保存場所・運用

Gatewayの設定正本は `~/.config/codex-hoshikage-gateway/config.toml`。現在の `[proxy]`を最終的に削除し、専属Codexコマンド、Codex home、作業先・権限、保存容量・保持、モデルの設定へ移す。既存設定からの切替手順は、保存済み依頼の隔離方法を確認して確定する。設定検証は子プロセス起動前に行い、state_dirや認証先の稼働中切替を許さない。

直接接続用の設定型は既存Proxy設定型と分離する。`[codex]`には絶対パスの`command`と`home`を置き、Gatewayが`app-server`をstdioで起動する。`default_model`と`model_provider`、`sandbox`、`approval_policy`を明示し、初期値は`workspace-write`／`on-request`、`network_access`はfalseとする。`thread/start`の`sandbox`と`approvalPolicy`はこのCLIが受け付けるkebab-case、`turn/start`の`sandboxPolicy.type`はcamelCaseへ変換する。Turn開始時にはworkspaceを明示したsandbox policyを渡し、Codex home側の設定とも整合を検証する。Codex homeはGateway専属とし、認証・MCP設定をそこへ配置する。設定ファイルや認証を旧Proxyの領域から暗黙にコピーしない。移行中は旧設定の読取りを維持するが、直接接続の設定に`[proxy]`を要求しない。新旧のどちらを起動するかは設定型で一意にし、一つの依頼を両方へ送らない。

モデル一覧と選択値の検証は、実行中Turnの2枠を占有しない専用の短命App Server子プロセスで行う。`/model` の引数省略時は本人限定の選択メニューを表示し、選択イベントでもGuild・本人・会話を照合する。選択値は新しいcatalogで再検証し、その選択イベントIDをSQLiteの順序・一意性判定に使う。検証完了が前後しても古い選択が新しい選択を上書きしない。Discordの選択肢上限を超える候補は `/model id:` の手入力経路を案内する。選択値は新規Turnの送信境界で固定し、既存会話のCodex threadを継続したまま次のTurnに渡す。

プロトコルの起動順序と設定値は[Codex App Server公式資料](https://developers.openai.com/codex/app-server/)と[Codex設定資料](https://developers.openai.com/codex/config-reference/)を基準にし、稼働バイナリのschemaと実機試験で照合する。Gatewayの`[codex]`はGateway専用の運用設定であり、Codex自身の`$CODEX_HOME/config.toml`とは別ファイルである。

通常の会話ワークはGateway設定の `codex.workspace_root/<Discord会話ID>` に自動作成する。未設定時は従来どおり `state_dir/workspaces/<Discord会話ID>` を使う。IDは数字として検証し、実パス・所有者・inodeを保存時と送信直前に照合する。既存会話ではDBに固定済みのパスを優先し、設定変更で自動移動・推測再割当をしない。`/workspace` は現在の会話で固定した実パス、または新規会話に適用する親ディレクトリを本人限定で示す。Discordからcwdを入力させない。共有ワークを利用する機能を将来追加する場合も、明示操作と別の排他・認可設計なしに既定ワークを共有しない。

GatewayのSQLiteと保存ファイルは管理対象を一緒にバックアップする。バックアップmanifestと復元世代を更新し、復元直後は未完了依頼を隔離する。子プロセスが失われてもCodexの実行状態が確定したと推測しない。systemd user serviceはGatewayを監視し、Gatewayは子プロセスを監視する。子プロセス異常を静かに放置しない。

バックアップ形式2はSQLite snapshotに加え、DBで参照される不変の確定回答ファイルをmanifestの容量・SHA-256と一緒に格納する。復元はDBより先に欠けた保存版を検証付きで配置し、同じ依頼IDの既存ファイルが異なる場合は上書きせず保留する。旧形式1は回答ファイルを参照しないDBに限り受け付ける。会話ワークは可変なので、実行中の整合したバックアップと復元方針を別途確定するまで、新構成の運用受入は完了しない。

子プロセスはGatewayのsystemd user unitの制御グループから逃がさない。現在の配布unitには `KillMode=control-group` がある。通常停止ではGatewayが子プロセスへ停止を要求して終了を待ち、期限内に終わらなければ同じ管理範囲の子孫を回収する。GatewayがSIGKILL等で後処理できない場合はsystemdの制御グループ単位の回収を利用する。ただし設定の記載だけで保証済みとはせず、旧PIDと子孫の消滅を実際の異常終了・再起動試験で確認する。起動時に以前の所有子プロセスの残存や同じ保存領域の二重所有を検出したらreadyにせず、新しいTurnを開始しない。`kill_on_drop` だけを異常終了時の回収保証にはしない。

## 8. 移行の順序

1. 既存GatewayのProxy呼出しと、Proxyから取り込むCodexコードの依存を棚卸しする。現行のユーザー操作を受入表へ対応付ける。
2. 模擬App Serverによるtransport／承認／中断／ハングの試験を作り、専属子プロセスの起動・停止・異常終了時の回収を隔離環境で確認する。
3. その直後に実App Serverで2並列Runと設定・承認の隔離を試験し、1子プロセスか複数子プロセスかを確定する。
4. 実行・状態管理、回答／成果物保存を段階的に内部呼出しへ置換する。現行常駐サービスの設定は変えず、試験用state_dirとCodex homeを使用する。
5. 旧サービスを停止し、旧state_dirのlockが解放されたことを確認して、DBと保存物の整合バックアップを作成・検証する。同じDBを直接方式へ変換する経路では、依頼・受付・成果物配信・制御に未終端状態があれば拒否する。変換できる場合だけ実行方式を`proxy`から`direct`へ一度だけ変更し、旧サービスからの再オープンを拒否する。旧状態に未確定配信が残る場合は、別の新state_dirに直接方式DBを初期化する経路を使える。この場合、検証済み旧バックアップを参照用アーカイブとして保持し、未確定件数と所在を新DBに監査記録する。旧レコードの状態を改ざんせず、新方式から旧依頼を再送しない。どちらの経路でも切替後の発言は新しいCodex文脈で開始し、同じBotの旧新サービスを同時稼働させない。
6. 英日README・導入・利用者マニュアルと設定例を新構成へ改める。Proxyが必須と書かれたまま新方式を配布しない。
7. 実Codex・実Discordで主要受入を通してから、常駐Gatewayの設定とバイナリを切り替える。旧Proxyサービス自体は他クライアントのために維持できる。

切替が完了するまでは現行GatewayのProxy経由動作が正本。新旧方式を同じ依頼に二重送信しない。

## 9. 内部APIの約束

型名は設計用。実装時はCodexの現行プロトコルとの対応を試験して確定する。

| 境界 | 受け渡す情報 | 呼出し側がしてはいけないこと |
| --- | --- | --- |
| Application → Execution | Gateway request ID、会話ID、入力参照、モデル、入力世代、送信意思の版 | JSON-RPCの生パラメータを組み立てること |
| Execution → Transport | Codex thread／turnと検証済みの上流メソッド・パラメータ、実行相関ID | DiscordのIDや表示文を上流へ意味なく渡すこと |
| Transport → Execution | 上流応答、通知、切断、子プロセスの状態、上流要求ID | 通知から勝手にDBをCOMPLETEDへ更新すること |
| Transport → Approval | 上流の承認・入力要求のID、Turn相関、完全な内容、受信順序 | 内容の意味を判定して自動承認すること |
| Approval → Transport | 同じ上流要求IDへの、確認済みの返信・拒否 | Discordボタンの文字列だけで返信を作ること |
| Execution／Approval → Store | 状態遷移と期待版、送信意思、監査情報 | 外部I/Oを開いたDB transaction内で待つこと |
| Execution → Content | 実Codex出力の帰属、確定本文・画像・成果物の保存要求 | Discord送信の成否から保存済み内容を改変すること |
| Application → Discord | 許可された表示モデル、配信先、同じ保存版のID | Discord送信失敗をCodex実行失敗と見なすこと |

上流のサーバー要求IDは任意のJSON IDを保持する。Gatewayの依頼IDと同一視しない。内部イベントは宛先を決めるための相関IDと、確認できた上流事実を含む。表示テキストはDiscord層で作り、モデルの発言と運用メッセージを区別する。

## 10. 現行コードからの移行単位

- `src/proxy.rs`／`src/proxy_v2.rs`は現在のHTTP境界であり、目標構成では廃止対象。関数呼出しの置換先を1件ずつ調べる。`src/application.rs`のSchedulerや配信には、既存の状態遷移規則を残す。
- `src/mcp_v06_ui.rs`、`src/mcp_grants.rs`などの画面／配信ロジックはそのままCodex transportへ移さない。Proxy応答型への依存だけをapproval内部型へ差し替える。
- Proxy `src/runtime.rs`にはstdio通信と、Proxy固有のCodex設定更新・policy thread拘束が同居する。前者をtransportへ、後者の必要部分をExecution／Approvalへ分けて移す。単一ファイルの丸ごと移植をしない。
- Proxy `src/v2/store.rs`はProxy製品のinstance／generationとJSON recordsを管理する。Gatewayの既存SQLiteへ単純に接続して流用せず、Gatewayの状態正本に合わせて必要な不変条件だけ取り入れる。
- Proxy `src/v2/engine.rs`と画像・成果物処理は、Codex上流の手順と成果物の安全な保存を参照する。HTTPレスポンス、lease API、Proxy固有IDはGateway内部の必須APIにしない。

機能を移したあとのProxyソースをGatewayのビルド時依存にはしない。移植箇所には元のファイルと取り込んだ振る舞いを開発記録へ残し、双方の修正が必要な不具合の追跡を可能にする。両製品の共有化は、本当に同じプロトコル実装を長期維持すると判断した時点で小さなcrateとして検討する。
