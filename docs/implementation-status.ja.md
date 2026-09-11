# 実装・検証状況

更新日: 2026-09-11  
実装版: 0.1.0（開発版）  
基準: 要件0.8、システム設計0.4、Proxy契約1.0  
著作者: Tane Channel Technology

## 現在の到達点

Rustの実行バイナリ、SQLite Store、Proxy/Discord接続、主要コマンド、配信、ファイル処理、管理CLIを実装した。Mock DiscordとMock Proxyを使い、通常投稿の受付から生成ストリーム・成功Responseによる会話継続まで接続できることを確認した。

**初期実装が存在することと、設計全項目の受入完了・本番リリース可能という判断は区別する。** 実Discord/実Codexへの作業送信、常駐サービスの導入・変更は行っていない。既存Proxyのコード・設定・サービスも、この実装作業では変更していない。

## 実装した範囲

| 領域 | 実装内容 | 主なファイル |
| --- | --- | --- |
| 起動・終了 | CLI、明示init、インスタンス照合、critical task監視、キャンセル・期限付き終了、panicの内容非表示 | `src/main.rs` |
| 状態保存 | SQLite WAL/FULL、初期schema1、checksum/整合性確認、専用worker、要求・操作・配信ID、本文非保存 | `src/storage.rs`、`migrations/001_initial.sql` |
| 受付・実行 | 許可ユーザー/Guild/Thread照合、検証予約、期限回収、入力再取得、2並列hold、送信最大1回 | `src/application.rs` |
| 送信許可 | 毎回のready/Capability再確認、要求と設定世代に結び付いた使い切り許可、DBへの根拠保存 | `src/proxy.rs`、`src/storage.rs` |
| Proxy制御 | 生成SSE、要求/Turn現在照会、イベント監視、Approval/Interrupt/Steer、UNKNOWNの照合 | `src/proxy.rs`、`src/application.rs` |
| Discord操作 | `/new`、`/status`、`/stop`、停止ボタン、`/resume`、`/model`、`/steer`、承認ボタン、`/get` | `src/commands.rs`、`src/discord.rs` |
| 回答配信 | 途中表示、分割、既知秘密のストリームマスク、POST/PATCHのpending/confirmed分離、GET照合 | `src/delivery.rs` |
| 添付・成果物 | 静止画像/UTF-8入力、取得量の制限と一時容量予約、相対パス・descriptorに基づく成果物読取 | `src/files.rs` |
| 運用 | 本人UID限定Unix socket、status、reload、状態照合、世代付きabandon、整合backup、復元隔離/解除 | `src/admin.rs`、`src/backup.rs` |
| 配布準備 | MIT、Cargo.lock、設定例、systemd user service例、依存ライセンス一覧 | リポジトリ直下、`config/`、`packaging/` |

## 実施済みの検証

```text
cargo test --locked --tests --quiet              28 passed / 0 failed
cargo clippy --locked --all-targets -- -D warnings  passed
cargo fmt --check                                passed
```

Rust 1.98.1、現在のUbuntu環境で実行。Mock HTTPは一時的なloopbackポート、DBとファイルは一時ディレクトリを使用した。

| 試験ファイル | 件数 | 確認したこと |
| --- | ---: | --- |
| `tests/state_store.rs` | 9 | 先行検証予約の追越し禁止、SENDINGの永続化・巻戻し禁止、重複受付防止、stop/resume競合、初回失敗の後続未送信処理、期限切れworker結果の拒否、分割秘密のマスク、未知schemaの書込拒否、backup改変検出、古い停止ボタンの拒否を含む |
| `tests/proxy_contract.rs` | 6 | 503後の生成POSTが1回、404から自動再送しない、現在照会によるUNKNOWN訂正、Capability喪失、2 hold中のControl到達、UTF-8/SSE境界、要求/設定世代の送信許可照合を含む |
| `tests/recovery_files.rs` | 3 | 復元した旧待機の永久隔離、backup後に追加されたProjectの復元block、解除後の旧投稿拒否、symlink/hardlink/FIFO/範囲外/容量超過拒否、親子workspace拒否 |
| `tests/delivery.rs` | 3 | Discord POST/PATCH応答喪失後のGET確定、重複送信なし、完全な429拒否だけの待機・再試行 |
| `tests/application.rs` | 2 | 通常投稿→入力再取得→送信許可→生成POST→SSE表示内容→現在照会→会話継続。重複Discord投稿でもPOSTは1回 |
| `tests/cli.rs` | 1 | CLIのローカルcheck/init、再初期化拒否、DB保持、秘密ファイルの権限拒否と非表示 |
| `tests/admin.rs` | 1 | 私有管理socketのstatus、正式backup作成・検証、同じ出力先の上書き拒否、終了時のsocket回収 |

