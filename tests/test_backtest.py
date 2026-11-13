"""The replay loop, and the four ways a backtest usually lies.

Each of these is a decision that makes the result worse and truer: acting one
bar later, refusing to fill an order the market never reached, taking the stop
when a bar could have hit either, and honouring a gap.  They are tested
because they are exactly the ones that get quietly "fixed" into a better
looking chart.
"""

from __future__ import annotations

import json
from collections import Counter
from datetime import datetime, timedelta, timezone

import pytest

from tradebot.backtest import run_backtest, summary
from tradebot.backtest.report import THIN_SAMPLE, to_dict
from tradebot.backtest.result import BacktestResult
from tradebot.backtest.runner import BacktestError, exit_on_bar, fill_price
from tradebot.core.config import load_config
from tradebot.money import D
from tradebot.strategies.base import MarketView, Strategy
from tradebot.types import Candle, Side, Signal, SizedOrder

SYMBOL = "BTC_USDT_Perp"
INTERVAL = "1h"
STEP = timedelta(hours=1)
GENESIS = datetime(2026, 1, 1, tzinfo=timezone.utc)


def config_for(**risk):
    """A config with an instrument whose rounding will not get in the way."""
    data = {
        "exchange": "grvt",
        "trading": {"symbol": SYMBOL, "timeframe": INTERVAL, "warmup_candles": 5},
        "risk": {
            "risk_per_trade_pct": 1.0,
            "max_slippage_pct": 1.0,
            "fee_rate_pct": 0.0,
            "starting_equity_usdt": 10000,
            **risk,
        },
        "instrument": {
            "symbol": SYMBOL,
            "tick_size": 0.01,
            "lot_step": 0.0001,
            "min_notional_usdt": 1,
            "min_qty": 0,
            "max_leverage": 20,
            "price_sig_figs": 0,
        },
    }
    return load_config(data=data)


def candles(rows, symbol: str = SYMBOL):
    """Build bars from (open, high, low, close) tuples."""
    return [
        Candle(
            symbol=symbol,
            interval=INTERVAL,
            open_time=GENESIS + STEP * i,
            open=D(str(o)),
            high=D(str(h)),
            low=D(str(low)),
            close=D(str(c)),
            volume=D("1"),
        )
        for i, (o, h, low, c) in enumerate(rows)
    ]


def flat(count: int, price: str = "100") -> list:
    return candles([(price, price, price, price)] * count)


class OnceAt(Strategy):
    """Fires one signal on the bar at *index*, then never again."""

    def __init__(self, index: int, side: str = "buy", stop: str = "99", take_profit=None):
        super().__init__()
        self.index = index
        self.side = side
        self.stop = stop
        self.take_profit = take_profit
        self.seen = 0

    def get_signal(self, market: MarketView) -> Signal | None:
        self.seen += 1
        if self.seen - 1 != self.index:
            return None
        return Signal.build(
            symbol=market.symbol,
            side=self.side,
            ref_price=market.price,
            stop_price=self.stop,
            take_profit=self.take_profit,
            reason="scripted",
            created_at=market.now,
        )


class EveryBar(Strategy):
    """Asks to buy on every bar it is flat, to exercise the gate."""

    def __init__(self, stop: str = "99"):
        super().__init__()
        self.stop = stop

    def get_signal(self, market: MarketView) -> Signal | None:
        return Signal.build(
            symbol=market.symbol,
            side="buy",
            ref_price=market.price,
            stop_price=self.stop,
            reason="always",
            created_at=market.now,
        )


# --- the fill model, on its own ---------------------------------------------


def order(side: str = "buy", limit: str = "100") -> SizedOrder:
    return SizedOrder(
        client_order_id="test",
        symbol=SYMBOL,
        side=Side.parse(side),
        qty=D("1"),
        limit_price=D(limit),
        stop_price=D("90"),
        take_profit=None,
        risk_amount=D("10"),
        reason="",
    )


def test_a_buy_fills_at_the_open_when_the_market_gaps_below_the_limit():
    bar = candles([("95", "96", "94", "95")])[0]
    assert fill_price(order("buy", "100"), bar) == D("95")


def test_a_buy_fills_at_its_limit_when_the_bar_trades_down_to_it():
    bar = candles([("105", "106", "99", "104")])[0]
    assert fill_price(order("buy", "100"), bar) == D("100")


