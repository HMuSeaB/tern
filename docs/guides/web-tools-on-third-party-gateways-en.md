# Why WebSearch and WebFetch Break on Third-Party Gateways

> This document explains why `WebSearch` and `WebFetch` stop working once Claude Code points at a third-party gateway — including through a local gateway like tern — and what to do about it. Addresses, ports, and model names below are placeholders — use your own values.
>
> tern flags third-party gateway providers in both `tern check` and `tern serve` startup. This document is the rationale behind that warning.

## The short answer

`WebSearch` and `WebFetch` do **not** travel over the `ANTHROPIC_BASE_URL` messages channel. They are separate client-side capabilities, and some rely on Anthropic-operated services to function. tern can forward `/v1/messages` flawlessly and still leave both tools completely broken — this is not a gateway bug, and there is nothing for the gateway to fix.

The two tools also fail for different reasons, and the error text tells them apart:

| Tool | Typical error | Nature | Whose problem |
|---|---|---|---|
| `WebSearch` | `API Error: 400 The input you provided is invalid` | Search request rejected or endpoint unimplemented | Gateway lacks the search endpoint |
| `WebFetch` | `Unable to verify if domain <domain> is safe to fetch` | **Preflight domain safety check failed** — page fetch never started | Check service unreachable through the gateway |

The `WebFetch` line is the one most often misread: it fires *before* any actual fetch, as a built-in domain safety check inside Claude Code. If that check cannot reach its service, every target reports the same message regardless of whether the site itself is reachable.

## A confusing but expected observation

Under these same errors, the machine's network is frequently **fine**. For example:

```powershell
curl -s -o /dev/null -w "%{http_code}" --max-time 8 https://example.com
```

A `200` here is not a contradiction — curl uses the system proxy or a direct route, while these two tools take their own egress path. "It opens in my browser" therefore does not imply "the tool will work."

## What tern does

**Detection and reporting only.** It never rewrites requests for these tools, and it cannot relay them — that is not achievable.

- `tern check` prints a "联网工具 / web tools" section listing third-party gateway providers
- `tern serve` logs one warning per third-party provider at startup

The criterion is whether the upstream host is `api.anthropic.com` (host comparison only; port, path, and userinfo are ignored). Two boundary cases follow from that:

- **False positive**: an upstream that speaks compatible Anthropic protocol *and* genuinely supports search is still flagged. Better a redundant sentence than a user concluding tern is broken.
- **Disguises handled**: `https://api.anthropic.com@evil.com/` — a real host hidden in userinfo — is correctly classified as third-party, because only the part after `@` counts.

## How to handle it

### Option 1: Disable in settings.json (cleanest for anyone who does not need the web)

If you do not need web access day to day, turn the tools off at the permission layer so Claude Code stops attempting them:

```json
{
  "permissions": {
    "deny": ["WebSearch", "WebFetch"]
  }
}
```

Notes:

- This is a **user-global** setting (`~/.claude/settings.json`) and applies to every project.
- **Merge** it with existing configuration; do not overwrite the whole file. Keep existing keys such as `env` and `includeCoAuthoredBy`.
- Deny rules are read at session startup. A new session is the reliable path; if the tool is blocked immediately in the current session, the config was hot-reloaded.
- This affects only the two web tools. Conversation and coding ability are untouched.
- tern does not manage this file. If CC Switch or another tool owns it, persistence follows that tool.

### Option 2: Skip the WebFetch preflight (when you only want WebFetch back)

If the gateway has real egress and only the domain safety check is blocked, skip that check:

```json
{
  "skipWebFetchPreflight": true
}
```

This can only ever recover `WebFetch`. It does nothing for `WebSearch`, whose problem is an upstream that does not recognize the search endpoint.

tern has no "preset" concept, so this key is yours to add to `~/.claude/settings.json`. A provider that needs it typically behaves as "can fetch pages, cannot search."

### Option 3: Switch back to the official endpoint

Both tools are fully supported against Anthropic's official endpoint. To restore that, return `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN`, and friends to official defaults — or drop the `provider/` prefix from the model name so the official endpoint serves it directly. Everything added here is reversible too: delete the `permissions.deny` and `skipWebFetchPreflight` keys, or keep just one if you want.

## Troubleshooting

| Symptom | Diagnosis |
|---|---|
| `400 The input you provided is invalid` + `WebSearch` | Gateway does not implement search. Disable search, or use an endpoint that supports it |
| `Unable to verify if domain ... is safe to fetch` + `WebFetch` | Domain check service cannot egress. Try `skipWebFetchPreflight: true` |
| Both error at once | Gateway does not cooperate with either capability. Prefer Option 1 |
| Browser works but tools still fail | Expected — different egress paths; not a counterexample |
| Errors persist after adding deny | Config not applied: confirm the write went to `~/.claude/settings.json` and restart the session |
| `tern check` did not flag my third-party gateway | The host is `api.anthropic.com` or a disguise of it; see the criterion above |

## Remarks

- `WebSearch` availability also depends on account entitlement; an official endpoint does not guarantee results.
- The warning wording here and the text printed by `tern check` / `tern serve` come from the same source. Change one and the other follows.
