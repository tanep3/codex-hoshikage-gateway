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
