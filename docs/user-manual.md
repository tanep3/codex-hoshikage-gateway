# User manual

[日本語](user-manual.ja.md) · [Product overview](../README.md) · [Installation guide](installation.md)

Ready to ask Codex for a hand? This guide walks you through a conversation, from your first message to picking up a finished file. Your Bot, account, and project channel should already be set up; if they are not, start with the [installation guide](installation.md).

Behind the scenes, [Codex Hoshikage Proxy](https://github.com/tanep3/codex-hoshikage-proxy) connects to Codex, while this Gateway handles your Discord conversations. You can do your day-to-day work right here in Discord.

The current version is for one authorized user. Bot command descriptions, buttons, and replies currently use Japanese; commands themselves use the names shown below. Live-service acceptance testing is still pending for development version 0.1.0.

## Start with a message

Open your project's text channel and select `/new` from Discord's command picker. Fill in its `title` option, for example:

```text
/new title:Review the test failure
```

Open the thread the Bot creates and post an ordinary message:

> Investigate the failing tests. Explain the cause and your proposed fix before changing files.

No Bot mention is needed. Your message becomes a work request. The Bot reports progress and displays reply text as it arrives. Reply in the same thread to continue with the same conversation context; create another thread with `/new` for a separate topic.

The project channel represents a folder on the server. Each Gateway-created thread represents a Codex conversation. Normal posts in the parent channel, DMs, and unrelated or manually created threads do not start work.

Threads created by the Bot are public within the parent channel's access boundary. Anyone who can view that conversation may see its content, even if they cannot operate the Bot.

## Another task, or a change to the current one?

While Codex is working, an ordinary post is **another queued request**. It does not immediately change the running task. Requests run in order within a conversation. The Gateway allows up to two executions overall, with one at a time for a workspace. Other conversations in the same project may therefore wait.

To give the active task an additional instruction, use:

```text
/steer text:Focus on the parser first. Do not change the public API.
```

Steer targets the current task only. It is unavailable when there is no suitable running task, while approval is pending, or when stopping/finished. It does not guarantee that the task will follow the instruction. **Use `/stop` when you need to request a stop.**

Avoid editing or deleting a queued original message or its attachments. The Gateway checks the original input again before sending it; changed or unavailable input is rejected. To correct a request, first check its state so you do not accidentally create a second task with overlapping effects.

## Stop and resume

```text
/stop
```

This immediately pauses the conversation's queue, then requests interruption if there is an active task. You can also use the stop button attached to a request.

- If only waiting requests exist, the queue is paused without an active task to interrupt.
- If a request is still being sent and its execution identity is not known, the pause takes effect, but stopping cannot yet be confirmed.
- A stop request being accepted does **not** mean execution has stopped. Check the following status updates or `/status`.
- Stopping does not undo file edits or other effects already made.

You may still post while paused; accepted requests wait without starting automatically. To allow those waiting requests to start:

```text
/resume
```

Resume does not restart an interrupted task. Write a new request if you want further work after checking the prior result. A pause survives Gateway restarts. Resume also does not clear an `UNKNOWN` execution hold; that requires operator investigation.

## Respond to approval requests

When the Proxy requests approval, the Bot shows buttons for that particular action:

| Button text | Meaning |
| --- | --- |
| 今回のみ承認 | Approve this action only |
| 拒否 | Deny the action |
| 取消 | Cancel the approval request |

Read the action before selecting a button. Only the configured user can respond. Buttons expire or become invalid when their task/approval no longer matches, so use the current prompt rather than an old one. The Gateway does not offer permanent or session-wide approval. Denial or cancellation may cause the task to fail or stop; inspect its eventual result.

Not every task produces a prompt: approval behavior depends on the Proxy's policy and what Codex needs to do.

## Choose a model

Inside a conversation, `/model` shows available models (up to 25 in the current list). Select a provider-qualified ID from the available models:

```text
/model id:PROVIDER/MODEL_ID
```

Replace the placeholder with an actual model ID. The selection applies to the **next task**, preserving the conversation context; it does not replace the model already running. Use `/status` to distinguish the selected model from the active task's model. A change to a different provider requires a new conversation.

## Attach input and retrieve output

Attach files to your ordinary request message using Discord's attachment control. Supported input is:

- Still PNG, JPEG, or WebP images.
- UTF-8 text files.

Animated images, PDF/document parsing, and archive extraction are not supported input features. Both the operator's configured limits and Discord's own upload limits apply. Ask the operator for allowed file counts/sizes; this version has no universal size promised by the manual. Files and prompts pass through Discord and the Proxy; include only what you intend those services to receive.

To receive an output file, explicitly name it relative to the project folder:

```text
/get path:output/report.pdf
```

This example retrieves a file already present on the **server**, not a file on your phone or computer. An output may be a PDF even though PDF input parsing is unsupported. The Bot does not automatically scan or send new/changed files, and a path mentioned in a Codex reply is not a download instruction.

Absolute paths, paths outside the project, symbolic links, hard links, special files, and files that fail the safe-read checks are rejected. A file may also exceed the configured return limit or Discord's limit. In that case, ask for a smaller suitable output and explicitly retrieve it.

## What is happening with my request?

Use `/status` in a conversation to inspect its pause state, model selection, current held request, and Proxy readiness. Common request states mean:

| State | What it means for you |
| --- | --- |
| `QUEUED` | Waiting for its turn, capacity, or resume |
| `SENDING` | The request is being handed to the Proxy; execution may not yet be confirmed |
| `RUNNING` | Execution is in progress |
| `APPROVAL_REQUIRED` | Your decision is needed |
| `CANCEL_REQUESTED` | Interruption was requested; stopping is not yet confirmed |
| `CANCELLED` | Cancellation was confirmed |
| `COMPLETED` | Codex execution completed; Discord reply delivery is a separate matter |
| `FAILED` | The task failed; inspect the reported result before trying new work |
| `UNKNOWN` | Execution status cannot be established reliably; automatic rerun is blocked |

There may also be an input-validation step before a request is queued. A rejected input was not accepted for execution. A full queue can prevent acceptance of another request.

## Coming back after a restart or connection problem

Return to the same Discord thread. Previously posted Discord messages remain there. The Gateway preserves conversation identity and queue pause state, and can recover eligible unsent requests by reading their original Discord messages again. Deleted, edited, or inaccessible input will not be executed automatically.

An uncertain request is not automatically submitted again. `UNKNOWN` can hold up later work in its workspace, and `/resume` does not override it. Ask the host operator to compare Gateway and Proxy status. If you are also the operator, begin with local `admin status` and the recovery instructions linked from the [installation guide](installation.md).

**Execution completion and receiving the complete answer are different.** The current Proxy contract cannot retrieve final answer text again. After a Gateway restart or reply-memory expiry, missing answer text may be unrecoverable even when completion is known. The Bot will report the limitation; it will not rerun Codex to reconstruct the answer. Existing project files can still be requested individually with `/get` when available.

An initial failed/cancelled task may leave no usable conversation context. If the Bot reports `NEW_CONVERSATION_REQUIRED`, use `/new` in the parent channel and describe the work you want to continue. If a thread is archived or locked, restore its usability through Discord if you have permission, or create a new conversation; do not assume the Bot will reopen it automatically.

## Command reference

The `name:value` notation below represents Discord command options. Choose the command in the picker and fill the displayed fields.

| Command | Where | Purpose |
| --- | --- | --- |
| `/new title:NAME` | Registered project channel | Create a conversation |
| Ordinary message, optionally with attachments | Registered conversation thread | Submit the next work request |
| `/status` | Conversation thread | Inspect status and models |
| `/stop` | Conversation thread | Pause the queue and request interruption |
| `/resume` | Conversation thread | Resume automatic start of waiting requests |
| `/steer text:INSTRUCTION` | Conversation thread | Instruct the active task |
| `/model` | Conversation thread | Show models |
| `/model id:PROVIDER/MODEL_ID` | Conversation thread | Choose the next task's model |
| `/get path:RELATIVE_PATH` | Conversation thread | Retrieve a specified output file |

If the Bot is offline, commands are missing, or all requests are rejected, see [installation troubleshooting](installation.md). If one request is uncertain, check it before posting the same work again.