追加試験: `src/discord.rs` 内の2件で応答モードと設定の省略・誤値を確認。`tests/model_commands.rs` の1件で一覧・選択表示・変更・無効IDを確認。applicationには通常チャンネルからの開始、deliveryには本文だけの正常返信を追加した。

件数はテスト関数単位であり、表の各確認点と1対1ではない。テスト件数をもってT-01〜T-26やV-01〜V-07をすべて完了扱いにしない。

## リリース前に残る検証・補完

以下は新しい利用要件ではなく、合意済み設計の実装仕上げ・受入項目である。

- 実Discordの権限・Intent・Thread操作、コマンド受付とボタン、モデル切替、画像添付、成果物返送を、許可した試験環境で一連に確認する。
- 実Codexによる承認待ち、Steer、中断と完了の競合、会話途中のモデル変更、再起動後の継続を確認する。常駐Proxyを障害試験の対象にはしない。
- Supervisorのpanic/channel断/応答停止、DB容量不足、各commit境界、設定更新中の故障、backup/restore/解除中の強制終了を含む故障注入を拡充する。
- 設定不正や復元マーカーの読取不能・不一致では現在は安全側に起動拒否する。設計のローカル診断モードについて、不正条件下でも提供できる操作の範囲を補完する。
- 一部の監視周期・timeout・内部queueサイズはコード内の初期値であり、設計第15節の設定可能項目への展開とreload分類の試験が残る。容量縮小は現在、待機・hold・未配信出力がない場合に限定する。
- Control/状態/配信の混雑、route/globalのDiscord rate limit、長時間常駐、メモリ上限、保持期限、終了期限を測定する。添付と成果物は一時容量を共有予約するが、ライブラリ内部の受信/JSON/TLSバッファも含めたプロセス実測は未実施。
- 配布用releaseビルドの動的依存確認、依存ライセンス本文・NOTICEの同梱、systemd user serviceでの運用確認を行う。現時点の依存一覧はライセンス本文の代替ではない。

## 運用上の確定事項

- 生成の到達不明を自動再送しない。Discord送信失敗もCodexの再実行理由にしない。
- 復元全体の保留解除と、個別UNKNOWN hold・pauseの解除は別操作。隔離した旧依頼の送信資格は戻さない。
- UNKNOWNのholdを明示解除した後の再確認は、管理者の`admin reconcile`でも行える。確実な実行中状態が判明した場合はholdを再取得する。
- 回答本文の再取得はProxy契約1.0で非対応。Gatewayのメモリから失われた回答全文を復元できるとは表示しない。
- 著作者の追加判断により、添付の容量・件数等は設定例に標準値を記載する。初回はそのまま利用でき、必要時に調整する。0を標準値へ変換する処理は追加せず、正数・容量間の整合性検証を維持する。

## 会話UIの改定（要件0.8）

- 登録チャンネルで直接会話。旧スレッドは継続利用し、未登録場所では登録案内だけを返す。
- response_modeはall/mention。認可対象は本人・設定Guildのまま。通常回答から状態カード・完了定型文を除く。
- /modelsは一覧、/modelは選択確認、id指定は次のモデル変更。無効IDとProvider変更は専用案内。
- 上記のMock試験を追加。稼働中Botのバイナリ置換・コマンド再登録・実Discordの改定UI受入試験は未実施。従来の実会話では2件の完了をDBで確認したが、新UIの受入結果には数えない。
