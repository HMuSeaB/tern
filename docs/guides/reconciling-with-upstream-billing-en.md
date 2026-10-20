# Reconciling tern's usage numbers with an upstream invoice

> What this covers: how to line up what `tern usage` reports against the invoice your upstream provider sends, which gaps are **differences in what each side counts** (expected, and explainable), and which ones mean **tern actually got it wrong** (worth fixing). Dates, model names, and amounts are sanitized.
>
> Step one is `tern usage --from <date> --to <date>`. Upstream invoices are cut per billing period, while `--days` is a rolling window that drifts every day — pointing that at a fixed-date invoice guarantees a mismatch of a few days.

## The short version

**tern's number and the upstream invoice will not match line for line, and should not.** Gaps fall into two buckets:

- **Differences in scope or rules**: the two sides count different sets of requests, or apply different billing rules to the same set. These are regular, explainable, and quantifiable. This document lists them.
- **Actual bugs**: tern mis-transcribed a token count, matched the wrong price, or dropped a multiplier. These have no pattern, and you can't explain the gap.

So the goal of reconciliation is not "make them equal". It's **being able to attribute the entire gap to one of the sections below**. Whatever is left over is the part worth investigating.

## Exporting

```powershell
tern usage --from 2026-10-01 --to 2026-10-31 --by model
tern usage --from 2026-10-01 --to 2026-10-31 --by day --recent 0
```

- `--by model` gives the split by the model actually billed — the closest thing to the model detail on an invoice.
- `--by day` gives a per-day split, for locating which days the gap sits in.
- `--recent 0` turns off the trailing request list, which is noise when you're comparing totals.

Output is text only; there is no JSON or CSV. Copy the block wholesale — the columns are aligned for a fixed-width font.

**Confirm the price table is stable before exporting.** `tern price set` recomputes history retroactively (the "historical records are repriced automatically" behavior). So after you fill in a price, exporting the same range gives a different answer than before. Run `tern price list` first, and don't change prices mid-reconciliation.

## The list of inherent differences

Ordered by how far each one can move the number. The first two usually account for most of the gap.

| # | Difference | tern vs. invoice | Magnitude |
|---|---|---|---|
| 1 | Aborted / mid-stream failures | tern higher | can be large |
| 2 | Unpriced models | tern lower | can be large |
| 3 | Cache read/write field conventions | either way | medium |
| 4 | Pricing by the echoed model name after a swap | either way | can be large |
| 5 | `costMultiplier` | either way | fixed ratio |
| 6 | Billing-period edges cut on local days | either way | concentrated on edge days |
| 7 | Only traffic that passed through tern | tern lower | depends on usage |

### 1. Aborted requests and mid-stream failures

tern records **the tokens it saw**. Three cases:

| Case | What tern records | What upstream usually does |
|---|---|---|
| Failed before a 2xx (4xx / 5xx / rate limit / connection failure) | 0 tokens, 0 cost, one failure counted | usually also nothing. **No gap from this case** |
| Stream errored partway through | billed on the usage block parsed before the error | may not bill the request at all |
| Client disconnected (shown as 中断 / aborted) | bills the tokens actually generated | most bill what was generated; some relays don't bill interrupted streams |

So the latter two are what open a gap. What you can isolate:

```powershell
tern usage --from 2026-10-01 --to 2026-10-31 --recent 0
```

The "失败（N 次，不计入上面的模型 token）" section gives you a failure **count**. That first case contributes zero tokens and zero cost, so it no longer affects the total.

The other two are the awkward part: **aborted tokens and cost are booked into the total for real**, and the CLI has no "aborted" line. `--by role` gives requests and tokens per role, but aborted is folded into the role rather than broken out. To see them one by one, the only place is the panel's 最近请求 (recent requests) table, where every row is marked 成功 / 中断 / 失败 (success / aborted / failed).

In practice: when the gap concentrates on long sessions, subagents, and Ctrl+C'd tasks, it's this category. Relays frequently don't bill those.

### 2. Unpriced models

`tern usage` lists them explicitly:

```
⚠ 3 个请求的模型没有定价，花费未计入：
```

Those requests recorded tokens but no money. So **whenever that section is non-empty, tern's total is necessarily low**, and the size of the shortfall is exactly those models' tokens at the correct prices.

Fix:

```powershell
tern price set <model> <input> <output> [<cache read> [<cache write>]]
tern price sync          # or sync from models.dev
```

History is repriced automatically, so re-export and it lines up. **This is the only one of the seven that goes to zero once you fix it**, which is why it's the first thing to check.

### 3. Cache read/write field conventions

