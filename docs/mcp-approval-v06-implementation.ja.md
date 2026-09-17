# MCP承認0.6 実装詳細と進捗

2026-09-17。合意済み[API 0.6](../../codex-hoshikage-proxy/docs/mcp-approval-api-v06.ja.md)第14節のカタログ更新待ち補正までを基準にする。責務設計は[後継設計](mcp-approval-layered-gateway-design.ja.md)。以下をコードより先に確定する。常駐有効化・実接続受入とは別。


## 現在の結論：今回の改訂完了（2026-09-17）

実装・138件の回帰試験・Clippy・常駐反映に加え、利用者の実Discord試験で、承認画面を表示して時間を置いた後の依頼中許可と、browser_tabs(action=list)の2回実行・最終回答を確認した。利用者の合意により、今回の承認UI／API 0.6接続改訂は完了とする。以下の未完了表記は各時点の経過記録である。

後続のSHOWROOM操作で発生したPlaywrightのTransport closedは別件として切り分ける。今回の完了は、全MCPツールやネットワーク障害の解消を保証するものではない。[別件の調査記録](mcp-playwright-transport-incident-20260917.ja.md)に確認済み事実と未確定事項を残す。

## 型・保存・上限

- profileはsource-conversation-v3。mcp_approval_v06を独立検証し、旧details/turnスイッチを汎用単発表示の必須条件にしない。
- 要求policyと実効policyは別型。Gatewayの初期要求はevaluated-turn-notion-guard/version=1。未選択型も実装・試験し、未選択に架空generationを作らない。
- 実効policyにはselection/binding_id/generation/state/reason/restrictions/upstream_overrides/preparationを必須とする。準備は60000ms、期限とTurn送信証拠・設定隔離のenumを検証する。readyと実行開始を混同しない。
- scopeは旧全項目にexecution_policy_binding_idを加え、policy_generation/definition_generationは必須nullable。保持値を削らず実効policyと照合する。
- presentationを表示の正本にし、argument_integrity、semantic_assessment、tool_policy、actionsを独立検証する。未評価・catalog不足だけでは単発不可にしない。禁止・不足・未配信は不可。
- HTTPの展開後上限はcapability全体1MiB、単体Response/interaction256KiB、presentation/0.6operation/control64KiB、grant一覧1MiB、interaction一覧64MiB。旧profileのoperation256KiBは維持する。任意query付きpresentationも同じ上限。

schema9は追加専用。新規runのprofile/要求policy/実効policyをmcp_v06_runsへ保存する。mcp_v06_viewsは版・scope・policy binding・audience・期限・状態だけ、mcp_v06_pagesはページtokenと配信状況、mcp_v06_partsは投稿ID・送信意思・鍵付き照合値だけを持つ。mcp_v06_decisionsは固定キー・返信JSON（非本文token類のみ）・状態を持ち、interactionごとに未確定または受理済み決定は1件。拒否は表示用フィールド不要。旧mcp_interactionsにも操作キーを記録して既存監視と整合させる。本文・引数・Discord interaction tokenをDBへ保存しない。

## ページと返信

0.5で確定した公開投稿分割、本人向け全ページ確認、拒否独立、ACK先行、再起動後の再照合を継承する。rendererはraw-arguments-v1/evaluated-operation-v1で共通。UI上の未評価は本人向けを強制する理由ではなく、Proxyのaudienceに従う。

許可bodyはexpected_revision、expected_scope_fingerprint、approval_view、expected_presentation_fingerprint、expected_page_tokens、expected_policy_binding_id、response(action=accept,content={})。turnのみgrant_scope=turn_tool。拒否はexpected_revisionとresponse(action=decline)のみ。新キーで不明結果を再送しない。

## 準備と停止

永続受付202後に実効policyを照会し、preparingは待機、設定不明/not_startedはAI未開始かつ占有保持として表示する。Gateway時計による期限切れで実行失敗・占有解放を確定しない。停止は既存v2 stopsへResponse IDまたは元request_keyを指定する。設定隔離未了のwaiting_for_startを「AIの開始を待っています」と表示しない。

