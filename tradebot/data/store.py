"""Candle history kept on disk, so it is downloaded once and read forever.

A broker hands back the most recent N bars and nothing more.  That is enough
to warm a strategy up and nowhere near enough to backtest one: a year of 4h
bars is about 2,200 of them, fetched in pages, and re-fetching that for every
experiment is both slow and a good way to get rate limited.

The format is CSV, one file per exchange, symbol and interval, because the
whole package runs on PyYAML alone and a backtest store is not a good reason
to make people install pandas.  Prices are written as plain text and read
straight back into :class:`~decimal.Decimal`, so a round trip through the disk
never loses a digit the way a detour through ``float`` would.

Two rules the rest of the code may rely on:

* bars are stored sorted by ``open_time``, oldest first, with no duplicates;
* :meth:`CandleStore.merge` is idempotent, so an interrupted download is
  simply run again - writing the same bars twice changes nothing.
"""

from __future__ import annotations

import csv
import os
from collections.abc import Iterable, Sequence
from dataclasses import dataclass
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation
from pathlib import Path

from ..intervals import interval_delta
from ..money import D
from ..types import Candle

DEFAULT_ROOT = Path("data/candles")

HEADER = ("open_time", "open", "high", "low", "close", "volume")

_NUMBERS = ("open", "high", "low", "close", "volume")

# Characters a symbol may keep once it becomes a file name.  Everything else
# is replaced, because "BTC/USDT:USDT" is a perfectly ordinary symbol and a
# perfectly impossible Windows path.
_SAFE = frozenset("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-")


class StoreError(RuntimeError):
    """The file on disk is not something we are willing to guess about."""


def safe_name(value: str) -> str:
    """Turn a symbol into a file name that every operating system accepts."""
    cleaned = "".join(ch if ch in _SAFE else "-" for ch in value.strip())
    return cleaned.strip("-.") or "unnamed"


def find_gaps(candles: Sequence[Candle], interval: str) -> tuple[tuple[datetime, datetime], ...]:
    """Spans where consecutive bars sit further apart than one interval.

    Each gap is reported as the pair of timestamps that straddle it, so the
    caller can hand them straight back to a downloader.  Exchanges really do
    skip bars - halts, outages, a venue that was not listed yet - so a gap is
    information to show the user, not automatically an error.
    """
    step = interval_delta(interval)
    gaps: list[tuple[datetime, datetime]] = []
    for previous, current in zip(candles, candles[1:], strict=False):
        if current.open_time - previous.open_time > step:
            gaps.append((previous.open_time, current.open_time))
    return tuple(gaps)


@dataclass(frozen=True)
class Coverage:
    """What the store holds for one symbol and interval."""

    exchange: str
    symbol: str
    interval: str
    count: int
    first: datetime | None
    last: datetime | None
    gaps: tuple[tuple[datetime, datetime], ...] = ()

    @property
    def is_empty(self) -> bool:
        return self.count == 0

    @property
    def missing_bars(self) -> int:
        """How many bars the gaps add up to."""
        step = interval_delta(self.interval)
        return sum(int((end - start) / step) - 1 for start, end in self.gaps)

    def describe(self) -> str:
        """One line for a human, for the CLI and the doctor."""
        if self.is_empty or self.first is None or self.last is None:
            return f"{self.exchange} {self.symbol} {self.interval}: nothing stored yet"
        line = (
            f"{self.exchange} {self.symbol} {self.interval}: {self.count} bars, "
            f"{self.first:%Y-%m-%d %H:%M} to {self.last:%Y-%m-%d %H:%M} UTC"
        )
        if self.gaps:
            line += f", {len(self.gaps)} gap(s) hiding {self.missing_bars} bars"
        return line


