# User manual

[日本語](user-manual.ja.md) · [Need to install it first?](installation.md)

Codex Hoshikage Gateway lets you give Codex tasks in an ordinary Discord conversation. Once it is installed, there is no special “start conversation” command.

## Talk to the Bot

Open a text channel or forum post in the authorized Discord server and send a message. If the Bot is set to respond only when mentioned, mention it with `@` first. Each text channel or forum post keeps its own conversation and working folder. You do not have to register a folder.

An ordinary message is a new task. It waits if a previous task is still running. To add a direction to **the task already running**, use `/steer`. Discord may show the Bot as “typing”; that means it is working, not that the task has finished.

Only the person selected during setup can operate this Bot. Other people who can view the channel can still see its messages. Use a private channel for private work.

## Commands

Type `/` in Discord and choose this Bot's command. In an example such as `/model id:...`, `id` is a field in Discord's command form.

| Command | What it does |
| --- | --- |
| `/status` | Show the current task, queue, model, reasoning effort, and conversation state |
| `/models` | List available model IDs; use `page` for more |
| `/model` | See the selected model and choose another from a menu |
| `/model id:MODEL_ID` | Enter a model ID directly; use it from the next task |
| `/effort` | See the selected reasoning effort and choose one supported by the current model |
| `/effort level:EFFORT` | Enter a reasoning effort directly; use it from the next task |
| `/workspace` | Show where this conversation's files are stored |
| `/get` | List artifacts registered in this conversation |
| `/get path:output/report.pdf` | Send a file from the working folder to Discord; enter a **relative path** |
| `/steer text:MORE_DETAIL` | Add instructions to the task currently running |
| `/cancel` | Cancel the newest queued task; if none is queued, request interruption of the running task |
| `/stop` | Request interruption and pause the remaining queue |
| `/resume` | Resume a queue paused by `/stop`; it does not rerun the stopped task |
| `/recover` | Review and recover a conversation blocked by an uncertain previous task |

The model and reasoning effort are saved for each conversation. Changing one conversation does not affect another. A conversation used for the first time starts with the defaults in the administrator's `config.direct.toml`.

Use `/cancel` when you want to withdraw a task you just sent; use `/stop` when you want to halt work and pause later tasks too. Sending an interrupt request and confirming that work stopped are separate. Check `/status` if needed. Changes already made to files are not automatically undone.

## Files and images

You can attach images and text files to a message. Files exceeding the configured size or count limits are rejected. Images made by Codex normally arrive automatically in the same conversation, sometimes after the text reply.

For other output, open `/get` to see registered artifacts. If you know the file location, enter a relative path such as `output/report.pdf` in `/get`'s `path` field. You do not need the absolute path shown by `/workspace`. `/get` sends a saved copy of the file as it was when fetched. A later change to the original file does not change that copy. If the result of sending to Discord is uncertain, the Gateway does not automatically resend it. Check the conversation for the attachment first.

## When Codex asks for permission

If Codex requests permission for a command or MCP tool, a card appears in the conversation. Select **自分だけに表示して確認** (“review privately”) and read the **operation and actual input** through to the last page. Approve only if you understand it. **今回だけ許可** (“allow once”) covers that call only. For some MCP tools, **この依頼中、このツールを許可** (“allow this tool during this task”) may appear; it can cover later calls to the same tool within this task, even when arguments or targets change. Read its scope before choosing it.

If the details are missing, unclear, or unwanted, select **拒否** (“decline”) or **取り消し** (“cancel”). Use `/stop` to stop the whole task. The tool's result appears in a later Codex reply; pressing Allow does not mean the tool succeeded.

## When a conversation gets stuck

If the Bot says it cannot confirm the previous task's result, do not keep posting the same request. Check `/status`. If it offers `/recover`, open `/recover` **in that conversation**. Read what will be kept and cancelled, then choose **新しい文脈で会話を再開** (“resume with a new context”) if you agree. **The old task is not rerun; Discord history and work files remain; unsent queued tasks are cancelled; Codex starts a fresh conversation context.** Send only the instructions you still need in a new message.

If `/recover` is unavailable, work may still be active. Check `/status`, use `/stop` if appropriate, and contact the Gateway operator if it remains stuck. `/resume` alone cannot clear an uncertain task. Restarting the service does not automatically rerun it either.

See [Installation](installation.md) for setup and [Operations](operations.md) for service checks.
