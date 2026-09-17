# Playwright MCP Transport closed 調査記録

2026-09-17。承認の待機時間不具合とは分離して調査する。コード・設定・サービスはこの調査で変更していない。

## 実Discordでの承認受入

利用者が21:16にbrowser_tabs(action=list)を2回要求し、21:19に依頼中許可1件で2回の実行完了回答を確認した。利用者による実画面の確認として記録する。

## 次の依頼の失敗

- Gateway request: `c26f6333-57f0-44bd-87e8-66e994158807`
- Proxy response: `resp_c06f377a-1da2-4057-a188-f86e057917af`
- Codex thread: `01a0a75d-0144-74f0-8383-89d4e4894e3c`
- Turn: `01a0af4f-cc84-7ac3-a662-5ff5921afce9`
- 接続設定: Playwright Streamable HTTP `http://192.168.0.220:8931/mcp`

時刻はJST。Codex rolloutとlogs_2.sqliteを読み取り専用で照合した。

| 時刻 | 証拠 |
| --- | --- |
| 21:20:52.157 | browser_tabs(action=list)呼出し |
| 21:21:00 | rmcp::transport::common::client_side_sse に `sse stream error: body error: HTTP request failed: error decoding response body` |
| 21:21:02.694 | browser_tabsの結果受領 |
| 21:21:07.293 | browser_navigateでSHOWROOMトップを要求 |
| 21:21:07.615 | `playwright/browser_navigate` が `Transport closed` を返却 |

SSEエラーのログ単独にはserver_name/thread_idがなく、特定のHTTPセッションとの対応は未確定。browser_navigateの失敗は該当Turnのtool出力およびcodex_core::mcp_tool_callログで確定している。GatewayのCOMPLETEDは失敗を説明するAI回答の完了であり、星集めの成功を意味しない。

## 現在確認できる範囲

- 調査時、接続先/mcpはHTTP400を返す。MCP初期化ではない単純GETへの応答なので、正常なMCP実行を証明するものではない。
- 接続先Windowsの8931番リスナーはnode.exe PID42192。調査時点で存在するが、過去のHTTPセッションの健全性は証明できない。
- 接続先へのSSHはWSLへ到達する。Windowsプロセスのコマンドラインは照会でnullとなり、サービス起動設定・サーバーログの取得には至っていない。
- Proxyのjournalには別途、過去のcall IDに対する `Custom tool call output is missing` がある。今回の通信切断との因果関係は未確定。

## Proxy側への調査依頼

1. logs_2.sqliteの21:21:00のSSEエラーを実行用MCP接続へ対応付ける。catalog取得用・実行用接続を区別する。
2. 21:20:37〜38に記録されたMCP client cancel/session deleteが実行用接続に影響していないか確認する。ログ上の近接だけで因果関係を断定しない。
3. 接続先Playwrightのログと照合し、切断元と再接続の可否を特定する。
4. 切断後、同一Runで閉じたクライアントを使い続ける経路がないか確認する。送達不明のツールを自動再実行しない。
5. タブ一覧だけでなく、隔離した検証用ブラウザーで通常のページ移動まで含めて再試験する。利用者の既存タブやログイン状態を破棄しない。

Gatewayが直接MCP接続やブラウザーを所有する対処は行わない。現時点でGateway固有の修正が必要と判明したわけではなく、ProxyまたはPlaywright担当の過失も断定していない。

## 追加診断手順

既存の実Proxy試験にtransportケースを追加する。実Proxy・隔離Gateway DB・模擬Discordを経由し、タブ一覧→SHOWROOMトップへの移動→depth=3の画面取得→タブ一覧を順次実行する。操作ごとにサーバー名・ツール名・実引数を照合し、単発許可のみを送る。再試行・ログイン操作・クリックは行わない。COMPLETEDだけを成功と判定せず、該当Codex rolloutの実ツール結果を別途照合する。既存ログイン状態は維持する。

## 追加調査結果（21:34〜21:43 JST）

