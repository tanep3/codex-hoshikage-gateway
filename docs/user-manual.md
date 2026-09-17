# User manual

[日本語](user-manual.ja.md) · [Installation](installation.md)

This manual is for people using an already configured Bot in Discord. If you are installing the Bot or setting up keys, start with the [installation guide](installation.md).

## Try your first conversation

1. Open the Discord server containing the Bot and enter a text channel, or open a forum post.
2. Send “Hello, what can you help me with?” If the Bot responds only to mentions, type `@`, select the Bot, and include your message.
3. Continue chatting after its reply. You do not need a new-conversation command or a server working-directory path.
4. If it does not respond, type `/`, select this Bot's `/status` command, and read its guidance. Share the result with your administrator if the issue persists.

Only the user allowed by the administrator can operate the Bot. Silence in response to other members is not necessarily an error. Conversation messages remain visible to people who can view that channel.

### Using commands

Notation such as `/model id:MODEL_ID` shows a command and its input field. Choose `/model` from Discord's suggestions, then enter the value in its `id` field. Do not enter the literal text `MODEL_ID`. For an easier model selection, use `/model` without an argument to open the menu.

## Start a conversation

Talk in an authorized text channel, thread, or forum post. No cwd or `/project` registration is needed. Each text channel has its own conversation; threads and forum posts have separate conversations.

The administrator chooses whether the Bot responds to all authorized posts or only mentions. The configured model is reused; a selection menu appears when a model must be chosen.

## Choose models and control work

| Command | Purpose |
| --- | --- |
| `/workspace` | Choose a shared workspace before starting a conversation (optional) |
| `/retry` | Select a saved reply/file and confirm redelivery |
| `/get scope:shared` | Explicitly list artifacts from the shared workspace |
| `/models` | List available models |
| `/model` | Show the selected model and choose a different model from the menu |
| `/model id:MODEL_ID` | Select the next request's model while preserving same-provider context |
| `/status` | Check conversation, model, pause, and Proxy connection state |
| `/steer text:INSTRUCTION` | Add an instruction to the current Turn; ordinary messages queue the next request |
| `/stop` | Pause the queue and stop the accepted request, including before Turn start |
| `/cancel` | Cancel the latest queued request, or interrupt the active request if nothing is queued. Does not pause or resume the queue. |
| `/resume` | Resume the queue, without rerunning a cancelled request |
| `/new title:NAME` | Start a new thread or forum post |
| `/mcp` | Inspect/revoke MCP permissions for this task. Use `/stop` to stop the whole task |

Stop acceptance and confirmed termination are different. The Bot reports uncertainty rather than claiming success. When approval is required, inspect the target and details before using the approval buttons.

## Send input and collect files

Attach supported images or UTF-8 text to your message. Oversized input is rejected before execution.

Use `/get` to select an artifact registered through the model's dedicated tool. For an unregistered file, use `/get path:output/report.pdf`. Paths are relative to the Proxy workspace; you do not need its absolute path.

An artifact ID identifies fixed bytes. Resending that version differs from capturing an updated source file. Lists show 25 items per page; use the next-page button to continue. Selections expire after ten minutes. Open `/get` again if a menu expires. Use `/retry` to select an undelivered or uncertain delivery, then confirm the possibility of duplicate messages. This never reruns the AI task.

## After a connection problem

Disconnecting the Gateway does not stop Proxy execution. Use `/stop` when you intend to stop. Saved final replies and artifacts can be retrieved by the same ID within the Proxy retention period, even after Gateway cache loss. Storage failure, expiration, or revoked access can prevent retrieval. Delivery recovery never automatically reruns the AI task.

Uncertain execution and changes to the Proxy recovery generation require operator reconciliation. See [Operations](operations.md) for recovery procedures.

## Share files across conversations

Each conversation normally gets its own workspace. If you want to share files, use `/workspace` before sending the first request and choose an authorized shared workspace. No config entry or host path is needed. Changes made by other conversations are visible there.

An existing conversation cannot change workspaces. Create another with `/new title:NAME` and choose there. Inside a forum post, this creates another post in the same forum. For forums that require tags, use Discord's standard post-creation screen.

See [Operations guide](operations.md).

Post normally in the place you want to use. `/new` is an optional shortcut to create a different Discord thread or forum post, not a prerequisite for conversation.

## Generated images

Ask for an image normally. With a compatible Proxy, the generated PNG appears automatically in the same conversation, even when the reply has no text. You do not need `/get`. Images may arrive after the text. Failed or interrupted work can still deliver saved images, marked as partial results when known before sending.

`/get` avoids attaching a saved version that is already queued or delivered. Use `/retry` and confirm if you want another copy. A size limit or expired resource may prevent delivery; `/get` does not bypass these limits. No extra configuration is required.

When image preparation or delivery takes longer, the Bot shows a progress message. That same message updates when delivery finishes, no images are found, or a problem occurs. You can wait without submitting the request again.

The Bot does not add routine working or completion messages. Temporary status warnings are removed automatically once normal operation is confirmed.

## Approvals and activity

