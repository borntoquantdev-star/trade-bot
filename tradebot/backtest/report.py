"""Turning a result into something a person can act on.

A backtest summary is read by someone deciding whether to risk money, often
someone who has never read one before.  So this prints the costs next to the
profit, says plainly when a run produced nothing and why, and refuses to
present a 12-trade sample as if it settled anything.

Plain text, ASCII only: this lands in a Windows console as often as a
terminal, and a box-drawing character that renders as a question mark is a
line nobody reads.
"""

from __future__ import annotations

import json
from collections.abc import Sequence
from decimal import ROUND_HALF_UP, Decimal
from pathlib import Path

from ..money import D
from ..types import RejectCode
from .result import BacktestResult, Trade

#: Below this, the statistics are anecdotes.  Chosen to be visibly arbitrary
#: rather than falsely precise: no sample size makes a backtest a promise.
THIN_SAMPLE = 30


def money(value: Decimal, places: str = "0.01") -> str:
    return f"{value.quantize(D(places), rounding=ROUND_HALF_UP):,}"


def pct(value: Decimal) -> str:
    return f"{value.quantize(D('0.01'), rounding=ROUND_HALF_UP)}%"


def summary(result: BacktestResult) -> str:
    """The headline block."""
    lines: list[str] = []
    span = ""
    if result.start and result.end:
        span = f"  {result.start:%Y-%m-%d} to {result.end:%Y-%m-%d}"
    lines.append(f"BACKTEST  {result.strategy} on {result.symbol} {result.interval}{span}")
    if result.exchange:
        lines.append(f"data      {result.exchange}, {result.bars} bars")
    else:
        lines.append(f"data      {result.bars} bars")
    lines.append("")

    lines.append("RESULT")
    lines.append(f"  starting equity   {money(result.starting_equity)}")
    lines.append(f"  final equity      {money(result.final_equity)}")
    lines.append(f"  net profit        {money(result.net_pnl)}  ({pct(result.return_pct)})")
    cagr = result.cagr_pct
    lines.append(f"  annualised        {pct(cagr) if cagr is not None else 'period too short'}")
    lines.append(f"  fees paid         {money(result.fees_paid)}")
    lines.append(
        f"  max drawdown      {money(result.max_drawdown)}  ({pct(result.max_drawdown_pct)})"
    )
    lines.append(
        f"  time underwater   {result.days_underwater.quantize(D('0.1'))} days"
        f"  ({result.bars_underwater} bars below the previous peak)"
    )
    worst_month = result.worst_month
    if worst_month is not None:
        lines.append(f"  worst month       {worst_month[0]}  {pct(worst_month[1])}")
    lines.append("")

    held = result.buy_and_hold_pct
    if held is not None:
        verdict = "the bot won" if result.return_pct > held else "holding won"
        lines.append("AGAINST DOING NOTHING")
        lines.append(f"  bot               {pct(result.return_pct)}")
        lines.append(f"  buy and hold      {pct(held)}   <- {verdict}")
        if result.return_pct <= held:
            lines.append("  All this machinery lost a race against sitting still.")
        lines.append("")

    lines.append("TRADES")
    if not result.trades:
        lines.append(f"  none. {result.never_traded_because()}")
    else:
        factor = result.profit_factor
        lines.append(
            f"  closed            {len(result.trades)}"
            f"  ({len(result.wins)} won, {len(result.losses)} lost)"
        )
        lines.append(f"  per year          {result.trades_per_year.quantize(D('0.1'))}")
        ratio = result.win_loss_ratio
        lines.append(
            f"  win rate          {pct(result.win_rate_pct)}"
            f"   (average win / average loss: "
            f"{ratio.quantize(D('0.01')) if ratio is not None else 'n/a'})"
        )
        lines.append(
            f"  profit factor     {factor.quantize(D('0.01')) if factor is not None else 'n/a'}"
        )
        lines.append(f"  expectancy        {result.expectancy_r.quantize(D('0.01'))}R per trade")
        sharpe, sortino = result.sharpe(), result.sortino()
        lines.append(
            f"  sharpe / sortino  "
            f"{sharpe.quantize(D('0.01')) if sharpe is not None else 'n/a'}"
            f" / {sortino.quantize(D('0.01')) if sortino is not None else 'n/a'}"
        )
        lines.append(f"  longest losing    {result.longest_losing_streak} in a row")
        lines.append(f"  time in market    {pct(result.exposure_pct)}")
        without = result.profit_without_best(2)
        lines.append(
            f"  minus best 2      {money(without)}"
            + ("   <- the result rests on two trades" if without <= 0 < result.net_pnl else "")
        )
        best, worst = result.best_trade, result.worst_trade
        if best is not None and worst is not None:
            lines.append(f"  best / worst      {money(best.net_pnl)} / {money(worst.net_pnl)}")
    lines.append("")

    if result.rejects:
        lines.append("REFUSED BY THE RISK GATE")
        for code, count in result.top_rejects:
            lines.append(f"  {code:<20} {count}")
        lines.append("")

    if result.unfilled_orders:
        lines.append(
            f"UNFILLED  {result.unfilled_orders} approved order(s) never met their limit price"
        )
        lines.append("")

    if result.was_halted:
        lines.append("HALTED")
        lines.append(f"  at {result.halted_at:%Y-%m-%d %H:%M} UTC")
        lines.append(f"  {result.halt_reason}")
        if result.halt_reason.startswith(f"{RejectCode.DAILY_LOSS_LIMIT.value}:"):
            lines.append("  The live bot would have stopped here until midnight, Thai time.")
        else:
            lines.append("  The live bot would have stopped here and waited for a human.")
        lines.append("")

    for warning in result.warnings:
        lines.append(f"WARNING   {warning}")
    if result.warnings:
        lines.append("")

    lines.extend(caveats(result))
    return "\n".join(lines)


