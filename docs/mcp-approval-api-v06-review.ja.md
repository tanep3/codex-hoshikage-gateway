# MCP承認API 0.6 Gateway接続レビュー

2026-09-17。対象：[API](../../codex-hoshikage-proxy/docs/mcp-approval-api-v06.ja.md)、[JSON例](../../codex-hoshikage-proxy/docs/mcp-approval/api-v06-examples.json)。**第12〜13節の再照合でC06-01・02は解消。Gateway側はAPI 0.6の接続仕様を受け入れ、接続Goとする。** 全342ツールの意味評価完了を要求するレビューではない。実装・実機受入・常駐反映は未完了。以下の指摘本文は履歴として保持する。

## 第12〜13節の再レビュー結果

- C06-01解消：エンドポイント別の非圧縮JSON上限、response_limits、発行前の将来状態込み容量予約、超過エラー、保存済みgrant IDからの独立した取消が定義された。旧版の上限と0.6 operationの上限も区別されている。
- C06-02解消：永続受付から60000msの期限、preparationの証拠、policy／Response／operation／占有の対応、Turn ID不要のstop、停止とTurn送信意思の直列化、設定のみ不明な場合と実行開始不明な場合の復旧が定義された。
- JSON例の新response_limits・60000ms、execution_policyのpreparation必須項目、返信のbinding／scope fingerprint／page tokenとの一致をGateway側でも確認した。追加のC06-S01〜04／P01〜05は受入条件として確認し、製品試験成功とは扱わない。
- 追加の接続差し戻しはない。Gatewayはこの合意版から型・DB・ページ取得・停止表示の内部設計を具体化する。Proxyの設定隔離・会話継続の実証は引き続き実装と受入で確認する。全ツール解析完了待ちへ戻さない。

## 確認できた整合

- 表示profileとpolicy選択が独立し、省略/nullは未選択。未選択にもbindingを発行し、policy世代はnullのまま保持する。
- 意味未評価とcall／引数取得不足を区別する。未評価でも完全表示・禁止検査の条件を満たせば単発可、依頼中許可は不可。
- guardの禁止は選択実行に限定する。旧クライアント・標準APIへ暗黙適用せず、同一会話で履歴を維持して設定を切り替える。
- 拒否は表示・policy検査と独立し、単発と依頼中の返信には期待bindingを要求する。
- arguments=nullを取得失敗と即断せず、presentation_pagesによる完全表示を使える。
- JSON構文、各例の65536 bytes以内、scopeとexecution_policyのbinding／世代、単発・依頼中返信と表示例のfingerprint／page tokenの一致をGateway側でも検査した。実行試験ではない。

## C06-01 応答上限をエンドポイント別に確定する

API末尾は「各応答の総量は65536 bytesを優先」としている。一方、第8節は0.5の一覧を継承しており、0.5にはgrant一覧16件・1MiB・ページングなしという契約がある。interaction一覧等の既存外枠も維持するとされているため、Gatewayがどの読取上限を適用すべきか曖昧である。

各policyには最大64件×1024 bytesのupstream_overridesを持てるため、単一policyの説明だけでも64KiBへ達する。各要素の制限内でも、繰り返し埋め込むexecution_policyやscopeを含む全体は収まらない場合がある。小さい成功例のサイズ確認だけではこの境界を確認できない。

追記依頼：capabilities、Response受付／単体照会、presentation、操作詳細、単一interaction／一覧、grant一覧について、上限と旧版継承範囲を表で固定する。登録policyの総量検証や発行上限で必要なメタデータを常に取得可能にし、超過時のHTTP/codeを定義する。既に発行したgrantが一覧取得不能になり取消導線を失わないことを受入へ追加する。配列の黙示切捨てや巨大なクライアント上限で回避しない。

## C06-02 preparingの期限・停止・復旧を照会可能にする

第3節の202は永続受付で、設定準備が完了してからTurnを開始するという境界は適切。ただし準備期限、準備中の再起動、設定結果不明とResponse状態・占有の対応が具体化されていない。Turn IDがないpreparingでGatewayの/stopが使う操作も特定が必要。Gateway独自timeoutで失敗確定・占有解除すると、遅着の準備完了による実行と競合する。

追記依頼：

1. 準備期限の開始点・有限上限・GETや再起動で延長しない条件を定める。固定値か公開される期限のどちらかを契約で確定する。
2. preparing/ready/failed/closedと既存Response状態、operation、会話占有の対応を示す。policy_setup_failedとpolicy_setup_unknownで、Turn未開始をどの証拠から確定し、次Runを許すかを定義する。
3. Turn未発行でも停止できる既存APIを明示するか、必要な接続差分を定義する。停止受付とready確定・Turn開始を直列化し、停止が先なら遅着準備で開始しない。
4. 準備中のProxy再起動・設定反映応答喪失は同じResponse/bindingで照合すること、設定不明をpolicy未選択に置き換えないことを具体例へ追加する。

これはruntime分離の実証完了を先に要求するものではない。Gatewayが利用者へ待機・失敗・結果不明を正しく案内し、安全に停止・復旧するための外部状態契約の確認である。

## Gateway側で反映する差分

接続確定後に、要求と実効policy、nullable scope、argument_integrity、semantic_assessment、arguments_deliveryを版別型へ落とす。policy未選択でもbinding必須。旧0.5のcatalog不足による単発一律禁止・必須tool_policyを流用しない。現在のDB schema9案をこの型に合わせて更新する。

0.6拒否例はresponse.actionのみでcontentを省略する形なので、Gatewayの後継serializerもその完全例へ合わせる。旧版のcontent:nullを勝手に新契約の必須条件にしない。

設定隔離と既存会話継続はProxy内部の実証課題、Discordの全投稿配信確定・本人認可・復旧はGatewayの実証課題として分離する。今回、コード・設定・常駐環境は変更していない。

## ProxyによるGateway設計レビューの受領

2026-09-17：[Proxyレビュー](../../codex-hoshikage-proxy/docs/mcp-approval-layered-gateway-review.ja.md)を確認。責務分担・開発工程は双方Go、方針の差し戻しなし。次の4点を具体契約合意後の反映項目として受け入れる。

- DisplaySnapshotの正本はpresentationの全ページ。operation.argumentsから公開用説明を再生成しない。
- selectionと実効binding/generation/stateを別保存。202を準備完了と扱わず、拒否へpolicy照合を要求しない。
- 初期0.6には未選択実行への強制policy選択はない。将来用の論理型から現行機能を捏造しない。
- 初期guard選択と表示profileを別保存し、未適用schema9案を整理して既存schema8の履歴を保持する。

これはC06-01・02への回答ではなく、具体APIの接続合意とは別のレビューである。2点の未解決状態は維持する。コード・DB・常駐環境は変更していない。