## 機能別の実装・検証

1. 契約型・endpoint別読取上限・schema9とStore。JSON例、矛盾したactions/binding/世代、容量・期限境界、旧版非影響を試験。
2. profile固定と要求・実効policy、準備状態・停止・復旧を接続。
3. 公開/本人向け表示・ページ配信証跡・単発/拒否/依頼中返信・取消一覧を接続。
4. モックの競合・再起動・実Proxyと許可先Discord受入。最後に英日利用者文書と配備。

1だけの実装で0.6の利用可能を宣言しない。表示・復旧を含む経路が揃ってから新規runの0.6選択を有効にする。以後の進捗は実際の試験結果に基づいて追記する。


## 接続実装の具体化

- 公開内容は最大1900 UTF-16単位/投稿に決定的分割し、最後の投稿へ許可・拒否を置く。全断片のPOST応答を確認するまでREADYにしない。公開を別の本人向け画面へ逃がさない。
- 本人向けは1ページずつ表示し、同じ会話の本人向け投稿を残す。前の内容はスクロールして再確認できるため、旧0.5案の「前へ」ボタンは設けず「次のページを確認」を使う。全ページ確認後にのみ許可を表示する。ボタンは最後に確認した画面へ拘束する。
- 部分投稿の意思をDB commitしてからDiscordへ送信する。本文の代わりに起動ごとの鍵によるHMACを記録。起動をまたぐ公開投稿は保存IDでGETし、同じProxy表示と本文を完全照合する。本人向けは新たな明示表示が必要。応答を失ったPOSTを自動で繰り返さない。
- 明示再表示は新しいviewで行う。無効な旧viewから許可できない。記憶上のviewは最大512件、期限切れを回収する。公開表示の定期照会は最短2秒。取得失敗は2/4/8/10秒へバックオフし、429の数値Retry-Afterを優先する（解析できない指定は60秒待つ）。自動再取得は保存した60秒の窓で打ち切り、GETや再起動で延長しない。「再確認」は新たな有限窓でGETだけを行う。HTTP取得自体も既存15秒timeoutで有限にする。
- 明確な未受理エラー（presentation stale/expired等、binding/policyの未成立）と応答不明を区別する。前者は旧許可を再送せず、次の明示操作・拒否を可能にする。後者は固定キーを照会し、新しいキーを発行しない。
- 表示取得は専用4枠。新規Turn枠は消費せず、停止・拒否・取消の制御枠を確保する。既存の独立期限回収ループで古い表示と再起動前の送信意思を照合する。
- 許可一覧の取得障害時は保存済みIDの取消メニューを表示し、一覧が空だと偽らない。
- 実効policyのbinding/選択/期限を固定し、確認済み世代と開始証拠を後退させない。設定隔離pendingのrejected/cancelledは契約矛盾として解放しない。失敗履歴をpreparingへ戻さない。
- 公開用viewer_idは空文字、本人用は不透明principal。どちらもscope/contextと実Discord本人・会話を別に照合する。旧0.5案のテーブル名・列制約は0.6 migrationで置換する。

## 実装状況

契約型・migration9・Store、公開/本人向けUI、実行要求のprofile/policy固定、準備照会、単発/依頼中/拒否、許可一覧・取消を接続済み。旧Runの0.3/0.4経路を保持する。Proxyの設定・サービスは変更していない。

実Proxyの0.6提供・runtime隔離・停止/Steer競合と、実Discordでの見え方・押下は未受入。実サービスへ配備済みという意味ではない。テスト結果は下に追記する。

## Gateway検証結果（2026-09-17）

