# MCP承認UI 全体監査（2026-09-17）

状態：調査結果・修正要求。修正完了や全ツールの実行試験完了を意味しない。

## 利用者の要求と今回の判定

通常の操作は最初から何を実行するかを元会話へ示し、「この依頼中、このツールを許可／今回だけ許可／拒否」をその場で選べるようにする。タブ一覧だけを例外的に直して完了とはしない。秘密入力・任意コード・認証フォームを無条件に公開／自動回答する変更とは区別する。

**現状は要求未達。** 0.4で実装した3 rendererと単一ツールの試験を、全体の完成と取り違えていた。設定や未対応扱いを利用者へ説明するだけでは解決しない。

## 調査方法・限界

- 常駐Gateway DBの実interactionをProxy operation/presentationで照会。browser_tabsのbinding_status=verified、ineligible_reason=tool_not_allowlisted、presentation=private_required/unsupported_rendererを確認。
- 同じ生成configを一時CODEX_HOMEへ複製し、実Codex App Serverのthread/startとmcpServerStatus/listで全カタログを取得。AI Turn・ツール実行・本番設定変更は行わない。一時認証コピーは終了時削除。
- 4サーバー・342ツールの定義を取得。一覧所要時間9.09秒。JSONをUTF-8・空白なしで再構成したサイズ1,076,442 bytes。実行権限は変更していない。
- 全件の**定義・判定コード・UI経路**を確認したもので、342ツールすべてを実行したという意味ではない。書込み・削除・外部送信を伴うツールを網羅性のためだけに実行しない。
- Appsや常時許可ツールを含む。以下の表はMCP操作確認として到来した場合の現行判定であり、全ツールが毎回確認を発生させるという意味ではない。

## 重大な指摘

### P-01 全カタログのサイズ制限で正常な公開表示まで失われる

Proxy `src/v2/presentations.rs` のrefresh_catalogは、各ページを1,048,576 bytesで制限し、超過時に結果全体をnullへ置換してregister_catalogへ渡す。register_catalogは受理済みツールを空へ更新する。サーバー／ツールの絞込みはその後なので、Apps側の大きなカタログによりPlaywrightの正常な3定義も失われる。

今回の同構成カタログは1,076,442 bytesで上限を27,866 bytes超過。Playwright単独は28,877 bytes。browser_tabsの実schemaは現行schema_matchesの条件と一致し、単純な「このツールの形式が未対応」では説明できない。一覧上限超過から全件破棄への経路はコードで確定。常駐プロセス内部の当該RPC本文そのものは取得していないため、全発生回の直接原因を同一と断定しない。

**必要修正：** 実環境全カタログを扱える有限の上限・ページング・対象抽出を契約化し、無関係なサーバーのサイズが対応ツールを未対応にしないようにする。無条件無制限化や未検証の古い定義の使い回しはしない。

### P-02 一時取得失敗を「未対応」と表示する

refresh_catalogの10秒timeout、RPC失敗、ページング異常もnullへ統合し、最終的にunsupported_rendererとなる。実presentation照会でも約10秒の応答を観測し、全カタログ取得は隔離時でも9.09秒だった。timeoutが実発生したことはログ不足のため未確定だが、失敗理由を失う設計は確定。

**必要修正：** catalog取得失敗・サイズ超過・定義不一致・未対応rendererを区別する。監査可能な機械可読理由を返し、Gatewayは「取得できないので待つ／再確認する」と「未対応」を区別して案内する。表示GETごとの重い全件取得、同時照会、キャンセル時のRPC後始末も見直す。取得失敗時に安全検証を迂回して許可しない。

### P-03 公開表示は3ツールの一部操作しかない

Playwright43ツールのうちfind、navigate、tabs/listだけ。残り40とtabsのnew/close/select、Serena23、node_repl4、Apps272には公開rendererがない。秘密がない通常操作まで「本人限定で確認」が通常導線になる。Gatewayも既知rendererを3種に限定しているため、Proxyだけ増やしても接続できない。

