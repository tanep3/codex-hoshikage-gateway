# MCPインライン承認API 0.4：Gateway接続レビュー

2026-09-16。対象は `../../codex-hoshikage-proxy/docs/mcp-inline-approval-api.ja.md` の0.4接続レビュー案。R-01対応版を再レビューし、Gateway接続レビュー完了・実装開始可能と判断した。Gatewayの接続実装は完了し、検証結果は [受入記録](mcp-turn-approval-acceptance.ja.md) に分離して記載する。0.4の実Proxy/Discord受入・常駐反映は未完了。既存0.3の合意を取り消すものではない。

## 接続可能と判断した部分

- 既存operationのrequester_onlyを変更せず、presentationを別GETにする境界。
- Response受付ごとのapproval_presentation指定と、既存approval_contextへの拘束。次Responseへ継承しない。
- presentation_id/fingerprintをscope fingerprintとは独立に照合し、本文だけでなく省略・state/actions・audience・期限も固定すること。
- Proxyで返信意思保存と同じ直列化境界において再検証すること。同一キーの既登録操作は失効後も元結果を返すこと。
- inline/private_required/unavailableの区別、長文・秘匿・未知情報で直接許可を出さないこと。
- display以外のメタデータをカードへ出さず、公開内容はそのまま保持して装飾のみ無効化すること。
- 32768 bytesのHTTP上限、1400 UTF-16 units、fields最大8、発行4版、既存interaction以内かつ10分の期限。
- source_conversationは元会話の閲覧者への提示であり、一般公開・他会話転送の許可ではないこと。

## R-01：解消・0.4接続レビュー完了

2026-09-16、ProxyのR-01対応版を全文照合。初稿のbrowser_tabs/list限定という問題は解消した。公開方針は利用者の明示回答と一致し、同じ判断を再度求めない。

| 確認点 | 判定 |
| --- | --- |
| browser-find-v1 | textまたはregexの一方、原文全体を表示。現在ページが対象で、特定サイト固定ではない制限も明記。通常検索を一律補足へ戻さない |
| browser-navigate-v1 | HTTP(S) URL全体とページ移動を表示。リダイレクト先の保証なし。ターン許可対象を勝手に増やさない |
| browser-tabs-list-v1 | action=listのみ。結果のタブ名やURLを承認本文へ追加しない |
| 秘密情報・コード | 認証候補名、資格形式、userinfo、署名URL、多重encoding、未知引数を契約で具体化。対象値全体を非公開として補足へ。未知の秘密の完全検出は保証しない |
| actions | 公開可能性とターン適格性を独立に扱う。inlineは2択または3択、private_requiredは補足・拒否、unavailableは許可なし |
| 表示と承認 | display・省略・actions・audienceを不透明tokenへ拘束。Proxy自身も返信意思保存と同じ境界で再検証する |
| 配信・復旧 | message ID・確定digest・表示版を照合。旧版、未確定配信、別会話から許可しない。返信結果不明は同じoperationを照会 |
| 常時許可・互換 | 常時許可のツールに承認を新設しない。0.3の本人限定経路を維持。追加API未対応時に実引数を公開しない |
| 制限・受入 | HTTP 32768 bytes、display 1400 UTF-16 units、8 fields、4版と実Discord受入を明記 |

接続上の差戻し事項なし。公開対象は元の会話に限定し、認証情報・秘密入力・任意コードの公開や許可範囲拡大は含まない。click・入力・ファイル操作等の未評価rendererは初期対応対象外で補足確認となる。この明示された範囲を、任意ツールの全面インライン化と案内しない。

## Gatewayの対応設計

- Capabilityを既存details/turnと独立保持。追加宣言は対応を確認した新規Responseにだけ付与し、送信前の永続リクエストにも固定する。
- presentation GETは32768 bytesで制限し、ID・revision・operation scope fingerprint・audienceと保存contextを検証する。
- 公開カード専用の表示記録を追加。ローカルID・interaction・presentation ID/token・revision・scope/display digest・期限・配信状態を保持し、本文・私的引数は保存しない。
- `approval_view=source_conversation` と `expected_presentation_fingerprint` を必ず対で送る。単発とturn_toolを区別する。declineは既存のrevision付き返信。
- 公開カードの押下は保存済みDiscord message IDと確定済み配信digestを必要とする。Proxy現表示と照合し、未知の結果では返信せず照会する。
- 0.3互換と例外の本人限定経路を維持。新API失敗時に私的引数を公開するフォールバックは作らない。

認識するrendererはbrowser-find-v1、browser-navigate-v1、browser-tabs-list-v1の3種類に確定。合意0.4を基準に具体schema・パーサー・返信経路・受入を実装する。日英マニュアルは利用者指示どおり、実装・テスト後に更新する。
