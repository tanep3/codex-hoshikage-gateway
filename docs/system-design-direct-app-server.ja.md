# Gateway専属Codex App Server — 内部システム設計

版0.1 / 2026-09-18 / Tane Channel Technology

状態：目標構成の内部設計案。[目標要件](requirements-direct-app-server.ja.md)に対応。具体schema・Codex設定・旧データの移行方式は現行コードと実データの調査後に確定する。現行常駐サービスは未変更。

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

App Serverの実行数・設定隔離は実機で検証する。ひとつの子プロセスに2つのRunがある場合、MCP設定・承認方針・Codex homeのグローバル状態が交差しないことが条件。満たせなければRunごとの子プロセス等を選ぶ。複数子プロセスでも外部HTTPサービスは作らない。

## 4. 実行と永続化の境界

1. Discordから本人・場所を検証し、Message IDで受付を永続化する。本文・添付の同一性を確認する。
2. Schedulerが同一会話1、全体2の枠を取得し、Codex実行先とモデルを確定する。受付順とworkの排他をDBで確認する。
3. thread/turnへ送る前に、対象、入力digest、送信意思をcommitする。境界を越えた後のcrashでは「送信されなかった」と推測しない。
4. Codex transportから得たthread/turn IDと通知を実行記録へ反映する。SSE/HTTPイベントへ変換する必要はない。進捗表示は揮発してもよいが、確定状態と承認要求は復旧可能にする。
5. 終了は実行結果、最終回答の保存結果、成果物／画像保存結果、Discord配信結果をそれぞれ記録する。保存失敗は成功したTurnを再実行する理由にしない。

内部呼出しに置き換えても、DB commit→実行送信間の障害は残る。App Serverの照会から「絶対に送信していない」と証明できない状態はUNKNOWNとする。停止／取消／承認は新規Turn枠の外で処理する。

## 5. 承認と制御

Codex transportは上流の承認要求を `ApprovalRequest { app_server_request_id, thread_id, turn_id, call_id, full_arguments, definition_generation }` のような論理型で渡す。approvalが保存した対象とDiscordの表示証跡を照合し、本人の明示選択を得てからtransportへ返信する。実際の上流形式に存在しない項目を捏造せず、欠ける場合は必要なイベント連結を実証する。

拒否は表示取得に依存しない。Steerは新しい利用者入力世代を作り、以前の依頼中許可を失効させてから上流へ送る。/stopは待機列のpauseとactive Turnへの中断を区別する。/cancelは現行仕様どおり、直近待機1件、なければactive依頼を対象とし、queue全体をpauseしない。返信送信結果不明時は同一上流要求を調べ、新しい承認として再送しない。

意味ベース委任はapprovalへ後付けできる判断providerの一つとする。標準の手動承認を動かしてから要件を再評価する。LLM、対象証拠、委任範囲、サイト固有知識をCodex transportやDiscord adapterへ組み込まない。

## 6. 回答・画像・成果物

Content storeは保存物とメタデータをGateway管理領域へ書き、確定回答・不変画像・不変成果物をそれぞれIDで参照する。元のCodex出力が後で変更されても、保存済み版を別内容へ差し替えない。安全な原本読取り、特殊ファイル・リンク・サイズ・ハッシュ検証、容量予約、保持期限、バックアップの対象を設計する。Discordの投稿上限や送信先をContent storeへ入れない。

画像の自動配信は、そのTurnに帰属すると確認できた生成結果だけを対象にする。未発見と「画像なし確定」を区別する。回答本文が空でも画像を配信できる。Discord送信結果不明は元の保存版・送信先・メッセージIDで照合する。キャッシュを失っても期限内の保存版から復旧し、Codexを再実行しない。

## 7. 設定・保存場所・運用

Gatewayの設定正本は `~/.config/codex-hoshikage-gateway/config.toml`。現在の `[proxy]`を最終的に削除し、専属Codexコマンド、Codex home、作業先・権限、保存容量・保持、モデルの設定へ移す。新項目の名前と既定値はconfig migrationを設計してから確定する。設定検証は子プロセス起動前に行い、state_dirや認証先の稼働中切替を許さない。

GatewayのSQLiteと保存ファイルは管理対象を一緒にバックアップする。バックアップmanifestと復元世代を更新し、復元直後は未完了依頼を隔離する。子プロセスが失われてもCodexの実行状態が確定したと推測しない。systemd user serviceはGatewayを監視し、Gatewayは子プロセスを監視する。子プロセス異常を静かに放置しない。

## 8. 移行の順序

1. 既存GatewayのProxy呼出しと、Proxyから取り込むCodexコードの依存を棚卸しする。現行のユーザー操作を受入表へ対応付ける。
2. 模擬App Serverによるtransport／承認／中断の試験を作り、専属子プロセスの起動と終了を隔離環境で確認する。
3. 実行・状態管理、回答／成果物保存を段階的に内部呼出しへ置換する。現行常駐サービスの設定は変えず、試験用state_dirとCodex homeを使用する。
4. 旧Proxy DB・保存物の実データに対する移行可能性を確認する。旧実行の未終端・UNKNOWN、期限付き保存物、Discordの既存配信記録を区別し、切替方法を決める。
5. 英日README・導入・利用者マニュアルと設定例を新構成へ改める。Proxyが必須と書かれたまま新方式を配布しない。
6. 実Codex・実Discordで主要受入を通してから、常駐Gatewayの設定とバイナリを切り替える。旧Proxyサービス自体は他クライアントのために維持できる。

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