def test_an_order_whose_limit_is_never_touched_does_not_fill():
    bar = candles([("105", "106", "101", "104")])[0]
    assert fill_price(order("buy", "100"), bar) is None


def test_a_bar_holding_both_the_stop_and_the_target_exits_at_the_stop():
    bar = candles([("100", "110", "90", "105")])[0]
    assert exit_on_bar(Side.BUY, D("95"), D("108"), bar) == (D("95"), "stop")


def test_a_gap_straight_through_the_stop_exits_at_the_open():
    bar = candles([("80", "85", "79", "84")])[0]
    assert exit_on_bar(Side.BUY, D("95"), None, bar) == (D("80"), "stop (gap)")


def test_a_target_is_taken_when_only_the_target_is_reachable():
    bar = candles([("100", "110", "99", "105")])[0]
    assert exit_on_bar(Side.BUY, D("95"), D("108"), bar) == (D("108"), "target")


def test_a_short_stops_out_on_the_way_up():
    bar = candles([("100", "110", "99", "105")])[0]
    assert exit_on_bar(Side.SELL, D("105"), None, bar) == (D("105"), "stop")


# --- the replay --------------------------------------------------------------


def test_a_signal_is_filled_on_the_bar_after_the_one_that_produced_it():
    rows = [("100", "100", "100", "100")] * 5
    rows[3] = ("50", "50", "50", "50")  # the bar after the signal
    result = run_backtest(candles(rows), OnceAt(2, stop="40"), config_for())
    assert len(result.trades) == 1
    assert result.trades[0].entry_at == GENESIS + STEP * 3
    assert result.trades[0].entry_price == D("50")


def test_an_approved_order_the_market_never_reached_is_counted_not_filled():
    # Signal at bar 2 with the price at 100; every later bar trades far above
    # the limit, so the buy never fills.
    rows = [("100", "100", "100", "100")] * 3 + [("200", "210", "199", "205")] * 3
    result = run_backtest(candles(rows), OnceAt(2, stop="90"), config_for())
    assert result.trades == []
    assert result.unfilled_orders == 1
    assert "never came back to the limit" in result.never_traded_because()


def test_nothing_is_opened_on_the_final_bar_because_nothing_could_fill_it():
    result = run_backtest(flat(4), OnceAt(3, stop="90"), config_for())
    assert result.signals == 0  # the strategy is not even asked on the last bar


def test_a_position_still_open_at_the_end_is_closed_at_the_last_close():
    rows = [("100", "100", "100", "100")] * 6
    rows[5] = ("100", "100", "100", "123")
    result = run_backtest(candles(rows), OnceAt(2, stop="90"), config_for())
    assert len(result.trades) == 1
    assert result.trades[0].exit_reason == "still open at the end"
    assert result.trades[0].exit_price == D("123")


def test_fees_are_charged_on_the_way_in_and_on_the_way_out():
    rows = [("100", "100", "100", "100")] * 6
    free = run_backtest(candles(rows), OnceAt(2, stop="90"), config_for(fee_rate_pct=0.0))
    charged = run_backtest(candles(rows), OnceAt(2, stop="90"), config_for(fee_rate_pct=0.05))
    assert free.fees_paid == 0
    assert charged.fees_paid > 0
    # Entry and exit both at 100, so the whole loss is the two fees.
    qty = charged.trades[0].qty
    assert charged.fees_paid == qty * D("100") * D("0.0005") * 2
    assert charged.net_pnl == -charged.fees_paid


def test_the_result_counts_what_the_risk_gate_refused():
    # A stop above the entry price is never valid for a long.
    result = run_backtest(flat(6), EveryBar(stop="101"), config_for())
    assert result.trades == []
    assert result.rejects["invalid_stop"] > 0
    assert "invalid_stop" in result.never_traded_because()


def test_it_stops_where_the_live_engine_would_halt():
    # A long bought near 100 and a market that collapses: the drawdown stop
    # fires, and the live engine would halt rather than keep trading.
    rows = [("100", "100", "100", "100")] * 3
    rows += [("100", "100", "70", "70")]
    rows += [("70", "70", "70", "70")] * 4
    result = run_backtest(
        candles(rows),
        EveryBar(stop="50"),
        config_for(max_drawdown_pct=5.0, risk_per_trade_pct=20.0),
    )
    assert result.was_halted
    assert "max_drawdown" in result.halt_reason
    assert "HALTED" in summary(result)


