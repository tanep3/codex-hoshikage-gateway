# User manual

[日本語](user-manual.ja.md) · [Installation](installation.md)

This guide targets API v2. API v2 integration is implemented and acceptance testing is in progress; see [implementation status (Japanese)](implementation-status.ja.md). This is not a production rollout guide.

## Start a conversation

Talk in an authorized text channel, thread, or forum post. No cwd or `/project` registration is needed. Each text channel has its own conversation; threads and forum posts have separate conversations.

The administrator chooses whether the Bot responds to all authorized posts or only mentions. The configured model is reused; a selection menu appears when a model must be chosen.

## Choose models and control work

| Command | Purpose |
| --- | --- |
| `/new title:NAME` | Start a new thread or forum post |
| `/workspace` | Choose a shared workspace before starting a conversation (optional) |
| `/retry` | Select a saved reply/file and confirm redelivery |
| `/get scope:shared` | Explicitly list artifacts from the shared workspace |
| `/models` | List available models |
| `/model` | Show the selected model and choose a different model from the menu |
| `/model id:MODEL_ID` | Select the next request's model while preserving same-provider context |
| `/status` | Check conversation, model, pause, and Proxy connection state |
| `/steer text:INSTRUCTION` | Add an instruction to the current Turn; ordinary messages queue the next request |
| `/stop` | Pause the queue and stop the accepted request, including before Turn start |
| `/resume` | Resume the queue, without rerunning a cancelled request |

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
