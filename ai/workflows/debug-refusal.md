---
name: debug-refusal
title: Why the bot will not trade
description: Diagnose why the bot is refusing to place orders, or has halted, by reading the reject code in its audit trail. Use when the bot runs but never trades, or stopped trading.
---

# Why the bot will not trade

A bot that refuses to trade is usually working correctly. Each refusal carries
a code, and the code says exactly which ceiling was hit. Find the code first;
do not change any setting before you can name it.

**Explain findings in Thai.** Do not edit `risk.py` during this workflow — the
answer is in the configuration, the account or the strategy, not in the gate.

## Step 1 — find the code

```bash
tail -20 logs/audit.jsonl
hyperbot --doctor
```

Each line of `audit.jsonl` is one decision. Refusals look like:

```json
{"at": "...", "price": "...", "reject": {"code": "below_min_notional", "detail": "..."}}
```

Count them to see the pattern:

```bash
grep -o '"code": "[a-z_]*"' logs/audit.jsonl | sort | uniq -c | sort -rn
```

No `reject` lines at all and no `signal` lines either means the strategy is
not producing signals. That is Step 3, not a risk problem.

## Step 2 — read the code

Codes are listed most serious first; the gate always reports the most serious
one that applies.

| code | what it means | what to do |
|---|---|---|
| `halted` | The bot stopped itself and wrote the reason to `logs/state.json`. | Read `halt_reason`. Understand it, then `hyperbot --resume`. Never resume without reading it. |
| `max_drawdown` | Equity fell `risk.max_drawdown_pct` below its peak. | This is the kill switch. Stop and review the strategy; do not raise the limit to keep trading. |
| `daily_loss_limit` | Equity dropped more than `risk.max_daily_loss_pct` since 00:00 UTC. | Resets at the next UTC midnight. Note that it measures *all* equity change, including trades made by hand. |
| `expired` | The signal was older than its `valid_for` when it reached the gate. | Usually a slow loop or a long broker call. Check `loop_interval` and the logs for retries. |
| `short_not_allowed` | The strategy asked to sell with `risk.allow_short: false`. | Either allow shorts or make the strategy long-only. |
| `invalid_stop` | The stop was on the wrong side of entry, closer than one tick, or the take profit was on the losing side. | A bug in the strategy. Fix the strategy, add a test. |
| `slippage_exceeded` | Price moved against the signal by more than `risk.max_slippage_pct` before the gate ran. | Normal in fast markets. Persistent cases mean the loop is too slow for the symbol. |
| `already_in_position` | Something is already open on that symbol. | By design: this bot never stacks. Check whether an old position is still open: `hyperbot --doctor`. |
| `too_many_positions` | `risk.max_positions` reached. | Expected with multiple symbols; raise it only deliberately. |
| `qty_rounds_to_zero` | The risk budget cannot buy one `lot_step`. | Account too small for that stop distance. Widen the stop, raise `risk_per_trade_pct`, or add funds. |
| `below_min_notional` | The order is smaller than the exchange accepts. | The most common refusal on a small account. Same three options as above; the honest answer is often "this account is too small for this symbol". |
| `exceeds_exposure` | Total exposure would pass `equity x risk.max_leverage`. | Close something, or lower the position size by tightening risk per trade. |
| `bad_instrument` | Tick size, lot step or the mark price are unusable. | Check the `instrument:` block against the exchange's real values. |

## Step 3 — if there are no signals at all

```bash
hyperbot --dry-run --strategy <name> --iterations 50
```

Then check, in order:

1. **Warm-up.** A strategy needing 30 observations needs 30 iterations, and
   `recent_prices` is empty again after every restart. At
   `loop_interval: 60` that is half an hour.
2. **An open position.** Most strategies return `None` while in a position.
3. **The condition never happens.** Print the indicator values and compare
   them with the rule from the interview.

## Step 4 — report back

Say which code dominates, what it means in plain language, and the smallest
change that addresses it. Then say what that change costs: raising
`risk_per_trade_pct` from 1% to 2% does not make the strategy better, it
doubles the loss per trade.

If the honest answer is "the account is too small to trade this symbol with
this stop", say that. It is a real answer, and better than a configuration
that trades by ignoring its own limits.
