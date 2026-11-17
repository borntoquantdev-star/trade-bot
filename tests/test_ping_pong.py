"""The demo strategy, and the way out of a position it depends on."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
from decimal import Decimal

import pytest

from tradebot.money import D
from tradebot.strategies import build_strategy
from tradebot.strategies.base import MarketView
from tradebot.strategies.ping_pong import PingPongStrategy
from tradebot.types import Position, Side

NOW = datetime(2026, 11, 13, 12, 0, tzinfo=timezone.utc)


def view(
    price: str,
    prices: list[str] | None = None,
    position: Position | None = None,
    at: datetime = NOW,
) -> MarketView:
    return MarketView(
        symbol="BTC_USDT_Perp",
        price=D(price),
        now=at,
        equity=D("10000"),
        position=position,
        recent_prices=tuple(D(p) for p in (prices or [])),
    )


def flat(window: int = 20, price: str = "100") -> list[str]:
    return [price] * window


def long_at(entry: str) -> Position:
    return Position(symbol="BTC_USDT_Perp", side=Side.BUY, qty=D("0.1"), entry_price=D(entry))


def short_at(entry: str) -> Position:
    return Position(symbol="BTC_USDT_Perp", side=Side.SELL, qty=D("0.1"), entry_price=D(entry))


def test_it_is_registered_under_a_name_the_cli_can_take() -> None:
    strategy = build_strategy("ping_pong")
    assert isinstance(strategy, PingPongStrategy)


def test_it_says_nothing_until_it_has_seen_enough_prices() -> None:
    strategy = PingPongStrategy(window=20)
    assert strategy.get_signal(view("100", flat(19))) is None
    assert strategy.average(view("100", flat(19))) is None
    assert strategy.average(view("100", flat(20))) == D("100")


def test_a_price_stretched_below_the_average_is_bought() -> None:
    strategy = PingPongStrategy(window=20, edge_pct="0.05", stop_pct="0.4")
    signal = strategy.get_signal(view("99.9", flat(20)))
    assert signal is not None
    assert signal.side is Side.BUY
    # The stop sits below the entry, by the configured distance.
    assert signal.stop_price == D("99.9") * (1 - D("0.004"))
    assert "average" in signal.reason


def test_a_price_stretched_above_the_average_is_sold() -> None:
    strategy = PingPongStrategy(window=20, edge_pct="0.05", stop_pct="0.4")
    signal = strategy.get_signal(view("100.1", flat(20)))
    assert signal is not None
    assert signal.side is Side.SELL
    assert signal.stop_price > D("100.1")


def test_a_price_sitting_on_its_average_is_left_alone() -> None:
    strategy = PingPongStrategy(window=20, edge_pct="0.05")
    assert strategy.get_signal(view("100", flat(20))) is None
    assert strategy.get_signal(view("100.02", flat(20))) is None


def test_it_does_not_add_to_a_position_it_already_has() -> None:
    strategy = PingPongStrategy(window=20)
    market = view("99.9", flat(20), position=long_at("100"))
    assert strategy.get_signal(market) is None


def test_a_stop_narrower_than_the_entry_move_is_refused_outright() -> None:
    # Otherwise every position opens and is stopped out on the same tick.
    with pytest.raises(ValueError):
        PingPongStrategy(edge_pct="0.5", stop_pct="0.4")


def test_a_long_is_closed_when_the_price_comes_back_to_the_average() -> None:
    strategy = PingPongStrategy(window=20)
    market = view("100", flat(20), position=long_at("99.9"))
    assert strategy.get_exit(market) == "price came back to the average"


def test_a_short_is_closed_when_the_price_comes_back_down() -> None:
    strategy = PingPongStrategy(window=20)
    market = view("100", flat(20), position=short_at("100.1"))
    assert strategy.get_exit(market) == "price came back to the average"


def test_a_long_still_under_water_is_held() -> None:
    strategy = PingPongStrategy(window=20)
    market = view("99.95", flat(20), position=long_at("99.9"))
    assert strategy.get_exit(market) is None


def test_the_stop_is_worked_out_from_the_position_not_from_memory() -> None:
    # A fresh strategy object - as after a restart - still knows where the
    # stop of an open position belongs.
    strategy = PingPongStrategy(window=20, stop_pct="0.4")
    assert strategy.stop_price(Side.BUY, D("100")) == D("99.6")
    assert strategy.stop_price(Side.SELL, D("100")) == D("100.4")

    market = view("99.5", flat(20, "99.0"), position=long_at("100"))
    assert strategy.get_exit(market) is not None
    assert "stop hit" in strategy.get_exit(market)


def test_a_trade_that_never_comes_back_is_let_go_of_eventually() -> None:
    strategy = PingPongStrategy(window=20, max_hold_seconds=180)
    held = long_at("99.90")
    # A long that is still below its average and still above its stop: the
    # one state where the trade is neither winning nor dead.
    waiting = view("99.85", flat(20, "99.90"), held, NOW)
    assert strategy.get_exit(waiting) is None
    # Three minutes of that and it is simply let go.
    later = view("99.85", flat(20, "99.90"), held, NOW + timedelta(seconds=181))
    reason = strategy.get_exit(later)
    assert reason is not None and "held" in reason


def test_the_hold_clock_starts_again_for_the_next_trade() -> None:
    strategy = PingPongStrategy(window=20, max_hold_seconds=180)
    held = long_at("99.90")
    strategy.get_exit(view("99.85", flat(20, "99.90"), held, NOW))
    # Flat again: the clock is forgotten...
    assert strategy.get_exit(view("100", flat(20), None, NOW + timedelta(seconds=200))) is None
    # ...so the next position is not instantly too old to hold.
    assert (
        strategy.get_exit(view("99.85", flat(20, "99.90"), held, NOW + timedelta(seconds=300)))
        is None
    )


def test_the_hold_clock_restarts_the_way_the_engine_calls_it() -> None:
    # The engine never shows get_exit a flat book: while flat it only asks
    # get_signal. The clock still has to be forgotten between trades.
    strategy = PingPongStrategy(window=20, max_hold_seconds=180)
    held = long_at("99.90")
    strategy.get_exit(view("99.85", flat(20, "99.90"), held, NOW))
    strategy.get_signal(view("100", flat(20), None, NOW + timedelta(seconds=200)))
    later = NOW + timedelta(seconds=300)
    assert strategy.get_exit(view("99.85", flat(20, "99.90"), held, later)) is None


def test_an_average_of_zero_cannot_divide_the_strategy_by_zero() -> None:
    strategy = PingPongStrategy(window=2)
    assert strategy.get_signal(view("0", ["0", "0"])) is None


def test_the_reason_it_gives_is_written_for_a_person() -> None:
    strategy = PingPongStrategy(window=20, edge_pct="0.05")
    signal = strategy.get_signal(view("99.8", flat(20)))
    assert signal is not None
    # Something a student can read next to the chart, not a condition number.
    assert "%" in signal.reason and "average" in signal.reason


def test_prices_stay_decimal_all_the_way_through() -> None:
    strategy = PingPongStrategy(window=4)
    middle = strategy.average(view("100", ["100.1", "99.9", "100.2", "99.8"]))
    assert isinstance(middle, Decimal)
    assert middle == D("100")


def test_the_chart_shows_the_average_and_the_band_it_trades_across() -> None:
    strategy = PingPongStrategy(window=20, edge_pct="0.05", stop_pct="0.4")
    drawn = strategy.chart(view("100", flat(20)))
    assert drawn is not None
    assert drawn["average"] == D("100")
    assert drawn["buy_below"] == D("99.95")
    assert drawn["sell_above"] == D("100.05")
    assert drawn["window"] == 20 and drawn["edge_pct"] == D("0.05")
    assert "stop" not in drawn, "no position, no stop to draw"


def test_the_chart_draws_the_stop_of_the_position_it_is_in() -> None:
    strategy = PingPongStrategy(window=20, edge_pct="0.05", stop_pct="0.4")
    drawn = strategy.chart(view("99", flat(20), position=long_at("100")))
    assert drawn is not None
    assert drawn["stop"] == D("99.6")
    drawn = strategy.chart(view("101", flat(20), position=short_at("100")))
    assert drawn is not None
    assert drawn["stop"] == D("100.4")


def test_the_chart_has_nothing_to_draw_before_the_window_fills() -> None:
    assert PingPongStrategy(window=20).chart(view("100", flat(19))) is None


def test_drawing_the_chart_does_not_start_the_holding_clock() -> None:
    strategy = PingPongStrategy(window=20, max_hold_seconds=60)
    strategy.chart(view("100", flat(20), position=long_at("100")))
    assert strategy._opened_at is None
    strategy.get_exit(view("99.9", flat(20, "100"), position=long_at("100")))
    later = NOW + timedelta(seconds=45)
    drawn = strategy.chart(view("99.9", flat(20), position=long_at("100"), at=later))
    assert drawn is not None and drawn["held_s"] == 45