def test_the_run_continues_past_a_limit_when_halting_is_switched_off():
    rows = [("100", "100", "100", "100")] * 3
    rows += [("100", "100", "70", "70")]
    rows += [("70", "70", "70", "70")] * 4
    result = run_backtest(
        candles(rows),
        EveryBar(stop="50"),
        config_for(max_drawdown_pct=5.0, risk_per_trade_pct=20.0),
        halt_on_limits=False,
    )
    assert not result.was_halted


def test_the_r_multiple_is_the_result_divided_by_the_risk_taken():
    # Risk 1% of 10,000 = 100 with the stop 10 away, so 10 units. The bar
    # after the signal opens at 100 (filled), then the market runs to 120:
    # 200 of profit on 100 of risk is 2R.
    rows = [("100", "100", "100", "100")] * 3
    rows += [("100", "120", "100", "120")]
    rows += [("120", "120", "120", "120")] * 2
    result = run_backtest(candles(rows), OnceAt(2, stop="90"), config_for())
    trade = result.trades[0]
    assert trade.risk_amount > 0
    assert trade.r_multiple == trade.net_pnl / trade.risk_amount
    assert trade.r_multiple > 1


def test_a_strategy_that_raises_does_not_kill_the_run():
    class Broken(Strategy):
        def get_signal(self, market):
            raise RuntimeError("bad indicator")

    result = run_backtest(flat(5), Broken(), config_for())
    assert result.trades == []
    assert any("bad indicator" in w for w in result.warnings)


def test_a_wall_clock_signal_is_restamped_and_the_user_is_told():
    class WallClock(Strategy):
        def get_signal(self, market):
            if market.in_position:
                return None
            return Signal.build(
                symbol=market.symbol,
                side="buy",
                ref_price=market.price,
                stop_price="90",
                reason="no created_at",
            )

    result = run_backtest(flat(6), WallClock(), config_for())
    assert any("wall clock" in w for w in result.warnings)


def test_the_same_run_twice_produces_the_same_numbers():
    rows = [("100", "101", "99", "100")] * 8
    first = run_backtest(candles(rows), OnceAt(2, stop="90"), config_for())
    second = run_backtest(candles(rows), OnceAt(2, stop="90"), config_for())
    assert first.net_pnl == second.net_pnl
    assert [t.entry_price for t in first.trades] == [t.entry_price for t in second.trades]


def test_candles_for_the_wrong_symbol_are_refused_rather_than_mis_rounded():
    with pytest.raises(BacktestError, match="the candles are"):
        run_backtest(
            candles([("1", "1", "1", "1")], symbol="ETH_USDT_Perp"), OnceAt(0), config_for()
        )


def test_an_empty_run_says_how_to_get_candles():
    with pytest.raises(BacktestError, match="fetch-candles"):
        run_backtest([], OnceAt(0), config_for())


def test_mixed_intervals_are_refused():
    bars = flat(3)
    bad = Candle(
        symbol=SYMBOL,
        interval="4h",
        open_time=GENESIS + STEP * 3,
        open=D("1"),
        high=D("1"),
        low=D("1"),
        close=D("1"),
        volume=D("0"),
    )
    with pytest.raises(BacktestError, match="mix intervals"):
        run_backtest([*bars, bad], OnceAt(0), config_for())


def test_a_run_with_no_signals_says_so_plainly():
    result = run_backtest(flat(5), OnceAt(99), config_for())
    assert result.never_traded_because() == "the strategy never produced a signal on this data"


def test_the_equity_curve_has_one_point_per_bar():
    result = run_backtest(flat(7), OnceAt(2, stop="90"), config_for())
    assert len(result.equity_curve) == 7


def test_time_in_market_counts_the_bars_a_position_was_held():
    rows = [("100", "100", "100", "100")] * 6
    result = run_backtest(candles(rows), OnceAt(2, stop="90"), config_for())
    assert 0 < result.exposure_pct < 100


# --- the statistics the report promises -------------------------------------


