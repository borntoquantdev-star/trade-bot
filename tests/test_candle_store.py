"""The candle store: what it promises the backtester.

A backtest is only as trustworthy as the bars it ran on, so the store has to
be boring in very specific ways - idempotent, lossless, and honest about what
is missing.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone

import pytest

from tradebot.data.store import CandleStore, StoreError, find_gaps, safe_name
from tradebot.money import D
from tradebot.types import Candle

SYMBOL = "BTC_USDT_Perp"
INTERVAL = "4h"
STEP = timedelta(hours=4)
START = datetime(2026, 1, 1, tzinfo=timezone.utc)


def bar(index: int, close: str = "100.25", symbol: str = SYMBOL) -> Candle:
    return Candle(
        symbol=symbol,
        interval=INTERVAL,
        open_time=START + STEP * index,
        open=D("100.5"),
        high=D("101"),
        low=D("99"),
        close=D(close),
        volume=D("7"),
    )


@pytest.fixture()
def store(tmp_path):
    return CandleStore(tmp_path / "candles")


def test_merging_the_same_download_twice_adds_nothing(store):
    bars = [bar(i) for i in range(5)]
    assert store.merge("grvt", bars) == 5
    assert store.merge("grvt", bars) == 0
    assert store.coverage("grvt", SYMBOL, INTERVAL).count == 5


def test_merging_the_same_download_twice_leaves_the_file_identical(store):
    bars = [bar(i) for i in range(5)]
    store.merge("grvt", bars)
    path = store.path_for("grvt", SYMBOL, INTERVAL)
    first = path.read_bytes()
    store.merge("grvt", bars)
    assert path.read_bytes() == first


def test_a_reread_returns_the_exact_decimal_that_was_written(store):
    store.merge("grvt", [bar(0, close="12345.678901234567890")])
    loaded = store.load("grvt", SYMBOL, INTERVAL)
    assert loaded[0].close == D("12345.678901234567890")


def test_bars_come_back_sorted_however_they_went_in(store):
    store.merge("grvt", [bar(3), bar(0), bar(2)])
    times = [candle.open_time for candle in store.load("grvt", SYMBOL, INTERVAL)]
    assert times == sorted(times)


def test_a_redownloaded_bar_replaces_the_one_already_stored(store):
    store.merge("grvt", [bar(0, close="100")])
    store.merge("grvt", [bar(0, close="101")])
    stored = store.load("grvt", SYMBOL, INTERVAL)
    assert len(stored) == 1
    assert stored[0].close == D("101")


def test_a_missing_file_reads_as_no_history(store):
    assert store.load("grvt", "NOPE", INTERVAL) == []
    assert store.coverage("grvt", "NOPE", INTERVAL).is_empty


def test_it_reports_the_gap_when_the_exchange_skipped_a_bar(store):
    store.merge("grvt", [bar(0), bar(1), bar(3), bar(4)])
    coverage = store.coverage("grvt", SYMBOL, INTERVAL)
    assert len(coverage.gaps) == 1
    assert coverage.missing_bars == 1
    assert "gap" in coverage.describe()


def test_a_continuous_series_reports_no_gaps(store):
    store.merge("grvt", [bar(i) for i in range(10)])
    assert store.coverage("grvt", SYMBOL, INTERVAL).gaps == ()


def test_loading_a_range_includes_the_start_and_excludes_the_end(store):
    store.merge("grvt", [bar(i) for i in range(5)])
    loaded = store.load("grvt", SYMBOL, INTERVAL, start=START + STEP, end=START + STEP * 3)
    assert [c.open_time for c in loaded] == [START + STEP, START + STEP * 2]


def test_a_naive_timestamp_is_refused_rather_than_assumed(store):
    naive = Candle(
        symbol=SYMBOL,
        interval=INTERVAL,
        open_time=datetime(2026, 1, 1),
        open=D("1"),
        high=D("1"),
        low=D("1"),
        close=D("1"),
        volume=D("0"),
    )
    with pytest.raises(StoreError, match="timezone"):
        store.merge("grvt", [naive])


def test_two_series_cannot_be_merged_in_one_call(store):
    with pytest.raises(StoreError, match="one series at a time"):
        store.merge("grvt", [bar(0), bar(1, symbol="ETH_USDT_Perp")])


def test_a_file_missing_a_column_is_refused_not_guessed(store):
    path = store.path_for("grvt", SYMBOL, INTERVAL)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("open_time,open,high,low\n2026-01-01T00:00:00+00:00,1,1,1\n", encoding="utf-8")
    with pytest.raises(StoreError, match="missing column"):
        store.load("grvt", SYMBOL, INTERVAL)


def test_a_broken_number_names_the_line_it_is_on(store):
    path = store.path_for("grvt", SYMBOL, INTERVAL)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "open_time,open,high,low,close,volume\n"
        "2026-01-01T00:00:00+00:00,1,1,1,1,1\n"
        "2026-01-01T04:00:00+00:00,1,1,1,oops,1\n",
        encoding="utf-8",
    )
    with pytest.raises(StoreError, match="line 3"):
        store.load("grvt", SYMBOL, INTERVAL)


def test_two_exchanges_keep_separate_history(store):
    store.merge("grvt", [bar(0, close="100")])
    store.merge("hyperliquid", [bar(0, close="200")])
    assert store.load("grvt", SYMBOL, INTERVAL)[0].close == D("100")
    assert store.load("hyperliquid", SYMBOL, INTERVAL)[0].close == D("200")
    assert len(store.series()) == 2


def test_a_symbol_with_slashes_becomes_a_usable_file_name():
    assert safe_name("BTC/USDT:USDT") == "BTC-USDT-USDT"
    assert safe_name("  ") == "unnamed"


def test_gaps_are_found_without_a_store_at_all():
    assert find_gaps([bar(0), bar(1)], INTERVAL) == ()
    assert find_gaps([bar(0), bar(5)], INTERVAL) == ((START, START + STEP * 5),)