Approval cards show the purpose and operation in readable form. “Approve once” permits only that operation. After approval, refusal, or cancellation, the same card shows the result and removes its buttons. If delivery is still being checked, wait in the same conversation instead of submitting again.

Discord’s native typing indicator shows that work is underway. Updates stop during approval waits, after completion, or when execution status is unknown; the indicator may remain visible for up to about 10 seconds. It indicates activity, not the model’s private reasoning.

Intermediate text is a temporary preview. The final answer arrives as a new message at the bottom of the conversation, then the preview is removed. You do not need to search above approval cards for an edited answer.

After you choose an approval button, the original card shows the result. A duplicate private confirmation is not posted.

### When an MCP tool asks for confirmation

MCP lets Codex use external tools such as browsers. Permission here means allowing a tool operation; it is separate from signing in.

The bot shows the operation and available choices. **今回だけ許可 (Allow this call only)** permits the displayed call once. Eligible tools also offer **この依頼中、このツールを許可 (Allow this tool during this request)**; see “Repeated MCP confirmations” below for its scope. A tool may ask for more information after you allow it.

For a form, open **入力フォームを開く**, select a field, and choose **入力する**. Enter the displayed number for an enumerated choice, or はい / いいえ for a boolean. Extra text boxes can hold the rest of a long string. Choose **この内容で送信** when ready. Defaults are never filled automatically. Form screens are private; answers are not posted to the public conversation. Re-enter unfinished answers after a bot restart.

Use `/stop` to stop the task. Expired or uncertain answers are never automatically approved or resent. Check the original card and `/status`. “Answer sent to MCP” confirms submission, not successful tool execution; the task result follows separately.

## If an answer or file does not arrive

- **Retrieval is being checked automatically:** wait without submitting the AI request again. If nothing arrives after a few minutes, share `/status` and the warning with the operator.
- **Discord delivery is uncertain:** check this conversation first. If the answer or file is present, no action is needed. Otherwise, use `/retry`, select the saved item, and acknowledge the possibility of duplicate messages. This resends the same saved version; it does not rerun the AI task.
- **Permission, capacity, corruption, or expiry errors:** address the stated cause first. Resending alone cannot fix it. Ask the operator to check settings or storage. For an expired file whose original still exists, `/get path:...` creates a new saved version. This cannot recover expired answer text.

Old retrieval warnings are removed after delivery completes. If the warning's own send or deletion result is uncertain, it can remain until reconciled.

### Repeated MCP confirmations

The same server or tool can ask again for each separate call. **今回だけ許可 (Allow this call only)** applies to one request. If the gateway cannot verify actual arguments or code separately from the original confirmation text, it tells you. If the operation is unclear, choose **拒否 (Decline)**, or use `/stop` to stop the whole task.

Cards with confirmed permission submission and resolution are consolidated into a count for the same task. The count covers accepted permissions and form submissions; it does not prove tool success. Declined, expired, and uncertain confirmations remain visible.

With compatible Gateway and Proxy versions and the feature enabled, supported operations such as page searches and navigation show **the operation and its search text or URL on the first card**. Read it and choose:

- **この依頼中、このツールを許可 (Allow this tool during this request):** allow repeated calls to this tool in the same request. This option appears only for eligible tools.
- **今回だけ許可 (Allow this call only):** allow the displayed call once. Another call may ask again.
- **拒否 (Decline):** decline this operation. Use `/stop` to stop the whole request.

Anyone who can read this conversation can also see the displayed search text or URL. Credentials, secret inputs, executable code, unsupported operations, and oversized details stay off the first card. It explains why and offers **本人限定で確認 (Review privately)** instead. Open this supplemental screen only when needed; use **次へ (Next)** for long details. If the operation cannot be retrieved, decline or use `/stop`, and ask the operator for help if the problem persists. Older Proxies and requests already in progress before the update may retain the **操作内容を確認 (View operation)** private-screen workflow.

Request-scoped permission includes **changed arguments to the same tool**; it is not limited to one website or read-only actions. It lasts up to ten minutes or until the request ends, and expires on stop or additional Steer instructions. It never carries into the next request. `browser_evaluate` and `browser_run_code_unsafe` always require individual confirmation.

Use `/mcp` to inspect and revoke permissions for the latest request. Revocation stops future permission applications; it cannot undo completed operations. If confirmation is uncertain, reopen the list to check the original revocation without sending a new request.

This feature requires an administrator to enable it and configure eligible tools on the Proxy; updating binaries alone does not enable it. Users do not need to edit configuration. If an option is missing, ask your administrator to check the [Gateway installation guide](installation.md) and the Proxy's [user and administrator guide (Japanese)](https://github.com/tanep3/codex-hoshikage-proxy/blob/main/docs/mcp-turn-approval-guide.ja.md). Unsupported or disabled environments retain individual confirmations.

`/cancel` links to the original request. Multiple queued requests are cancelled one at a time, newest first. An active request remains held until the Proxy confirms it has stopped; cancellation does not undo changes already made. If you previously paused the queue with `/stop`, use `/resume` when you want remaining requests to proceed.