def curve(values, interval: str = "1d"):
    """A result with a hand-made equity curve and nothing else."""
    from tradebot.backtest.result import BacktestResult, EquityPoint

    step = timedelta(days=1) if interval == "1d" else STEP
    result = BacktestResult(
        symbol=SYMBOL,
        interval=interval,
        strategy="hand",
        starting_equity=D(str(values[0])),
    )
    result.equity_curve = [
        EquityPoint(at=GENESIS + step * i, equity=D(str(v))) for i, v in enumerate(values)
    ]
    result.start = result.equity_curve[0].at
    result.end = result.equity_curve[-1].at
    return result


def test_an_account_that_doubled_in_a_year_reports_about_100_percent_a_year():
    result = curve([10_000] * 364 + [20_000])
    assert 95 < result.cagr_pct < 105


def test_a_test_shorter_than_a_month_refuses_to_annualise():
    assert curve([10_000, 11_000]).cagr_pct is None


def test_time_underwater_counts_the_bars_below_the_previous_peak():
    result = curve([100, 120, 110, 105, 115, 130])
    assert result.bars_underwater == 3
    assert result.days_underwater == D(3)


def test_an_account_that_only_rises_is_never_underwater():
    assert curve([100, 110, 120]).bars_underwater == 0


def test_the_worst_month_is_named_and_measured():
    from tradebot.backtest.result import EquityPoint

    result = curve([100])
    result.equity_curve = [
        EquityPoint(at=datetime(2026, 1, 1, tzinfo=timezone.utc), equity=D("100")),
        EquityPoint(at=datetime(2026, 1, 28, tzinfo=timezone.utc), equity=D("110")),
        EquityPoint(at=datetime(2026, 2, 1, tzinfo=timezone.utc), equity=D("110")),
        EquityPoint(at=datetime(2026, 2, 27, tzinfo=timezone.utc), equity=D("88")),
    ]
    month, change = result.worst_month
    assert month == "2026-02"
    assert change == D(-20)


def test_buy_and_hold_is_measured_from_the_candles_themselves():
    rows = [("100", "100", "100", "100")] * 4 + [("150", "150", "150", "150")] * 2
    result = run_backtest(candles(rows), OnceAt(99), config_for())
    assert result.first_price == D("100")
    assert result.last_price == D("150")
    assert result.buy_and_hold_pct == D(50)


def test_sharpe_is_none_when_the_account_never_moved():
    assert curve([10_000] * 50).sharpe() is None


def test_sharpe_rises_when_the_same_return_arrives_more_smoothly():
    steady = curve([100, 101, 102, 103, 104, 105, 106])
    jagged = curve([100, 112, 96, 118, 92, 120, 106])
    assert steady.sharpe() > jagged.sharpe()


def test_sortino_ignores_the_volatility_that_paid():
    result = curve([100, 130, 128, 160, 158, 200])
    assert result.sortino() > result.sharpe()


def test_dropping_the_two_best_trades_exposes_a_result_built_on_luck():
    rows = [("100", "100", "100", "100")] * 6
    result = run_backtest(candles(rows), OnceAt(2, stop="90"), config_for())
    result.trades.extend(result.trades)  # two identical round trips
    assert result.profit_without_best(2) == 0


def test_the_win_loss_ratio_is_none_when_nothing_was_lost():
    rows = [("100", "100", "100", "100")] * 3
    rows += [("100", "120", "100", "120")]
    rows += [("120", "120", "120", "120")] * 2
    result = run_backtest(candles(rows), OnceAt(2, stop="90"), config_for())
    assert result.losses == []
    assert result.win_loss_ratio is None


# --- the JSON the dashboard reads --------------------------------------------
#
# The dashboard words the same facts in Thai, so what it needs from here is
# codes and thresholds, not sentences.  These tests pin the parts of the shape
# a second reader depends on.


def _empty_result(**fields) -> BacktestResult:
    """A result that opened nothing, set up to hit one dead end."""
    base = dict(symbol=SYMBOL, interval=INTERVAL, strategy="T", bars=10, signals=1)
    base.update(fields)
    return BacktestResult(**base)


def test_the_no_trade_reason_names_each_dead_end():
    assert _empty_result(bars=0, signals=0).no_trade_reason == "no_candles"
    assert _empty_result(signals=0).no_trade_reason == "no_signal"
    assert _empty_result(rejects=Counter({"invalid_stop": 3})).no_trade_reason == "all_refused"
    assert _empty_result(unfilled_orders=2).no_trade_reason == "never_filled"
    assert _empty_result().no_trade_reason == "unrecorded"