def caveats(result: BacktestResult) -> list[str]:
    """What this result does not prove.

    Printed every time, not as a footnote.  The gap between a backtest and a
    live account is where beginners lose money, and it is not a gap you can
    close by reading a bigger number.
    """
    lines = ["READ THIS BEFORE BELIEVING ANY OF IT"]
    if len(result.trades) < THIN_SAMPLE:
        lines.append(
            f"  * {len(result.trades)} trades is too few to mean anything. "
            f"Below about {THIN_SAMPLE}, luck is louder than edge."
        )
    lines.append("  * One symbol, one period, one set of parameters. Change any of")
    lines.append("    them and the number changes; that is the test, not the result.")
    lines.append("  * Funding on a perpetual is not modelled. Holding for days costs")
    lines.append("    more than this says.")
    lines.append("  * Fills assume your order was small enough not to move the market.")
    lines.append("  * The next step is paper trading, not real money.")
    return lines


def trade_table(trades: Sequence[Trade], limit: int = 20) -> str:
    """The individual round trips, newest last."""
    if not trades:
        return "no trades"
    shown = list(trades)[-limit:]
    head = (
        f"{'entry':<17}{'exit':<17}{'side':<6}{'qty':>12}"
        f"{'in':>12}{'out':>12}{'net':>12}{'R':>7}  reason"
    )
    rows = [head, "-" * len(head)]
    for trade in shown:
        rows.append(
            f"{trade.entry_at:%Y-%m-%d %H:%M}  "
            f"{trade.exit_at:%Y-%m-%d %H:%M}  "
            f"{trade.side.value:<6}"
            f"{trade.qty:>12}"
            f"{trade.entry_price:>12}"
            f"{trade.exit_price:>12}"
            f"{money(trade.net_pnl):>12}"
            f"{trade.r_multiple.quantize(D('0.01')):>7}"
            f"  {trade.exit_reason}"
        )
    if len(trades) > limit:
        rows.append(f"({len(trades) - limit} earlier trades not shown)")
    return "\n".join(rows)


