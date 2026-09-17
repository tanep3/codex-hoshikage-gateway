# アーキテクチャ方針転換：Gateway専属Codex App Server

2026-09-18 / Tane Channel Technology

状態：目標構成として採用。内部の責務境界も維持する。[目標要件](requirements-direct-app-server.ja.md)・[内部設計](system-design-direct-app-server.ja.md)は作成済み。具体schema・移行方式・実装・稼働環境の切替は未完了。現行の常駐Gatewayは引き続きProxy API v2を使用する。

## 判断の理由

Gatewayの承認表示や委任、会話と配信の変更が、汎用API製品であるProxyの大きな仕様変更を繰り返し要求してきた。分離したプロセスであっても、変更時には両製品の契約・実装・配備が連動している。Gatewayに専属の実行環境を持たせ、この変更依存をGateway内へ収める。

現在のProxyは、すでにCodex App Serverをstdioの子プロセスとして起動し、双方向JSON-RPCを扱う。Gatewayはその実装の必要な部分を取り込める。Proxyを削除する判断ではない。ProxyはOpenAI互換API等を提供する独立製品として継続する。

## 目標構成

```text
systemd --user
  └─ codex-hoshikage-gateway
       ├─ Discord接続・UI・配信
       ├─ 会話・依頼・承認・成果物・復旧の状態管理
       └─ 専属 Codex App Server（stdio）

codex-hoshikage-proxy（別製品として独立稼働可能）
  └─ 必要に応じて自身の Codex App Server
```

Codex App ServerはGatewayが所有する子プロセスであり、独立した外部サービスやHTTP APIの境界としては扱わない。Gateway内部ではCodex通信、実行、承認、保存、Discord表示を別の責務として実装する。

GatewayがApp Serverの子プロセスとその入出力を所有する。Gatewayの動作にProxyのHTTP APIや常駐サービスを必須としない。別のアプリは自身の実行環境を持てる。両製品が同じホストにあっても、同一のApp ServerプロセスやDBを暗黙に共有しない。

## 責務の仕分け

| 機能 | 目標構成での扱い |
| --- | --- |
| Codex起動・stdio JSON-RPC・通知・App Server障害検知 | ProxyのCodex実装を参考にGatewayの内部モジュールへ取り込む。HTTPサーバーと結合させない |
| Discord認可・会話対応・キュー・承認画面・配信 | Gatewayに維持する |
| thread／turn開始・継続・Steer・interrupt・モデル指定 | Gatewayから専属App Serverを直接操作する。上流の制約と実行結果を忠実に扱う |
| 依頼、承認、停止、UNKNOWN、復旧、実行と配信の分離 | Gatewayの永続状態に統合する。現在の最大1回送信・不明時の自動再実行禁止を維持する |
| ワークの選択・権限・隔離 | Gateway専属Codex実行の設定・状態として再設計する。Proxyの許可設定を暗黙に継承しない |
| 確定回答、画像、成果物の保存・再取得・保持 | Gateway側に必要な機能を取り込む。Discordへの配信失敗でCodexを再実行しない |
| MCP承認の実call・実引数・上流返信の照合 | GatewayのCodex実行層に取り込む。公開表示・本人限定表示はDiscord層が担当する |
| 意味ベース承認委任 | Proxy向けの追加開発は保留。Gateway内の任意機能として要件から再評価する。現時点では製品実装未着手 |
| `/v1`互換API、OpenWebUI Pipe、他モデルProvider、HTTP認証・Capability API | Gatewayへ移さず、Proxy製品に残す |

「コードを移す」と「既存のProxy API v2をGateway内部で模倣する」は同義ではない。App Serverと直接やり取りする内部型・状態へ整理し、不要なHTTPや二重の操作記録を減らす。既存コードを利用する場合は依存するモジュール・設定・テストを確認し、Proxyの成果物や許可の動作を無検証で変えない。

## 設定とデータ

Gatewayの設定ファイルは現時点ですでに `~/.config/codex-hoshikage-gateway/config.toml` にある。新しい配置先を作るのではなく、この設定へCodexコマンド、専属のCodex設定・認証環境、ワークと権限、保存容量・期限を統合する。Proxy URL、APIキー、契約版といった必須接続設定は、切替完了後に除去する。項目名・既定値・移行手順は詳細設計で確定する。

GatewayのSQLiteと既存のProxy DBは別の正本である。実行中Turn、承認待ち、回答・画像・成果物、期限付きリース、Discord配信結果を「新しいGateway DBにすべて存在する」と仮定しない。旧実行を停止せず新方式へ付け替えたり、結果不明のものを再送したりしない。保存済み成果物の参照・回収期限を含め、移行可能な単位を調べて切替手順を決める。稼働中の旧依頼がある間は、旧経路を照会専用として残す等の方式を比較する。

ProxyとGatewayが同じCodex設定元を読む場合も、認証情報・MCP設定・セッションの所有、更新時の競合、ファイル権限を明示する。専属App Serverを作ることと、同じ認証済みアカウントを利用することは別の設計判断である。

## 維持する品質条件

- Codexへ送る依頼・許可・停止は、実際に送る前の永続記録と実callとの照合を持つ。送信結果不明を別IDで再送しない。
- App Serverの終了、stdio切断、イベント欠落、Gateway再起動でもRUNNINGを放置せず、照会可能な事実と不明を分ける。
- 承認要求は実call・完全引数・Run／入力世代へ結び付け、停止・Steer・次Runの権限を混同しない。未知操作を意味が安全と推測しない。
- 確定回答と配信状態を分ける。画像・成果物は不変の保存版と内容検証を保ち、配信失敗でAIを再実行しない。
- DiscordトークンとCodex認証情報をログ・画面へ出さない。別プロセス化で失われる権限境界は、専属子プロセスと設定・保存先の権限で再設計する。

## 詳細設計へ進む順序

1. ProxyのCodex実行・承認・保存・復旧コードの依存関係を棚卸しし、取り込む単位を選ぶ。今のGatewayが実際に使用する機能を受入表にする。
2. Gateway内部のApp Server runtime、状態正本、ワーク、成果物、設定の設計を先に確定する。意味ベース委任はこの構造の上で改めて検討する。
3. 現行Gateway／Proxyの実データと未完了依頼を調べ、移行・並走・切替・復旧の手順を設計する。
4. 要件定義書・システム設計書を全体改定し、導入・利用者資料の更新点も抽出する。現行運用の説明と目標構成を混ぜない。
5. Gateway内部へ段階的に実装して模擬App Server、実Codex、実Discordで受け入れる。設定・常駐サービスの切替は受入後に行う。

この方針はGatewayからProxyへの新たな意味ベース委任API要求を一旦止める。Proxy側で独立製品として進める開発は、この文書の変更対象ではない。