class CandleStore:
    """Reads and writes candle history under *root*."""

    def __init__(self, root: Path | str = DEFAULT_ROOT) -> None:
        self.root = Path(root)

    # --- locating ---------------------------------------------------------

    def path_for(self, exchange: str, symbol: str, interval: str) -> Path:
        return self.root / safe_name(exchange) / safe_name(symbol) / f"{safe_name(interval)}.csv"

    def exists(self, exchange: str, symbol: str, interval: str) -> bool:
        return self.path_for(exchange, symbol, interval).exists()

    def series(self) -> list[tuple[str, str, str]]:
        """Every (exchange, symbol, interval) the store holds, sorted."""
        if not self.root.exists():
            return []
        return [
            (path.parent.parent.name, path.parent.name, path.stem)
            for path in sorted(self.root.glob("*/*/*.csv"))
        ]

    # --- reading ----------------------------------------------------------

    def load(
        self,
        exchange: str,
        symbol: str,
        interval: str,
        start: datetime | None = None,
        end: datetime | None = None,
    ) -> list[Candle]:
        """Every stored bar whose open time falls in ``[start, end)``.

        A missing file reads as "no history yet" rather than raising: that is
        the case the caller has to handle anyway, and it is not an error the
        first time anyone asks.
        """
        path = self.path_for(exchange, symbol, interval)
        if not path.exists():
            return []
        out: list[Candle] = []
        with path.open("r", encoding="utf-8", newline="") as handle:
            reader = csv.DictReader(handle)
            missing = [name for name in HEADER if name not in (reader.fieldnames or ())]
            if missing:
                raise StoreError(f"{path} is missing column(s): {', '.join(missing)}")
            for line_no, row in enumerate(reader, start=2):
                candle = _row_to_candle(row, symbol, interval, path, line_no)
                if start is not None and candle.open_time < start:
                    continue
                if end is not None and candle.open_time >= end:
                    continue
                out.append(candle)
        return out

    def coverage(self, exchange: str, symbol: str, interval: str) -> Coverage:
        """Count, span and gaps for one stored series."""
        candles = self.load(exchange, symbol, interval)
        return Coverage(
            exchange=exchange,
            symbol=symbol,
            interval=interval,
            count=len(candles),
            first=candles[0].open_time if candles else None,
            last=candles[-1].open_time if candles else None,
            gaps=find_gaps(candles, interval),
        )

    # --- writing ----------------------------------------------------------

    def merge(self, exchange: str, candles: Iterable[Candle]) -> int:
        """Add *candles* to the store and return how many bars are new.

        A bar with an open time that is already stored replaces the stored
        one, because a re-download of a bar that was still forming is the
        corrected version of it.  The write lands in a temporary file that is
        then moved into place, so a crash halfway through cannot leave a
        half-written history behind.
        """
        incoming = list(candles)
        if not incoming:
            return 0
        series = {(c.symbol, c.interval) for c in incoming}
        if len(series) != 1:
            raise StoreError(f"merge takes one series at a time, got {sorted(series)}")
        symbol, interval = series.pop()

        stored = self.load(exchange, symbol, interval)
        merged: dict[datetime, Candle] = {c.open_time: c for c in stored}
        before = len(merged)
        for candle in incoming:
            if candle.open_time.tzinfo is None:
                raise StoreError(f"the bar at {candle.open_time} has no timezone; use UTC")
            merged[candle.open_time.astimezone(timezone.utc)] = candle

        path = self.path_for(exchange, symbol, interval)
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_suffix(".csv.tmp")
        with temporary.open("w", encoding="utf-8", newline="") as handle:
            writer = csv.writer(handle, lineterminator="\n")
            writer.writerow(HEADER)
            for open_time in sorted(merged):
                writer.writerow(_candle_to_row(merged[open_time]))
        os.replace(temporary, path)
        return len(merged) - before


def _candle_to_row(candle: Candle) -> list[str]:
    return [
        candle.open_time.astimezone(timezone.utc).isoformat(),
        format(candle.open, "f"),
        format(candle.high, "f"),
        format(candle.low, "f"),
        format(candle.close, "f"),
        format(candle.volume, "f"),
    ]


def _row_to_candle(
    row: dict[str, str], symbol: str, interval: str, path: Path, line_no: int
) -> Candle:
    try:
        stamp = datetime.fromisoformat(row["open_time"])
    except (KeyError, TypeError, ValueError) as exc:
        raise StoreError(f"{path} line {line_no}: bad open_time {row.get('open_time')!r}") from exc
    if stamp.tzinfo is None:
        stamp = stamp.replace(tzinfo=timezone.utc)
    try:
        values = {name: D(row[name]) for name in _NUMBERS}
    except (InvalidOperation, KeyError, TypeError, ValueError) as exc:
        raise StoreError(f"{path} line {line_no}: bad number in {row!r}") from exc
    return Candle(
        symbol=symbol,
        interval=interval,
        open_time=stamp.astimezone(timezone.utc),
        open=values["open"],
        high=values["high"],
        low=values["low"],
        close=values["close"],
        volume=values["volume"],
    )


def closes(candles: Sequence[Candle]) -> tuple[Decimal, ...]:
    """Just the closing prices, oldest first."""
    return tuple(candle.close for candle in candles)


__all__ = [
    "DEFAULT_ROOT",
    "CandleStore",
    "Coverage",
    "StoreError",
    "closes",
    "find_gaps",
    "safe_name",
]