def test_a_run_that_traded_has_no_dead_end_to_explain():
    result = run_backtest(flat(6), OnceAt(2, stop="90"), config_for())
    assert result.trades
    assert result.no_trade_reason == ""
    assert result.never_traded_because() == ""


def test_the_english_sentence_is_a_wording_of_the_reason_code():
    # One ladder of conditions, two readers.  If these ever disagree, the
    # dashboard and the console are describing different runs.
    for result in (
        _empty_result(bars=0, signals=0),
        _empty_result(signals=0),
        _empty_result(rejects=Counter({"slippage_exceeded": 4})),
        _empty_result(unfilled_orders=2),
        _empty_result(),
    ):
        assert result.no_trade_reason != ""
        assert result.never_traded_because() != ""


def test_the_refusal_sentence_names_the_reason_that_came_up_most():
    result = _empty_result(rejects=Counter({"invalid_stop": 1, "slippage_exceeded": 9}))
    assert result.no_trade_reason == "all_refused"
    assert "slippage_exceeded (9 times)" in result.never_traded_because()


def test_the_json_view_carries_the_reason_code_a_second_reader_needs():
    result = run_backtest(flat(5), OnceAt(99), config_for())
    assert to_dict(result)["no_trade_reason"] == "no_signal"


def test_the_json_view_carries_the_thin_sample_threshold_rather_than_the_warning():
    # The dashboard warns in Thai, so it needs the number, not the sentence.
    # Keeping a second copy of 30 in JavaScript is how the two drift apart.
    view = to_dict(run_backtest(flat(6), OnceAt(2, stop="90"), config_for()))
    assert view["thin_sample_below"] == THIN_SAMPLE
    assert view["trades"] < view["thin_sample_below"]


def test_the_json_view_keeps_numbers_as_strings_so_a_browser_cannot_round_them():
    view = to_dict(run_backtest(flat(6), OnceAt(2, stop="90"), config_for(fee_rate_pct=0.05)))
    for key in ("starting_equity", "final_equity", "net_pnl", "return_pct", "fees_paid"):
        assert isinstance(view[key], str), key
    assert isinstance(view["equity_curve"][0]["equity"], str)
    assert isinstance(view["trade_list"][0]["net_pnl"], str)


def test_the_json_view_survives_being_written_and_read_back():
    result = run_backtest(flat(6), OnceAt(2, stop="90"), config_for())
    reloaded = json.loads(json.dumps(to_dict(result)))
    assert reloaded["bars"] == result.bars
    assert len(reloaded["equity_curve"]) == len(result.equity_curve)
    assert len(reloaded["trade_list"]) == len(result.trades)


def test_what_cannot_be_measured_is_null_in_the_json_not_zero():
    # A browser that reads 0 here would print "sharpe 0.00", which is a
    # measurement.  There was no measurement.
    view = to_dict(_empty_result())
    assert view["sharpe"] is None
    assert view["sortino"] is None
    assert view["profit_factor"] is None
    assert view["cagr_pct"] is None
    assert view["buy_and_hold_pct"] is None
    assert view["worst_month"] is None


def test_a_strategy_that_wants_out_is_let_out_by_the_replay_too() -> None:
    """The live loop honours `get_exit`; a replay that ignored it would be
    testing a different bot from the one that trades."""

    class LeavesNextBar(Strategy):
        description = "buys once, then asks to leave"

        def __init__(self) -> None:
            super().__init__()
            self.opened = False

        def get_signal(self, market: MarketView):
            if self.opened or market.in_position:
                return None
            self.opened = True
            return Signal.build(
                symbol=market.symbol,
                side="buy",
                ref_price=market.price,
                stop_price=market.price * D("0.5"),
                reason="one trade, then out",
                created_at=market.now,
            )

        def get_exit(self, market: MarketView) -> str | None:
            return "the strategy asked"

    # A flat market: no stop and no target would ever fire here, so if the
    # trade closes at all it is because the strategy asked.
    bars = flat(10, price="100")
    result = run_backtest(bars, LeavesNextBar(), config_for())

    assert len(result.trades) == 1
    assert result.trades[0].exit_reason == "the strategy asked"
