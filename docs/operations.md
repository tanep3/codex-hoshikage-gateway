# Operations guide

[日本語](operations.ja.md) · [Installation](installation.md) · [User manual](user-manual.md)

## Recover a delivery

Use `/status` to inspect delivery states. Temporary transfer failures retry the same saved resource. An uncertain Discord send is reconciled without automatically creating another message.

`/retry` opens a selection and confirmation flow. Confirm the possibility of duplicates to resend the same saved ID and bytes. Use `/get path:...` to capture a newer source version instead. Expired, corrupt, or access-revoked content cannot be repaired by redelivery alone.

## Accept an officially restored Proxy

Ordinary restarts preserve the generation. A changed recovery generation blocks new execution. Run as the Gateway's OS user:

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin proxy-inspect
```

Review the old/new generations, checked count, and unavailable resources. The review expires after five minutes. A Proxy operator must clear the Proxy's own recovery hold first; the Gateway cannot do that.

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin proxy-accept --review-token REVIEW_TOKEN --reason "Checks performed and decision" --accept-risk
```

Acceptance rechecks identities before updating the binding. Queues remain paused and old pending requests remain ineligible for automatic dispatch. UNKNOWN is not changed into success or confirmed cancellation. Check conversation state; use `/new` where old queued requests are quarantined. This operation cannot attach a different Proxy instance.

## Resolve an indefinite UNKNOWN hold

A Proxy operator must investigate the execution, remaining processes, and workspace first. Follow the [Proxy operations guide](https://github.com/tanep3/codex-hoshikage-proxy/blob/main/docs/v2-operations.ja.md) for `admin execution-hold inspect/release`.

After Proxy release, obtain the request ID and hold generation from Gateway `admin status`:

```sh
codex-hoshikage-gateway --config "$HOME/.config/codex-hoshikage-gateway/config.toml" admin abandon --request-id REQUEST_ID --generation HOLD_GENERATION --reason "Proxy release and investigation findings" --accept-risk
```

This does not stop execution. The old conversation stays quarantined; explicitly select the authorized shared workspace in a new conversation. A late running observation reacquires the Gateway hold.

Gateway backup restoration uses the separate `restore --from ...` and `admin recovery release` workflow. Accepting a Proxy generation does not clear the Gateway's own restore hold. Arbitrary rollback of the entire disk, including external markers, is outside the guaranteed detection scope.