- `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`git diff --check` 成功。
- 最終の `cargo test --quiet` は129件成功、失敗0、実環境用6件は既定どおり未実行。
- うち契約・Store・容量・期限・DB移行14件、承認UI/結合47件（既存33件＋0.6追加14件）。通常表示と3択、未評価単発、長文の全ページ確認、本人向け表示失敗時のACK完結、古い投稿/別人/別会話/停止後の拒否、監視不明時も拒否可能、二重押下、応答喪失、再起動、有限再照会、準備中の占有、保存済み許可の独立取消を検証した。
- ProxyのJSON例とfixtureは一致。独立した0.6 capabilityを使用し、旧スイッチOFFでも新UI・要求・取消が動くこと、既存Runが旧形式のまま扱われることをモックで確認した。
- 途中の検証では実行ファイルのPermission deniedによる起動中断が発生した。ビルドと変更を止めた後の最終一括実行は全件成功している。

残る受入は実Proxy 0.6との接続、実Codexの承認到達・停止/Steer失効・runtime隔離、実Discordの画面と押下。常駐反映・コミット・pushはこの実装作業では行っていない。

## 常駐反映・統合受入（2026-09-17）

Proxyコミット3a38734/ea823b6、常駐ready、source-conversation-v3と2 policy enabledを確認。契約JSON例とGateway fixtureは一致。Gatewayの134件回帰試験・Clippy成功版に、明示実行用の実Proxy試験の0.6操作経路を追加する。

実Proxy＋実Codex＋実Playwright＋隔離Gateway DB＋模擬Discordで、browser_tabs(action=list)を5回要求する。最初のRunでは依頼中許可、次のRunでは単発を5回選ぶ。原引数を照合してから許可し、public初回カードの操作名とボタン、許可の終端失効、次Runへの非持越しを確認する。タブの移動・作成・選択・閉鎖は実施しない。模擬Discordでの押下を実Discordの利用者操作確認とは扱わない。

常駐DBは公式backup経路でschema8・integrity=okの保存版を取得済み。配備後はschema9、service active、Proxy ready、slash command登録、旧UNKNOWNの照合と待機復旧を確認する。実Discordの操作は利用者と協力して別途記録する。

追加の実接続試験では、同じ読み取り専用依頼の承認待ちで模擬Discordの `/cancel` をcontrol loopへ送り、実Proxyの中断確定、hold解放、会話pauseが変わらないことを確認する。

### 配備・実接続の結果

- 20:04 JST、`cargo install --path . --locked` で `/home/tane/bin/codex-hoshikage-gateway` を更新し、systemd user serviceを再起動。設定ファイルは変更していない。schema8→9成功。旧完了40件による監視詰まりが解消し、旧UNKNOWNを実Proxyの中断結果へ訂正。元のAI依頼を再送していない。
- 配備バイナリSHA-256: `9ada14a789f9782d0e7a6e8bf151bc5a8233c7bb528146c881e405c9b0ab81ac`。配備前backup IDは `8f27f6f9-2b36-4837-a9a4-77525c9621d0`、integrity=ok。
- Discord connected=true、Proxy ready=true、recovery_pending=false。実Discordの登録コマンド一覧にcancelがあることをREST照会で確認。最後の運用照会で未終端依頼・holdは0件。実Discordからのcancel(active/APPLIED)と対象CANCELLED/stop_requestedも常駐DBで確認した。
- 実Proxy＋実Codex＋実Playwright＋隔離Gateway DB＋模擬Discordの試験を3ケース実施し、すべて成功。試験には任意の本番権限を増やす設定変更を使っていない。

| ケース | 確認結果 |
| --- | --- |
| once-turn | 初回カードに操作内容と3択。依頼中許可1回で5呼出し、scope_endedで無効化。次Runは単発確認5回、grantなし |
| cancel | 承認待ちからDiscord control loopのcancel経由で実Proxyの中断を確認。会話pause=false、hold解放 |
| revoke | 依頼中許可をGatewayから取消。以降4呼出しは個別確認、grantはoperator_revoked。全5呼出しの集計も一致 |

実行方法: `HOSHIKAGE_LIVE_CONFIG=<設定> HOSHIKAGE_MCP_TOOL=tabs HOSHIKAGE_MCP_CASE=<once-turn|cancel|revoke> cargo test --test live_mcp_gateway -- --ignored --nocapture`。Proxy側に作る試験会話はケースごとに独立し、異なる実行キーを使う。認証情報やタブ一覧結果は試験ログへ出さない。

試験ハーネスを0.6へ更新する途中、DiscordのMarkdown escapeと、0.6のRun終了時のrevoked/scope_endedを旧試験が誤判定した。実HTTP・コードと照合し、判定を修正して別試験Runで再確認した。失敗した試験Runを再送していない。製品コードの追加修正は不要だった。試験更新後もClippy全target、format、差分検査は成功。

実Discordの承認カード外観・本人によるボタン押下は利用者へ確認依頼中。模擬Discordの成功を実画面の受入済みとしない。停止/Steer競合・通信断/復元の網羅試験は既存モック試験の範囲であり、今回常駐Proxyに障害を注入したとは扱わない。コミット・pushは未実施。

## 20:25 実Discord受入で発見した未解決不具合

最初の公開カードと3択は利用者が確認済み。しかし人が内容を読んでから押すと失敗した。現在照会では同じinteractionがprivate_required/privacy_unclassifiedへ変化し、diagnostic.code=null、presentation_id/pageもnullとなった。Gatewayは契約検証で拒否し、この試験interactionの返信送信意思は未作成。Proxyのcatalog.peekは30秒後にLoadingを返す。HTTP入口に再取得はあるが有界待機結果を捨て、表示評価はpeekを参照するため、更新未完了を未評価・本人向けへ分類する経路がある。短時間で押す先の成功試験ではこの経路を覆えていなかった。

受入へ「同一定義のまま表示後35秒以上読んでから承認」「定期更新を跨ぐ」「private_required全応答の契約整合」を追加する。`HOSHIKAGE_MCP_REVIEW_DELAY_SECS=35`で実接続試験の押下を遅らせられる。現時点では実Discordの承認受入は未完了。詳細は[修正依頼](proxy-mcp-v06-review-delay-fix.ja.md)。

35秒待機の追加実接続試験は単発・依頼中の双方で成功した。これは実Discordの不具合解消を意味しない。HTTP入口にはカタログ再取得処理があり、待機結果と押下タイミングに依存するため、恒常的な30秒ハードタイムアウトという説明は不正確。更新未完了／失敗の応答契約不整合と、実Discordで受信した不正応答の再現条件をProxy側で検証する必要がある。

## カタログ更新待ち修正の再受入（2026-09-17）

Proxy修正5b17f9cの常駐反映後、実Proxy・実Codex・実Playwrightと隔離Gateway DB・模擬Discordで再試験した。`HOSHIKAGE_MCP_REVIEW_DELAY_SECS=90`を指定し、最初のRunは90秒後の依頼中許可で5呼出し、次のRunは90秒後の単発許可と残り4件の単発許可で完了した。次Runへの許可持越しはない。タブ操作は一覧取得のみ。これは利用者による実Discord押下の代替ではない。

第14節へ接続するGatewayの補正も実施した。更新待ちのnull表示IDを「別表示への置換」と混同せず、許可前に同じ表示IDで最大3回・2秒間隔の再照会を行う。復帰した表示・scope・ページ証拠は従来どおり照合する。許可POSTは自動再送しない。Proxyが許可未送信を保証する409 catalog_loading/catalog_failedはREJECTEDとして記録し、拒否と新しい明示選択を妨げない。未送信と次の操作を利用者へ案内する。

承認UI50件・契約型15件の試験が成功。更新待ちからの復帰、有限待機、409後の拒否、実Proxyが生成した4応答例の型照合を含む。初回の追加試験でnull表示IDを拒絶するGatewayの不具合を検出し、修正後に通過した。承認を考える時間に30秒の制限を設けていない。表示証拠や元の操作自体の失効は別途検証する。

全体回帰138件成功、実環境用6件は既定では除外。Clippy全target・format確認も成功。今回の90秒待機試験は実環境用試験を別途明示実行した結果である。

21:15:52 JSTに修正版をsystemd user serviceへ反映。配備SHA-256は `b7a3a6a0cda7a54684041d913b394ad14ae36d4885edee815b24df914f6bb0b5`。再起動後active/running、Discord connected=true、Proxy ready=true、recovery_pending=false、holdなしを確認。設定・Proxyサービスは変更していない。実Discordで1分以上考えてから押す最終確認は利用者へ依頼する。