Claude-family caching bills in two pieces: writes (`cache_creation_input_tokens`, slightly above list price) and hits (`cache_read_input_tokens`, well below). tern prices each separately, from the fields the upstream echoed.

The gap comes from upstreams echoing differently:

- Some upstreams **don't echo the cache fields at all**, filling 0. tern then bills that input at full price — result is high.
- Some fold cache hits into `input_tokens` without breaking them out. tern treats them as full-price input — also high.
- Some echo at a different granularity than the official API (per-request totals rather than per-block).

How to tell: look at the panel's 缓存命中率 (cache hit rate). If Claude Code is clearly reusing context (long sessions, repeatedly reading the same files) and the hit rate is near zero, the upstream probably isn't echoing — it's not that nothing was actually cached.

### 4. Pricing by the echoed model name after a swap

tern matches prices against the **model name the upstream echoed**, and only falls back to the name you sent. The reason is that relays often remap the request model once more, and what actually gets billed is the echoed one.

This creates a reconciliation trap: **if the name the upstream echoes is not the same string as the billed model name on the invoice, the price match fails.** Typical cases:

- Upstream echoes `claude-sonnet-4-6`, the invoice says `claude-sonnet-4-6-20260218` — no match, falls through to unpriced.
- Upstream echoes its own alias (`step-5-preview`), the invoice uses the official name — same problem.

So difference #2 (unpriced) is often a **symptom** of #4 rather than a cause. When the unpriced list has entries, compare those strings against the invoice first.

The panel's 模型被换 (model swapped) alert exists to catch exactly this: the model the client asked for ≠ the model actually billed. On any day that alert is non-empty, this section is worth reading.

### 5. `costMultiplier`

When a relay bills at a discount, you set `"costMultiplier": "0.3"` on the provider and tern applies it. Gaps come from:

- The invoice is at **list price**, with the discount returned in a separate cycle.
- The invoice uses a different multiplier (tiered discounts, and your range falls in another tier).
- The multiplier is misconfigured, or was changed mid-range — requests recorded before the change still carry the old one.

This one is a **fixed ratio**, so it's the easiest to recognize: divide tern's number by the invoice's. If you get a constant, this is it.

### 6. Billing-period edges cut on local days

tern's `day` column is cut on the **local machine's** natural day, and so are `--from` / `--to`. An upstream invoice may be cut on UTC, or on their own period (say, the 1st of the month at 00:00 UTC).

Consequence: requests on the first and last day or two land in different ranges. The magnitude is small, but it means the totals **always differ by a little**, and keep differing after you've explained everything else.

How to tell: `--by day`, then look at the first and last days. If the residual gap is about one day's worth, it's this.

### 7. Only traffic that passed through tern

`usage.db` holds only requests that went through tern. Sessions that went direct to the upstream (another terminal without `ANTHROPIC_BASE_URL` set, or a different cc-switch slot) are not in there.

This is most visible **in the first few days after wiring up**: usage from before the wiring was never in tern to begin with. Any stretch where the panel's 接入 (wire) page showed 未接入 (not wired) is a hole.

## Reconciliation checklist

Work it in order; each step rules out one category.

1. Is `⚠ N 个请求的模型没有定价` empty? If not → fill in prices, re-export, category 2 goes to zero.
2. Are the model names in the unpriced list the same strings as the billed names on the invoice? If not → category 4; the price keys need to match the invoice's names.
3. Does the panel show a 模型被换 (model swapped) alert? If yes → for those days, check both the priced model and the swapped-to model against the invoice.
4. Is tern ÷ invoice a near-constant ratio? If yes → category 5; check `costMultiplier`.
5. Is the residual gap about one day's worth at a period edge? If yes → category 6, timezone boundary.
6. Does the residual gap concentrate on long sessions, subagents, and Ctrl+C'd tasks? If yes → category 1, aborted billing. Per-request confirmation is only in the panel's 最近请求 table.
7. Is the cache hit rate clearly below what you'd expect from feel? If yes → category 3, upstream not echoing cache fields.
8. None of the above → *now* it's worth suspecting tern got it wrong. Record the range, the `--by model` output, and the invoice detail together, and attach the output of `tern check`.

## Notes

- Categories 1, 3, and 4 can push in either direction, so "tern is higher than the invoice" says nothing on its own — attribute it first.
- Category 2 is the only one that goes to zero once fixed, so it always goes first.
- The panel's three first-screen alerts (model swapped / failures / unpriced) correspond to categories 4, 1, and 2 here. On days when the first screen is clean, reconciliation goes much faster.
- The criteria in this document share their source with `tern usage`'s output: the `day` column is cut on local days, the priced model prefers what the upstream echoed, and the multiplier comes from the provider config. Change it in one place and both follow.