- 担当を跨いでこちらから調査継続。Playwright直接接続でSHOWROOMトップ移動・画面取得に成功し、ログイン済み表示を確認。認証情報の読取り・変更なし。
- 実Proxy＋隔離Gateway＋模擬Discordの新規会話で4操作成功。Response `resp_86734036-62b1-44e5-bd04-1ed63193028d`。同じexec内でツールのisErrorを検査し、すべて成功した場合のマーカーを確認。
- 次に各操作を別execへ分け、新規会話とその継続依頼を試験。Response `resp_a98aff5b-ff49-4d41-8c1c-2ef5d08c6080` / `resp_077e9832-968e-4dd5-ab9a-34299bd13890`。同一thread `01a0af5f-8da5-7213-9015-2b398d8962b0`のrolloutで8件の実ツール応答を照合し、isErrorなし・Page/Resultが存在することを確認。許可は単発のみ。
- 実Discordの元の会話でも再依頼 `resp_30d3a0fe-b9bd-4dbc-8d68-33019403eb93` が開始。21:42:44にSHOWROOM移動成功、21:42:48に画面取得成功、21:43:00にオンライブ一覧へ移動成功。元の失敗と同じ会話でも、現時点では接続が回復している。
- 接続先のstart.ps1はheadless Chrome、isolated、storage-state読み込みを指定。既存ブラウザーのログイン状態を変更せず検証した。Proxy/Gateway/Playwrightの再起動・設定変更・製品コード変更は実施していない。

根本原因は未確定。今回の成功を恒久修正済みとは扱わない。原障害とSSEエラーの対応付け・切断側を確定できるサーバーログが不足する。

別件のUI診断では、配信ルームのミッションボタンをギフト領域が覆いクリックが時間切れになった。検証用ブラウザーを1440×1000へ広げるとミッションを開け、6/20表示を確認。変更前後の視聴数は未測定のため、この試験で数値を増やしたとは断定しない。課金・ギフト送信・報酬受取は行っていない。

## 本人向け承認画面の別障害（21:44〜）

実Discordで「自分だけに表示して確認」の初回押下が失敗し、再押下では詳細が一瞬表示された後、案内文とボタンに変わった。GatewayのDBはrequester READYだったが、これは最終的な画面本文の残存を保証していなかった。

確定したGatewayの不具合：deferred応答後の最初のfollowupが元の応答になり、その後のPATCH @originalで詳細を案内文に上書きしていた。[Discord公式仕様](https://docs.discord.com/developers/interactions/receiving-and-responding#followup-messages)を確認。最初の詳細を明示的なPATCH @originalで表示し、後続更新はcomponentsだけとする。モックはdeferred応答・PATCHのマージ・interactionごとの別メッセージIDを再現し、単一／複数ページの最終本文残存を検査する。

30秒程度で本人向けボタンと再確認が切り替わるとの利用者報告もあり、監視処理がcatalog_loading/failedをそのまま画面へ反映する経路を確認。期限内・同revisionの正常な直前画面を一時的なカタログ更新で置換しない。更新待ち監視上限・押下時の再取得と照合は維持する。

初回表示失敗の内部エラーは旧版が分類ログを残しておらず、当時の原因を確定できない。本人向け表示経路にも更新待ちの有限再取得が欠けていたため追加する。表示操作の失敗メッセージは「この操作で許可は送っていない」と再表示手順を案内する。

本人向け応答を一時ファイルで型・レンダリング検証したところ成功。診断用の応答ファイル・一時テストは検証後に削除した。引数本文をリポジトリに保存していない。

修正後、承認UI53件＋契約型15件の計68件が成功。Clippy全target・format・diff検査も成功。本人向けの各ページ本文が最終PATCH後も残ること、一時的なカタログ更新で既存の入口が変わらないこと、再取得失敗時も許可未送信であることを含む。実Discordでの修正版の見え方は別途確認する。

22:00:16 JST、実行中依頼・holdがないことを確認してGatewayのみ再起動。配備バイナリSHA-256は `5c5cc3dfaaa63836dac0834e3f0ede31d02242edd67b1cbb99119709550e76f8`。今回の改修は未コミット。修正版での実Discord表示確認は利用者へ依頼する。

初回起動でDiscord接続成立前にcontrolタスク終了を検出し、systemdが22:00:41に自動再起動した。その後Discord connected=true、Proxy ready=true、recovery_pending=false、holdなしを確認。起動時接続失敗の詳細原因は未確定であり、MCPの切断と同一原因とは断定しない。
