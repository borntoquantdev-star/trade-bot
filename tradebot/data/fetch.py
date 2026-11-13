"""Walking a venue's history pages into the store.

Exchanges serve candles a page at a time.  Asking for a year in one call does
not fail loudly; it comes back truncated, and a backtest run on a silently
truncated series is worse than one that refused to start.  So this module
walks forward page by page, writing each page to disk as it arrives.

Two decisions worth knowing about:

* **The bar that is still forming is dropped.**  Its high, low and close are
  whatever the market has done so far this hour, and they will change.  A
  strategy that sees one in a backtest is reading a bar that did not exist
  yet, which flatters the result for no reason.
* **A download resumes.**  Pages land in the store immediately, and the next
  run starts from the last bar already stored, so a connection that drops
  halfway costs the remaining pages and nothing else.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Protocol, runtime_checkable

from ..intervals import interval_delta
from ..types import Candle
from .store import CandleStore, find_gaps

#: How many empty pages in a row mean "the venue has nothing further", rather
#: than "this particular window happens to be a hole".
EMPTY_PAGES_BEFORE_STOPPING = 3

#: A ceiling on page requests, so a venue that keeps answering the same window
#: cannot spin the loop forever.
MAX_PAGES = 2_000

ProgressHook = Callable[["DownloadReport"], None]


@runtime_checkable
class HistorySource(Protocol):
    """The slice of a broker a download needs.

    Narrower than the full broker on purpose: a download needs no credentials,
    places no orders, and a test can satisfy this with twenty lines.
    """

    name: str
    max_candles_per_request: int

    def fetch_candles_range(
        self, symbol: str, interval: str, start: datetime, end: datetime
    ) -> list[Candle]:
        """One page of bars with open times in [start, end), oldest first."""


@dataclass(frozen=True)
class DownloadReport:
    """What one download did, for logs, tests and the CLI."""

    exchange: str
    symbol: str
    interval: str
    start: datetime
    end: datetime
    pages: int = 0
    fetched: int = 0
    added: int = 0
    first: datetime | None = None
    last: datetime | None = None
    gaps: tuple[tuple[datetime, datetime], ...] = ()
    stopped_early: str = ""

    @property
    def missing_bars(self) -> int:
        step = interval_delta(self.interval)
        return sum(int((gap_end - gap_start) / step) - 1 for gap_start, gap_end in self.gaps)

    def describe(self) -> str:
        """One line for a human."""
        if self.fetched == 0:
            return (
                f"{self.exchange} {self.symbol} {self.interval}: nothing new "
                f"({self.stopped_early or 'already up to date'})"
            )
        line = (
            f"{self.exchange} {self.symbol} {self.interval}: {self.fetched} bars in "
            f"{self.pages} page(s), {self.added} new"
        )
        if self.first and self.last:
            line += f", {self.first:%Y-%m-%d %H:%M} to {self.last:%Y-%m-%d %H:%M} UTC"
        if self.gaps:
            line += f", {len(self.gaps)} gap(s) hiding {self.missing_bars} bars"
        if self.stopped_early:
            line += f" [stopped: {self.stopped_early}]"
        return line


def closed_bars(candles: list[Candle], interval: str, now: datetime) -> list[Candle]:
    """Drop any bar whose interval has not finished yet."""
    step = interval_delta(interval)
    return [candle for candle in candles if candle.open_time + step <= now]


def download(
    broker: HistorySource,
    store: CandleStore,
    symbol: str,
    interval: str,
    start: datetime,
    end: datetime,
    exchange: str | None = None,
    log: logging.Logger | None = None,
    refresh: bool = False,
    now: datetime | None = None,
    on_page: ProgressHook | None = None,
) -> DownloadReport:
    """Fill the store with *symbol* history between *start* and *end*.

    Args:
        broker: Anything with ``fetch_candles_range`` and
            ``max_candles_per_request``; the live brokers and the paper broker
            all qualify, which is what lets the whole path be tested offline.
        store: Where the bars land.  Written to once per page.
        symbol: The venue's own symbol, unchanged.
        interval: A key of :data:`tradebot.intervals.INTERVAL_SECONDS`.
        start: First bar wanted, inclusive.
        end: Last bar wanted, exclusive.
        exchange: Folder name in the store.  Defaults to the broker's ``name``.
        refresh: Re-download bars the store already has.  Off by default: the
            point of the store is to not ask twice.
        now: Overridable clock, so tests are not at the mercy of the hour.
        on_page: Called with a running report after every page, for progress
            output on a download that takes minutes.

    Returns:
        A :class:`DownloadReport`.  Partial downloads are already on disk, so
        a report that stopped early is still progress, not a rollback.
    """
    log = log or logging.getLogger(__name__)
    now = now or datetime.now(timezone.utc)
    venue = exchange or str(getattr(broker, "name", "unknown"))
    fetch = getattr(broker, "fetch_candles_range", None)
    if not callable(fetch):
        raise TypeError(
            f"{venue} broker has no fetch_candles_range; it cannot serve history. "
            "Use the paper broker or an exchange that publishes candles."
        )
    if start.tzinfo is None or end.tzinfo is None:
        raise ValueError("start and end must be timezone-aware (use UTC)")

    step = interval_delta(interval)
    page_span = step * max(int(getattr(broker, "max_candles_per_request", 500)), 1)
    # Never ask for bars that cannot have closed yet.
    end = min(end, now)

    cursor = start
    if not refresh:
        stored = store.coverage(venue, symbol, interval)
        if stored.last is not None and stored.last >= start:
            cursor = stored.last + step

    pages = fetched = added = 0
    first_seen: datetime | None = None
    last_seen: datetime | None = None
    empty_streak = 0
    stopped = ""

    while cursor < end:
        if pages >= MAX_PAGES:
            stopped = f"hit the {MAX_PAGES} page ceiling"
            break
        pages += 1
        page = closed_bars(list(fetch(symbol, interval, cursor, end)), interval, now)
        if not page:
            empty_streak += 1
            if empty_streak >= EMPTY_PAGES_BEFORE_STOPPING:
                stopped = "the venue has no more history here"
                break
            # Step over the hole rather than asking for the same window again.
            cursor += page_span
            continue

        empty_streak = 0
        page.sort(key=lambda candle: candle.open_time)
        added += store.merge(venue, page)
        fetched += len(page)
        first_seen = first_seen or page[0].open_time
        last_seen = page[-1].open_time
        log.debug(
            "%s %s %s: page %s, %s bars to %s",
            venue,
            symbol,
            interval,
            pages,
            len(page),
            last_seen.isoformat(timespec="minutes"),
        )

        advanced = page[-1].open_time + step
        if advanced <= cursor:
            # A venue that keeps answering the same window would loop forever.
            stopped = "the venue stopped moving forward"
            break
        cursor = advanced

        if on_page is not None:
            on_page(
                DownloadReport(
                    exchange=venue,
                    symbol=symbol,
                    interval=interval,
                    start=start,
                    end=end,
                    pages=pages,
                    fetched=fetched,
                    added=added,
                    first=first_seen,
                    last=last_seen,
                )
            )

    stored_now = store.load(venue, symbol, interval, start, end)
    return DownloadReport(
        exchange=venue,
        symbol=symbol,
        interval=interval,
        start=start,
        end=end,
        pages=pages,
        fetched=fetched,
        added=added,
        first=first_seen,
        last=last_seen,
        gaps=find_gaps(stored_now, interval),
        stopped_early=stopped,
    )


__all__ = ["DownloadReport", "HistorySource", "closed_bars", "download"]