**必要修正：** 全ツールの操作説明・対象・変更の有無・公開可否・許可範囲を下表で管理し、通常操作の表示を両者で拡張する。ツール名／モデルの要約だけを実操作の証拠として承認しない。未実装と秘密情報を同じ理由にしない。未知ツールを無審査で公開する汎用JSONダンプで解決しない。

### P-04 3択の運用が1ツールに限定されている

本番Proxyの対象はplaywright.browser_findのみ。browser_tabsが2択なのはtool_not_allowlistedによる。これは表示障害とは別。対象外だから2択でよいと説明するだけでは、利用者が求めた操作性は満たさない。

**必要修正：** 下表の全ツールについてターン許可の適格性を評価し、対象方針・導入既定値・運用設定を一致させる。browser_tabsはlistだけでなくnew/close/selectも含むツールなので、現行turn_toolは引数変更も含むことをカードで明示する。list限定許可にしたい場合はscope契約の追加が必要。高危険度コード実行や内部認証フォームを一括許可へ含めない。

### G-01 コマンド・ファイル承認は別表示経路

Gatewayのapproval_uiはMCP presentationを使わず、コマンドを950 UTF-16 units、ファイルを最大8件／各100 unitsへ省略してもapproval_can_acceptは許可可能とする。長文と省略表示を同時検証する受入が不足し、MCPの「判断材料を切り詰めたまま直接承認させない」と不一致。無条件にMCPのturn_tool許可を流用できる経路でもない。

**必要修正：** 共通の表示原則へ整理し、全操作内容を確認できないカードを直接承認可能にしない。コマンド文字の置換表示と原文、ファイル変更の対象と変更内容を区別する。要件・設計から改訂する。

### T-01 これまでの接続試験の不足

実試験はPlaywrightのnavigate/findだけを露出し、カタログを小さくしていた。全MCP・Appsを含む本番構成を検証していなかった。また取消試験の1回のprivate経路を例外として記録しただけで、理由が未調査だった。以後、通常操作が補足へ落ちた場合は理由を必須記録し、想定inlineの試験はfallbackでも合格にしない。

## Gateway承認経路の横断確認

| 経路 | 現在の表示・選択 | 確認結果／不足 |
| --- | --- | --- |
| 新MCP公開表示 | displayと2/3択、送信確定digest照合 | 既知3 rendererだけ。全体カタログの不具合の影響を受ける |
| MCP補足 | 本人限定の生引数JSON、単発／適格時ターン／拒否 | 通常経路化が問題。生JSONだけで意味が伝わりにくい |
| 旧Response／旧Proxy | 詳細ボタンから確認 | 互換維持として必要。新依頼でもここへ落ちないことを別途試験 |
| MCPフォーム・追加認証 | 本人限定入力、送信／拒否 | 操作許可と別物。一括自動回答しない |
| コマンド実行 | 目的・コマンド、単発／拒否／取消 | 長文の省略と直接承認の不整合あり |
| ファイル変更 | パス一覧、単発／拒否／取消 | 件数・パス長の省略と直接承認の不整合あり |
| 旧版・別人・別会話・期限切れ | 拒否／結果照会 | 既存回帰試験あり。342ツール個別の実行試験とは別 |
| Steer・停止・取消・終了 | Proxyで失効、Gatewayで旧表示拒否 | 一括許可の解除と実行停止を混同しない |

## 全ツール確認表

「公開表示実装なし」は引数に秘密があるとの判定ではない。表示アダプタそのものが存在しないことを示す。対象設定ありでも呼出し同定・秘密除外・世代・期限の判定が必要。

