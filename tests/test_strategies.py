"""Strategies are pure, so they are tested directly on synthetic prices."""

import random
from datetime import datetime, timezone

import pytest

from hyperbot.money import D
from hyperbot.strategies import STRATEGIES, UnknownStrategy, build_strategy, describe_strategies
from hyperbot.strategies.base import MarketView
from hyperbot.strategies.ma_cross import MaCrossStrategy, sma
from hyperbot.strategies.random_strategy import RandomStrategy
from hyperbot.types import Position, Side

SYMBOL = "BTC_USDT_Perp"
NOW = datetime(2026, 11, 4, 12, 0, tzinfo=timezone.utc)


def view(price, prices=(), position=None) -> MarketView:
    return MarketView(
        symbol=SYMBOL,
        price=D(price),
        now=NOW,
        equity=D("10000"),
        position=position,
        recent_prices=tuple(D(p) for p in prices),
    )


# --- registry ----------------------------------------------------------------


def test_the_registry_builds_every_listed_strategy():
    for name in STRATEGIES:
        assert build_strategy(name).name


def test_an_unknown_name_lists_the_real_ones():
    with pytest.raises(UnknownStrategy, match="ma_cross"):
        build_strategy("telepathy")


def test_descriptions_are_printable():
    text = describe_strategies()
    assert "random" in text and "ma_cross" in text


# --- sma ---------------------------------------------------------------------


def test_sma_needs_a_full_window():
    assert sma([D(1), D(2)], 3) is None
    assert sma([D(1), D(2), D(3)], 3) == D(2)


def test_sma_uses_only_the_last_window():
    assert sma([D(100), D(1), D(2), D(3)], 3) == D(2)


def test_sma_window_must_be_positive():
    with pytest.raises(ValueError):
        sma([D(1)], 0)


# --- ma cross ----------------------------------------------------------------


def test_fast_must_be_shorter_than_slow():
    with pytest.raises(ValueError, match="shorter"):
        MaCrossStrategy(fast=30, slow=10)


def test_says_nothing_until_it_has_enough_history():
    strategy = MaCrossStrategy(fast=2, slow=4)
    assert strategy.get_signal(view(100, prices=[100, 101, 102])) is None


def test_goes_long_on_a_cross_up():
    strategy = MaCrossStrategy(fast=2, slow=4, stop_pct="1")
    # Flat, then a jump: the fast average crosses above the slow one here
    # and nowhere earlier, which is what "a cross" has to mean.
    prices = [100] * 7 + [130]
    signal = strategy.get_signal(view(130, prices=prices))
    assert signal is not None
    assert signal.side is Side.BUY
    assert signal.stop_price == D(130) * D("0.99")
    assert "crossed above" in signal.reason


def test_goes_short_on_a_cross_down():
    strategy = MaCrossStrategy(fast=2, slow=4, stop_pct="1")
    prices = [100] * 7 + [70]
    signal = strategy.get_signal(view(70, prices=prices))
    assert signal is not None
    assert signal.side is Side.SELL
    assert signal.stop_price == D(70) * D("1.01")


def test_a_long_only_ma_cross_skips_the_short():
    strategy = MaCrossStrategy(fast=2, slow=4, allow_short=False)
    prices = [100] * 7 + [70]
    assert strategy.get_signal(view(70, prices=prices)) is None


def test_no_cross_means_no_signal():
    strategy = MaCrossStrategy(fast=2, slow=4)
    prices = [100, 101, 102, 103, 104, 105, 106, 107]
    assert strategy.get_signal(view(107, prices=prices)) is None


def test_never_signals_while_a_position_is_open():
    strategy = MaCrossStrategy(fast=2, slow=4)
    prices = [100] * 7 + [130]
    held = Position(SYMBOL, Side.BUY, D("0.01"), D(100))
    assert strategy.get_signal(view(130, prices=prices, position=held)) is None


def test_the_stop_is_always_on_the_losing_side_of_entry():
    strategy = MaCrossStrategy(fast=2, slow=4, stop_pct="2.5")
    long_signal = strategy.get_signal(view(130, prices=[100] * 7 + [130]))
    short_signal = strategy.get_signal(view(70, prices=[100] * 7 + [70]))
    assert long_signal.stop_price < long_signal.ref_price
    assert short_signal.stop_price > short_signal.ref_price


# --- random ------------------------------------------------------------------


def test_the_random_strategy_is_reproducible_when_seeded():
    first = RandomStrategy(rng=random.Random(7)).get_signal(view(100))
    second = RandomStrategy(rng=random.Random(7)).get_signal(view(100))
    assert (first is None) == (second is None)
    if first is not None:
        assert first.side == second.side


def test_the_random_strategy_always_attaches_a_stop():
    strategy = RandomStrategy(rng=random.Random(1), signal_chance=0.5, stop_pct="2")
    seen = 0
    for _ in range(50):
        signal = strategy.get_signal(view(100))
        if signal is None:
            continue
        seen += 1
        if signal.side is Side.BUY:
            assert signal.stop_price == D(100) * D("0.98")
        else:
            assert signal.stop_price == D(100) * D("1.02")
    assert seen > 0


def test_a_silly_stop_percentage_is_refused():
    with pytest.raises(ValueError):
        RandomStrategy(stop_pct="0")
    with pytest.raises(ValueError):
        RandomStrategy(stop_pct="150")


def test_the_random_strategy_holds_off_while_in_position():
    strategy = RandomStrategy(rng=random.Random(1), signal_chance=1.0)
    held = Position(SYMBOL, Side.BUY, D("0.01"), D(100))
    assert strategy.get_signal(view(100, position=held)) is None


def test_the_throttle_blocks_a_second_signal_in_the_same_window():
    strategy = RandomStrategy(
        rng=random.Random(1), signal_chance=1.0, min_seconds_between_signals=300
    )
    assert strategy.get_signal(view(100)) is not None
    assert strategy.get_signal(view(100)) is None
