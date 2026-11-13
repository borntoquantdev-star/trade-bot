"""One line per run, so a board can show a history of them.

The audit trail says what happened on each tick; it does not say where one
run stopped and the next began.  Reading that back out of timestamps is
guesswork - a gap means "stopped and restarted", unless the loop interval was
long, or the machine slept, or the clock moved.  So the bot writes it down
instead: a line when a run starts and a line when it ends, sharing an id.

Two lines rather than one rewritten line, because this file is append-only
and a process that is killed outright never gets to write its second line.
A run with no end is therefore not a bug in the writer - it is a run that was
killed, which is exactly what the reader should be told.

Nothing here is allowed to break a run.  A board that cannot show a history
is a smaller problem than a bot that will not trade because a log file is
read-only.
"""

from __future__ import annotations

import json
import logging
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from decimal import Decimal
from pathlib import Path
from typing import Any

log = logging.getLogger("tradebot.runs")


@dataclass(frozen=True)
class Start:
    """What is known before the first tick."""

    strategy: str
    exchange: str
    symbol: str
    timeframe: str
    mode: str
    config: str
    iterations: int | None


def started(path: Path | None, what: Start) -> str:
    """Write the opening line and return the id that will close it."""
    run_id = uuid.uuid4().hex[:12]
    _write(
        path,
        {
            "id": run_id,
            "event": "start",
            "at": _now(),
            "strategy": what.strategy,
            "exchange": what.exchange,
            "symbol": what.symbol,
            "timeframe": what.timeframe,
            "mode": what.mode,
            "config": what.config,
            "iterations": what.iterations,
        },
    )
    return run_id


def finished(
    path: Path | None,
    run_id: str,
    *,
    ticks: int,
    orders: int,
    exits: int,
    rejects: int,
    first_equity: Decimal | None,
    last_equity: Decimal | None,
    halted: bool,
    reason: str,
) -> None:
    """Write the closing line for *run_id*."""
    _write(
        path,
        {
            "id": run_id,
            "event": "end",
            "at": _now(),
            "ticks": ticks,
            "orders": orders,
            "exits": exits,
            "rejects": rejects,
            # Money stays a string at full precision, the same as everywhere
            # else the bot writes a number down.
            "first_equity": _money(first_equity),
            "last_equity": _money(last_equity),
            "halted": halted,
            "reason": reason,
        },
    )


def _money(value: Decimal | None) -> str:
    return "" if value is None else format(value, "f")


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _write(path: Path | None, row: dict[str, Any]) -> None:
    if path is None:
        return
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(row, ensure_ascii=False) + "\n")
    except OSError as exc:  # never let bookkeeping stop a run
        log.warning("could not write the run log: %s", exc)


__all__ = ["Start", "finished", "started"]