| サーバー | ツール | 現行公開表示 | 現行ターン許可 |
| --- | --- | --- | --- |
| codex_apps | codex_document_control.execute_document_command | 公開表示実装なし | 個別確認（Proxy危険名判定） |
| codex_apps | codex_document_control.get_document_tool_schemas | 公開表示実装なし | 対象設定なし |
| codex_apps | codex_document_control.list_document_sessions | 公開表示実装なし | 対象設定なし |
| codex_apps | github.add_comment_to_issue | 公開表示実装なし | 対象設定なし |
| codex_apps | github.add_issue_assignees | 公開表示実装なし | 対象設定なし |
| codex_apps | github.add_issue_labels | 公開表示実装なし | 対象設定なし |
| codex_apps | github.add_reaction_to_issue_comment | 公開表示実装なし | 対象設定なし |
| codex_apps | github.add_reaction_to_pr | 公開表示実装なし | 対象設定なし |
| codex_apps | github.add_reaction_to_pr_review_comment | 公開表示実装なし | 対象設定なし |
| codex_apps | github.add_review_to_pr | 公開表示実装なし | 対象設定なし |
| codex_apps | github.compare_commits | 公開表示実装なし | 対象設定なし |
| codex_apps | github.convert_pull_request_to_draft | 公開表示実装なし | 対象設定なし |
| codex_apps | github.create_blob | 公開表示実装なし | 対象設定なし |
| codex_apps | github.create_branch | 公開表示実装なし | 対象設定なし |
| codex_apps | github.create_commit | 公開表示実装なし | 対象設定なし |
| codex_apps | github.create_file | 公開表示実装なし | 対象設定なし |
| codex_apps | github.create_issue | 公開表示実装なし | 対象設定なし |
| codex_apps | github.create_pull_request | 公開表示実装なし | 対象設定なし |
| codex_apps | github.create_tree | 公開表示実装なし | 対象設定なし |
| codex_apps | github.delete_file | 公開表示実装なし | 対象設定なし |
| codex_apps | github.dismiss_pull_request_review | 公開表示実装なし | 対象設定なし |
| codex_apps | github.download_user_content | 公開表示実装なし | 対象設定なし |
| codex_apps | github.download_workflow_artifact | 公開表示実装なし | 対象設定なし |
| codex_apps | github.enable_auto_merge | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch_blob | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch_commit | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch_commit_workflow_runs | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch_file | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch_issue | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch_issue_comments | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch_pr | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch_pr_comments | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch_pr_file_patch | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch_pr_patch | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch_workflow_job_logs | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch_workflow_job_steps | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch_workflow_run_artifacts | 公開表示実装なし | 対象設定なし |
| codex_apps | github.fetch_workflow_run_jobs | 公開表示実装なし | 対象設定なし |
| codex_apps | github.get_commit_combined_status | 公開表示実装なし | 対象設定なし |
| codex_apps | github.get_issue_comment_reactions | 公開表示実装なし | 対象設定なし |
| codex_apps | github.get_pr_diff | 公開表示実装なし | 対象設定なし |
| codex_apps | github.get_pr_info | 公開表示実装なし | 対象設定なし |
| codex_apps | github.get_pr_reactions | 公開表示実装なし | 対象設定なし |
| codex_apps | github.get_pr_review_comment_reactions | 公開表示実装なし | 対象設定なし |
| codex_apps | github.get_pr_statuses | 公開表示実装なし | 対象設定なし |
| codex_apps | github.get_profile | 公開表示実装なし | 対象設定なし |
| codex_apps | github.get_repo | 公開表示実装なし | 対象設定なし |
| codex_apps | github.get_repo_collaborator_permission | 公開表示実装なし | 対象設定なし |
| codex_apps | github.get_user_login | 公開表示実装なし | 対象設定なし |
| codex_apps | github.get_users_recent_prs_in_repo | 公開表示実装なし | 対象設定なし |
| codex_apps | github.label_pr | 公開表示実装なし | 対象設定なし |
| codex_apps | github.list_installations | 公開表示実装なし | 対象設定なし |
| codex_apps | github.list_installed_accounts | 公開表示実装なし | 対象設定なし |
| codex_apps | github.list_pr_changed_filenames | 公開表示実装なし | 対象設定なし |
| codex_apps | github.list_pull_request_review_threads | 公開表示実装なし | 対象設定なし |
| codex_apps | github.list_pull_request_reviews | 公開表示実装なし | 対象設定なし |
| codex_apps | github.list_recent_issues | 公開表示実装なし | 対象設定なし |
| codex_apps | github.list_repositories | 公開表示実装なし | 対象設定なし |
| codex_apps | github.list_repositories_by_affiliation | 公開表示実装なし | 対象設定なし |
| codex_apps | github.list_repositories_by_installation | 公開表示実装なし | 対象設定なし |
| codex_apps | github.list_user_org_memberships | 公開表示実装なし | 対象設定なし |
| codex_apps | github.list_user_orgs | 公開表示実装なし | 対象設定なし |
| codex_apps | github.lock_issue_conversation | 公開表示実装なし | 対象設定なし |
| codex_apps | github.mark_pull_request_ready_for_review | 公開表示実装なし | 対象設定なし |
| codex_apps | github.merge_pull_request | 公開表示実装なし | 対象設定なし |
| codex_apps | github.remove_issue_assignees | 公開表示実装なし | 対象設定なし |
| codex_apps | github.remove_issue_label | 公開表示実装なし | 対象設定なし |
| codex_apps | github.remove_pull_request_reviewers | 公開表示実装なし | 対象設定なし |
| codex_apps | github.remove_reaction_from_issue_comment | 公開表示実装なし | 対象設定なし |
| codex_apps | github.remove_reaction_from_pr | 公開表示実装なし | 対象設定なし |
| codex_apps | github.remove_reaction_from_pr_review_comment | 公開表示実装なし | 対象設定なし |
| codex_apps | github.reply_to_review_comment | 公開表示実装なし | 対象設定なし |
| codex_apps | github.request_pull_request_reviewers | 公開表示実装なし | 対象設定なし |
| codex_apps | github.rerun_failed_workflow_run_jobs | 公開表示実装なし | 対象設定なし |
| codex_apps | github.rerun_workflow_job | 公開表示実装なし | 対象設定なし |
| codex_apps | github.resolve_review_thread | 公開表示実装なし | 対象設定なし |
| codex_apps | github.search | 公開表示実装なし | 対象設定なし |
| codex_apps | github.search_branches | 公開表示実装なし | 対象設定なし |
| codex_apps | github.search_commits | 公開表示実装なし | 対象設定なし |
| codex_apps | github.search_installed_repositories_streaming | 公開表示実装なし | 対象設定なし |
| codex_apps | github.search_installed_repositories_v2 | 公開表示実装なし | 対象設定なし |
| codex_apps | github.search_issues | 公開表示実装なし | 対象設定なし |
| codex_apps | github.search_prs | 公開表示実装なし | 対象設定なし |
| codex_apps | github.search_repositories | 公開表示実装なし | 対象設定なし |
| codex_apps | github.unlock_issue_conversation | 公開表示実装なし | 対象設定なし |
| codex_apps | github.unresolve_review_thread | 公開表示実装なし | 対象設定なし |
| codex_apps | github.update_file | 公開表示実装なし | 対象設定なし |
| codex_apps | github.update_issue | 公開表示実装なし | 対象設定なし |
| codex_apps | github.update_issue_comment | 公開表示実装なし | 対象設定なし |
| codex_apps | github.update_pull_request | 公開表示実装なし | 対象設定なし |
| codex_apps | github.update_ref | 公開表示実装なし | 対象設定なし |
| codex_apps | github.update_review_comment | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.apply_labels_to_emails | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.archive_emails | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.batch_modify_email | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.batch_read_email | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.batch_read_email_threads | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.bulk_label_matching_emails | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.create_draft | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.create_label | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.delete_emails | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.forward_emails | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.get_profile | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.list_drafts | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.list_labels | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.read_attachment | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.read_email | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.read_email_thread | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.search_email_ids | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.search_emails | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.send_draft | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.send_email | 公開表示実装なし | 対象設定なし |
| codex_apps | gmail.update_draft | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.batch_read_event | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.create_event | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.delete_event | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.fetch | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.get_availability | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.get_colors | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.get_profile | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.list_calendars | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.list_event_labels | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.read_event | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.respond_event | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.search | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.search_events | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.set_event_label_silently | 公開表示実装なし | 対象設定なし |
| codex_apps | google_calendar.update_event | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.batch_update_document | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.batch_update_presentation | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.batch_update_spreadsheet | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.bulk_update_file_comments | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.copy_file | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.create_file | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.create_folder | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.create_presentation_from_template | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.delete_file | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.duplicate_sheet_in_new_spreadsheet | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.export_file | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.fetch | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.fetch_file_revision | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.find_document_text_range | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_document | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_document_comments | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_document_paragraph_range | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_document_tables | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_document_text | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_file_comments | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_file_metadata | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_presentation | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_presentation_comments | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_presentation_outline | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_presentation_tables | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_presentation_text | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_profile | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_slide | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_slide_thumbnail | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_spreadsheet_cells | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_spreadsheet_comments | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_spreadsheet_metadata | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.get_spreadsheet_range | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.import_document | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.import_presentation | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.import_spreadsheet | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.list_drives | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.list_file_revisions | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.list_folder | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.recent_documents | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.search | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.search_spreadsheet_rows | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.share_file | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.update_file | 公開表示実装なし | 対象設定なし |
| codex_apps | google_drive.upload_file | 公開表示実装なし | 対象設定なし |
| codex_apps | hotline.get_local_hotline | 公開表示実装なし | 対象設定なし |
| codex_apps | hugging_face.dataset_search | 公開表示実装なし | 対象設定なし |
| codex_apps | hugging_face.hf_doc_fetch | 公開表示実装なし | 対象設定なし |
| codex_apps | hugging_face.hf_doc_search | 公開表示実装なし | 対象設定なし |
| codex_apps | hugging_face.hf_jobs | 公開表示実装なし | 対象設定なし |
| codex_apps | hugging_face.hf_whoami | 公開表示実装なし | 対象設定なし |
| codex_apps | hugging_face.hub_repo_details | 公開表示実装なし | 対象設定なし |
| codex_apps | hugging_face.model_search | 公開表示実装なし | 対象設定なし |
| codex_apps | hugging_face.paper_search | 公開表示実装なし | 対象設定なし |
| codex_apps | hugging_face.space_search | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.fetch | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-check-mcp-next-steps | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-convert-page-to-skill | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-create-attachment | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-create-comment | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-create-database | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-create-file-upload | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-create-folder | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-create-pages | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-create-view | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-download-attachment | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-duplicate-page | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-get-async-task | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-get-comments | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-get-session-status | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-get-teams | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-get-users | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-list-favorite-pages | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-list-private-pages | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-list-recent-pages | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-list-session-events | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-list-shared-pages | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-move-pages | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-query-meeting-notes | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-query-sessions | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-read-session-event | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-search-agents | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-search-sessions | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-search-skills | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-send-message-to-session | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-show-advanced-analysis-next-steps | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-spawn-session | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-stop-session | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-update-data-source | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-update-folder | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-update-page | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-update-view | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.notion-wait-session | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.query-data-sources | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.query-multiple-data-sources | 公開表示実装なし | 対象設定なし |
| codex_apps | notion.search | 公開表示実装なし | 対象設定なし |
| codex_apps | plugin_management.get_app_permissions | 公開表示実装なし | 対象設定なし |
| codex_apps | plugin_management.get_plugin_dependencies | 公開表示実装なし | 対象設定なし |
| codex_apps | plugin_management.search_plugins | 公開表示実装なし | 対象設定なし |
| codex_apps | plugin_management.suggest_plugins | 公開表示実装なし | 対象設定なし |
| codex_apps | plugin_management.uninstall_app | 公開表示実装なし | 対象設定なし |
| codex_apps | plugin_management.update_app_permissions | 公開表示実装なし | 対象設定なし |
| codex_apps | safety_settings.get_family_info | 公開表示実装なし | 対象設定なし |
| codex_apps | safety_settings.get_parental_controls | 公開表示実装なし | 対象設定なし |
| codex_apps | safety_settings.get_trusted_contact | 公開表示実装なし | 対象設定なし |
| codex_apps | safety_settings.prepare_parental_control_update | 公開表示実装なし | 対象設定なし |
| codex_apps | safety_settings.update_parental_control | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.add_custom_domain | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.change_site_slug | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.check_slug_availability | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.create_site | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.create_source_repository_write_credential | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.delete_site | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.deploy_private_site_version | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.deploy_site_version | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.generate_siwc_bypass_token | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.get_database_overview | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.get_database_table_rows | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.get_deployment_status | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.get_environment | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.get_environment_variables | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.get_project | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.get_site | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.get_site_analytics_overview | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.get_site_version | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.get_site_worker_logs | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.list_custom_domains | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.list_projects | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.list_site_analytics_events | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.list_site_versions | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.list_sites | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.list_sites_creator_control_panel_projects | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.query_site_analytics_event | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.read_database_overview | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.read_database_table_rows | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.refresh_custom_domain_status | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.remove_custom_domain | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.save_site_version | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.update_access | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.update_environment | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.update_environment_variables | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.update_site_access | 公開表示実装なし | 対象設定なし |
| codex_apps | sites.update_site_metadata | 公開表示実装なし | 対象設定なし |
| node_repl | js | 公開表示実装なし | 対象設定なし |
| node_repl | js_add_node_module_dir | 公開表示実装なし | 対象設定なし |
| node_repl | js_reset | 公開表示実装なし | 対象設定なし |
| node_repl | turn_ended | 公開表示実装なし | 対象設定なし |
| playwright | browser_click | 公開表示実装なし | 対象設定なし |
| playwright | browser_close | 公開表示実装なし | 対象設定なし |
| playwright | browser_console_messages | 公開表示実装なし | 対象設定なし |
| playwright | browser_cookie_clear | 公開表示実装なし | 対象設定なし |
| playwright | browser_cookie_delete | 公開表示実装なし | 対象設定なし |
| playwright | browser_cookie_get | 公開表示実装なし | 対象設定なし |
| playwright | browser_cookie_list | 公開表示実装なし | 対象設定なし |
| playwright | browser_cookie_set | 公開表示実装なし | 対象設定なし |
| playwright | browser_drag | 公開表示実装なし | 対象設定なし |
| playwright | browser_drop | 公開表示実装なし | 対象設定なし |
| playwright | browser_evaluate | 公開表示実装なし | 個別確認（Proxy危険名判定） |
| playwright | browser_file_upload | 公開表示実装なし | 対象設定なし |
| playwright | browser_fill_form | 公開表示実装なし | 対象設定なし |
| playwright | browser_find | text / regexの一方のみ | 対象設定あり（呼出し時に追加判定） |
| playwright | browser_handle_dialog | 公開表示実装なし | 対象設定なし |
| playwright | browser_hover | 公開表示実装なし | 対象設定なし |
| playwright | browser_localstorage_clear | 公開表示実装なし | 対象設定なし |
| playwright | browser_localstorage_delete | 公開表示実装なし | 対象設定なし |
| playwright | browser_localstorage_get | 公開表示実装なし | 対象設定なし |
| playwright | browser_localstorage_list | 公開表示実装なし | 対象設定なし |
| playwright | browser_localstorage_set | 公開表示実装なし | 対象設定なし |
| playwright | browser_navigate | HTTP(S) URLのみ | 対象設定なし |
| playwright | browser_navigate_back | 公開表示実装なし | 対象設定なし |
| playwright | browser_network_request | 公開表示実装なし | 対象設定なし |
| playwright | browser_network_requests | 公開表示実装なし | 対象設定なし |
| playwright | browser_press_key | 公開表示実装なし | 対象設定なし |
| playwright | browser_resize | 公開表示実装なし | 対象設定なし |
| playwright | browser_run_code_unsafe | 公開表示実装なし | 個別確認（Proxy危険名判定） |
| playwright | browser_select_option | 公開表示実装なし | 対象設定なし |
| playwright | browser_sessionstorage_clear | 公開表示実装なし | 対象設定なし |
| playwright | browser_sessionstorage_delete | 公開表示実装なし | 対象設定なし |
| playwright | browser_sessionstorage_get | 公開表示実装なし | 対象設定なし |
| playwright | browser_sessionstorage_list | 公開表示実装なし | 対象設定なし |
| playwright | browser_sessionstorage_set | 公開表示実装なし | 対象設定なし |
| playwright | browser_set_storage_state | 公開表示実装なし | 対象設定なし |
| playwright | browser_snapshot | 公開表示実装なし | 対象設定なし |
| playwright | browser_storage_state | 公開表示実装なし | 対象設定なし |
| playwright | browser_tabs | action=listのみ | 対象設定なし |
| playwright | browser_take_screenshot | 公開表示実装なし | 対象設定なし |
| playwright | browser_type | 公開表示実装なし | 対象設定なし |
| playwright | browser_wait_for | 公開表示実装なし | 対象設定なし |
| playwright | browser_webmcp_call | 公開表示実装なし | 対象設定なし |
| playwright | browser_webmcp_list | 公開表示実装なし | 対象設定なし |
| serena | activate_project | 公開表示実装なし | 対象設定なし |
| serena | delete_memory | 公開表示実装なし | 対象設定なし |
| serena | edit_memory | 公開表示実装なし | 対象設定なし |
| serena | find_declaration | 公開表示実装なし | 対象設定なし |
| serena | find_implementations | 公開表示実装なし | 対象設定なし |
| serena | find_referencing_symbols | 公開表示実装なし | 対象設定なし |
| serena | find_symbol | 公開表示実装なし | 対象設定なし |
| serena | get_current_config | 公開表示実装なし | 対象設定なし |
| serena | get_diagnostics_for_file | 公開表示実装なし | 対象設定なし |
| serena | get_symbols_overview | 公開表示実装なし | 対象設定なし |
| serena | initial_instructions | 公開表示実装なし | 対象設定なし |
| serena | insert_after_symbol | 公開表示実装なし | 対象設定なし |
| serena | insert_before_symbol | 公開表示実装なし | 対象設定なし |
| serena | list_memories | 公開表示実装なし | 対象設定なし |
| serena | onboarding | 公開表示実装なし | 対象設定なし |
| serena | read_memory | 公開表示実装なし | 対象設定なし |
| serena | rename_memory | 公開表示実装なし | 対象設定なし |
| serena | rename_symbol | 公開表示実装なし | 対象設定なし |
| serena | replace_in_files | 公開表示実装なし | 対象設定なし |
| serena | replace_symbol_body | 公開表示実装なし | 対象設定なし |
| serena | safe_delete_symbol | 公開表示実装なし | 対象設定なし |
| serena | search_for_pattern | 公開表示実装なし | 対象設定なし |
| serena | write_memory | 公開表示実装なし | 対象設定なし |


## 検証記録

- 実カタログの3種類のschemaが対応条件と一致すること、全ページが1MiB上限を27,866 bytes超えることをローカル検証した。
- Gateway回帰：MCP33件、通常承認3件、配信7件、計43件成功。これらの成功は全カタログ経路・全ツールの通常表示の合格を意味しない。
- 調査で本番コード・Proxy設定・許可範囲は変更していない。承認要求への回答、ツール実行、外部投稿も行っていない。
- Gatewayの要件・設計書に要求未達を明記し、Proxy向けの修正・契約調整依頼を作成した。調整依頼を別タスクへ送信済みという意味ではない。
