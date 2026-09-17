# MCP API 0.6：内容確認中の30秒経過で承認できなくなる問題

2026-09-17。実Discordで再現、Proxy修正・再接続試験待ち。Gatewayの入力検証を緩めて迂回しない。

## 実測

20:24にbrowser_tabs(action=list)の公開カードと3択が表示され、20:25に「この依頼中、このツールを許可」を選ぶとGatewayが再照会を検証できず失敗。

対象interaction: `int_746fe24c54fbc9683d18d68dfad10d0c402874d92c1e96cdd76b72a90540becd`。

- 初回：public ready、操作と全引数、allow_once/allow_turn_tool。
- 後の同一presentation GET：private_required、reason=privacy_unclassified、diagnostic.code=null、presentation_id=null、page=null。
- Gatewayのmcp_interactions.operation_keyはnull、対応するmcp_v06_decisionsもなし。観測時点でProxyへの許可送信に達していない。

## 原因となる実装

Proxy `src/v2/catalog.rs::peek` はReadyの取得完了から30秒でLoadingを返す。HTTP入口の `api.rs::refresh_v06_catalog` は再取得を要求して有界待機するが、待機結果を捨てて表示生成へ進む。`src/v2/approval_v06.rs::evaluation` はその時点のpeekを参照するため、更新がまだLoadingなら未評価へ落ち、snapshotの公開判定もUnclassifiedとなる。したがって30秒経過で必ず失敗するのではなく、更新の待機・成否と押下時刻の組合せで失敗する。

snapshotのprivate_required分岐はreasonのみ設定しdiagnostic.codeを更新していない。ページを作らない入口応答がpresentation_id/pageともnullのまま返り、合意済みGateway型と不整合。カタログ問題の修正とは別に、この分岐も修正が必要。

## 利用者の追加確定事項

承認の判断に30秒の制限を設けない。内容を考える・離席する時間と、内部キャッシュTTLを分離する。待機中に必要な内部情報を更新し、押下時に同一操作・現在の権限を検証する。同一内容なら時間経過だけで操作を失敗させない。真の内容／権限変更があれば変更点を説明して再確認する。保存版やtokenの技術的な期限を、AI再実行や自動承認で補わない。長時間の離席で表示tokenが失効した場合も、元操作が待機中なら新しい確認画面を明示的に取得できるフローを契約化する。

## 要求

1. 既存の待機中承認のカタログ更新経路で、有界待機の結果（更新中・成功・失敗）を表示生成へ正しく伝える。更新中を個人情報検出や恒久未評価と偽らない。
2. 同一定義への正常更新で既存scope/表示の証拠を不要に失効させず、既存契約どおり再評価する。実際の定義・設定・権限変更、停止、期限切れでは旧承認を適用しない。
3. private_requiredを含む全応答のreason/diagnostic、表示ID・fingerprint・page、actionsを合意型へ合わせる。変更が必要ならコードより先に契約を調整する。
4. 更新待ちを示す機械可読な状態と再照会条件を返し、Gatewayは待機表示／明示的再確認を行う。利用者に分からない再クリックを繰り返させない。
5. AI再実行、許可の自動再送、引数の省略、単発への自動切替で補わない。

## Gateway担当

押下直前の現在情報の再検証は維持する。取得・契約検証失敗と、許可POST後の結果不明を区別して案内する。前者では許可未送信と次に行える再確認を明示し、後者では保存キーによる照会だけを行う。「元の画面を開き直す」だけでは解決しない今回の案内を修正する。Proxy契約／実装との整合後に製品UIを更新する。

## 受入

- 表示後35秒、90秒待ってから単発／依頼中許可を押し、同一定義なら成立する。
- 更新中に押す、定義変更・取得失敗・停止・再起動が競合する場合は、誤許可も二重送信もない。
- publicからprivateへの本当の変更、および初めからprivate_requiredの全JSONをGateway型で検証する。
- Gatewayの実接続試験へHOSHIKAGE_MCP_REVIEW_DELAY_SECS=35を追加済み。即時押下試験の成功を人間の確認待ちの受入に代用しない。
