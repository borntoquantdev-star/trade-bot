"""Walking a venue's history pages.

The fake venue below is the point of the exercise: a download has to cope with
short pages, holes, bars that have not closed, and a server that stops moving
forward - none of which can be arranged on demand against a real exchange.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone

import pytest

from tradebot.data.fetch import HistorySource, closed_bars, download
from tradebot.data.store import CandleStore
from tradebot.money import D
from tradebot.types import Candle

SYMBOL = "BTC_USDT_Perp"
INTERVAL = "1h"
STEP = timedelta(hours=1)
GENESIS = datetime(2026, 1, 1, tzinfo=timezone.utc)
NOW = GENESIS + timedelta(days=5)


class FakeVenue:
    """A venue with a finite history and a page limit."""

    name = "fake"

    def __init__(self, bars: int = 100, page: int = 10, missing: set[int] | None = None) -> None:
        self.max_candles_per_request = page
        self.missing = missing or set()
        self.calls: list[tuple[datetime, datetime]] = []
        self.history = [self._bar(i) for i in range(bars) if i not in self.missing]

    @staticmethod
    def _bar(index: int) -> Candle:
        price = D(100 + index)
        return Candle(
            symbol=SYMBOL,
            interval=INTERVAL,
            open_time=GENESIS + STEP * index,
            open=price,
            high=price + 1,
            low=price - 1,
            close=price,
            volume=D("1"),
        )

    def fetch_candles_range(
        self, symbol: str, interval: str, start: datetime, end: datetime
    ) -> list[Candle]:
        self.calls.append((start, end))
        window = [c for c in self.history if start <= c.open_time < end]
        return window[: self.max_candles_per_request]


@pytest.fixture()
def store(tmp_path):
    return CandleStore(tmp_path / "candles")


def test_it_walks_pages_until_the_whole_span_is_covered(store):
    venue = FakeVenue(bars=100, page=10)
    report = download(venue, store, SYMBOL, INTERVAL, GENESIS, NOW, now=NOW)
    assert report.fetched == 100
    assert report.pages >= 10
    assert store.coverage("fake", SYMBOL, INTERVAL).count == 100


def test_a_second_download_resumes_instead_of_fetching_it_all_again(store):
    venue = FakeVenue(bars=100, page=10)
    download(venue, store, SYMBOL, INTERVAL, GENESIS, NOW, now=NOW)
    calls_after_first = len(venue.calls)

    again = download(venue, store, SYMBOL, INTERVAL, GENESIS, NOW, now=NOW)
    assert again.added == 0
    assert len(venue.calls) - calls_after_first <= 3


def test_refresh_asks_for_bars_that_are_already_stored(store):
    venue = FakeVenue(bars=20, page=10)
    download(venue, store, SYMBOL, INTERVAL, GENESIS, NOW, now=NOW)
    again = download(venue, store, SYMBOL, INTERVAL, GENESIS, NOW, now=NOW, refresh=True)
    assert again.fetched == 20
    assert again.added == 0  # same bars, so nothing is new


def test_the_bar_that_has_not_closed_yet_is_dropped(store):
    venue = FakeVenue(bars=10, page=10)
    # Mid-way through the bar that opened at index 9.
    now = GENESIS + STEP * 9 + timedelta(minutes=30)
    report = download(venue, store, SYMBOL, INTERVAL, GENESIS, now + STEP, now=now)
    assert report.fetched == 9
    assert report.last == GENESIS + STEP * 8


def test_it_steps_over_a_hole_in_the_history(store):
    venue = FakeVenue(bars=60, page=5, missing=set(range(10, 25)))
    report = download(venue, store, SYMBOL, INTERVAL, GENESIS, NOW, now=NOW)
    assert report.fetched == 45
    assert len(report.gaps) == 1
    assert report.missing_bars == 15


def test_it_stops_after_enough_empty_pages(store):
    venue = FakeVenue(bars=5, page=10)
    report = download(venue, store, SYMBOL, INTERVAL, GENESIS, NOW, now=NOW)
    assert report.fetched == 5
    assert "no more history" in report.stopped_early


def test_a_venue_that_never_moves_forward_does_not_loop_forever(store):
    class StuckVenue(FakeVenue):
        def fetch_candles_range(self, symbol, interval, start, end):
            return [self._bar(0)]  # always the same bar

    report = download(StuckVenue(), store, SYMBOL, INTERVAL, GENESIS, NOW, now=NOW)
    assert "stopped moving forward" in report.stopped_early


def test_it_never_asks_for_bars_in_the_future(store):
    venue = FakeVenue(bars=200, page=50)
    download(venue, store, SYMBOL, INTERVAL, GENESIS, GENESIS + timedelta(days=365), now=NOW)
    assert all(end <= NOW for _, end in venue.calls)


def test_partial_progress_survives_a_venue_that_fails_halfway(store):
    class FlakyVenue(FakeVenue):
        def fetch_candles_range(self, symbol, interval, start, end):
            if len(self.calls) >= 3:
                raise ConnectionError("the venue hung up")
            return super().fetch_candles_range(symbol, interval, start, end)

    venue = FlakyVenue(bars=100, page=10)
    with pytest.raises(ConnectionError):
        download(venue, store, SYMBOL, INTERVAL, GENESIS, NOW, now=NOW)
    # The pages that did arrive are on disk, so the next run resumes.
    assert store.coverage("fake", SYMBOL, INTERVAL).count == 30


def test_a_broker_with_no_history_feed_is_refused_with_advice(store):
    class NoHistory:
        name = "blind"

    with pytest.raises(TypeError, match="no fetch_candles_range"):
        download(NoHistory(), store, SYMBOL, INTERVAL, GENESIS, NOW, now=NOW)


def test_naive_timestamps_are_refused(store):
    with pytest.raises(ValueError, match="timezone-aware"):
        download(FakeVenue(), store, SYMBOL, INTERVAL, datetime(2026, 1, 1), NOW, now=NOW)


def test_the_progress_hook_sees_each_page(store):
    seen: list[int] = []
    venue = FakeVenue(bars=50, page=10)
    download(
        venue,
        store,
        SYMBOL,
        INTERVAL,
        GENESIS,
        NOW,
        now=NOW,
        on_page=lambda r: seen.append(r.fetched),
    )
    assert seen == [10, 20, 30, 40, 50]


def test_closed_bars_keeps_only_finished_ones():
    bars = [FakeVenue._bar(i) for i in range(3)]
    kept = closed_bars(bars, INTERVAL, GENESIS + STEP * 2)
    assert len(kept) == 2


def test_the_paper_broker_satisfies_the_history_protocol():
    from tradebot.brokers.paper import PaperBroker, fixed_price
    from tradebot.types import InstrumentSpec

    broker = PaperBroker(
        price_source=fixed_price(D("100")),
        instrument=InstrumentSpec.build(SYMBOL, "0.1", "0.001", "10"),
    )
    assert isinstance(broker, HistorySource)