def to_dict(result: BacktestResult) -> dict:
    """A JSON-ready view, for the dashboard and for diffing two runs.

    Numbers are strings on purpose: this file is read by a browser, and
    JavaScript would turn a Decimal into the nearest float it fancies.
    """
    factor = result.profit_factor
    sharpe, sortino = result.sharpe(), result.sortino()
    worst = result.worst_month
    ratio = result.win_loss_ratio
    held = result.buy_and_hold_pct
    return {
        "symbol": result.symbol,
        "interval": result.interval,
        "strategy": result.strategy,
        "exchange": result.exchange,
        "start": result.start.isoformat() if result.start else None,
        "end": result.end.isoformat() if result.end else None,
        "bars": result.bars,
        "starting_equity": format(result.starting_equity, "f"),
        "final_equity": format(result.final_equity, "f"),
        "net_pnl": format(result.net_pnl, "f"),
        "return_pct": format(result.return_pct, "f"),
        "fees_paid": format(result.fees_paid, "f"),
        "max_drawdown": format(result.max_drawdown, "f"),
        "max_drawdown_pct": format(result.max_drawdown_pct, "f"),
        "cagr_pct": format(result.cagr_pct, "f") if result.cagr_pct is not None else None,
        "buy_and_hold_pct": format(held, "f") if held is not None else None,
        "bars_underwater": result.bars_underwater,
        "days_underwater": format(result.days_underwater, "f"),
        "worst_month": {"month": worst[0], "return_pct": format(worst[1], "f")} if worst else None,
        "monthly_returns": [
            {"month": month, "return_pct": format(value, "f")}
            for month, value in result.monthly_returns
        ],
        "sharpe": format(sharpe, "f") if sharpe is not None else None,
        "sortino": format(sortino, "f") if sortino is not None else None,
        "win_loss_ratio": format(ratio, "f") if ratio is not None else None,
        "profit_without_best_two": format(result.profit_without_best(2), "f"),
        "trades_per_year": format(result.trades_per_year, "f"),
        "trades": len(result.trades),
        "wins": len(result.wins),
        "losses": len(result.losses),
        "win_rate_pct": format(result.win_rate_pct, "f"),
        "profit_factor": format(factor, "f") if factor is not None else None,
        "expectancy_r": format(result.expectancy_r, "f"),
        "longest_losing_streak": result.longest_losing_streak,
        "exposure_pct": format(result.exposure_pct, "f"),
        "signals": result.signals,
        "unfilled_orders": result.unfilled_orders,
        "no_trade_reason": result.no_trade_reason,
        # The dashboard warns about a thin sample too, and must not keep its
        # own copy of the threshold: one judgement, one number, two readers.
        "thin_sample_below": THIN_SAMPLE,
        "rejects": dict(result.rejects),
        "halted_at": result.halted_at.isoformat() if result.halted_at else None,
        "halt_reason": result.halt_reason,
        "warnings": list(result.warnings),
        "equity_curve": [
            {"at": point.at.isoformat(), "equity": format(point.equity, "f")}
            for point in result.equity_curve
        ],
        "trade_list": [
            {
                "side": trade.side.value,
                "qty": format(trade.qty, "f"),
                "entry_at": trade.entry_at.isoformat(),
                "entry_price": format(trade.entry_price, "f"),
                "exit_at": trade.exit_at.isoformat(),
                "exit_price": format(trade.exit_price, "f"),
                "net_pnl": format(trade.net_pnl, "f"),
                "r_multiple": format(trade.r_multiple, "f"),
                "exit_reason": trade.exit_reason,
            }
            for trade in result.trades
        ],
    }


def write_json(result: BacktestResult, path: Path | str) -> Path:
    """Save the machine-readable view next to the logs."""
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(to_dict(result), indent=2, ensure_ascii=False), encoding="utf-8")
    return target


def compare(before: BacktestResult, after: BacktestResult) -> str:
    """Two runs side by side, for tuning without fooling yourself."""
    rows = [
        ("net profit", money(before.net_pnl), money(after.net_pnl)),
        ("return", pct(before.return_pct), pct(after.return_pct)),
        ("max drawdown", pct(before.max_drawdown_pct), pct(after.max_drawdown_pct)),
        ("trades", str(len(before.trades)), str(len(after.trades))),
        ("win rate", pct(before.win_rate_pct), pct(after.win_rate_pct)),
        (
            "expectancy",
            f"{before.expectancy_r.quantize(D('0.01'))}R",
            f"{after.expectancy_r.quantize(D('0.01'))}R",
        ),
    ]
    width = max(len(name) for name, _, _ in rows)
    lines = [f"{'':<{width}}  {'before':>14}  {'after':>14}"]
    for name, left, right in rows:
        lines.append(f"{name:<{width}}  {left:>14}  {right:>14}")
    if len(after.trades) < THIN_SAMPLE or len(before.trades) < THIN_SAMPLE:
        lines.append("")
        lines.append("Both samples are small. A better number here is not yet evidence.")
    return "\n".join(lines)


__all__ = [
    "THIN_SAMPLE",
    "caveats",
    "compare",
    "money",
    "pct",
    "summary",
    "to_dict",
    "trade_table",
    "write_json",
]
