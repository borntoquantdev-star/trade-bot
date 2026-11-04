"""The GRVT broker, exercised with a stub ccxt-style client.

No network and no SDK. Payload shapes follow the types published in
``grvt-pysdk`` (``Instrument``, ``Ticker``, ``Positions``), so if those change
these tests fail before anything reaches an exchange.
"""

import pytest

from tradebot.brokers.base import BrokerError
from tradebot.brokers.grvt import (
    DEFAULT_MIN_NOTIONAL,
    GrvtBroker,
    numeric_client_order_id,
    raise_for_error,
    read_candles,
    read_equity,
    read_instrument,
    read_order_id,
    read_positions,
    read_price,
)
from tradebot.core.config import load_config
from tradebot.money import D
from tradebot.risk import risk_gate
from tradebot.types import InstrumentSpec, PortfolioState, Side, Signal

SYMBOL = "BTC_USDT_Perp"
FALLBACK = InstrumentSpec.build(SYMBOL, "0.1", "0.001", "500", price_sig_figs=0, min_qty="0.001")

MARKET = {
    "instrument": SYMBOL,
    "instrument_hash": "0xabc",
    "base": "BTC",
    "quote": "USDT",
    "kind": "PERPETUAL",
    "base_decimals": 4,
    "quote_decimals": 2,
    "tick_size": "0.5",
    "min_size": "0.002",
    "max_position_size": "100",
}

POSITIONS = [
    {"instrument": SYMBOL, "size": "0.25", "entry_price": "99000"},
    {"instrument": "ETH_USDT_Perp", "size": "-2", "entry_price": "3000"},
    {"instrument": "SOL_USDT_Perp", "size": "0"},
]


class StubClient:
    def __init__(self, **payloads):
        self.payloads = payloads
        self.calls: list[tuple] = []

    def _answer(self, name, default=None, **kwargs):
        self.calls.append((name, kwargs))
        value = self.payloads.get(name, default)
        if isinstance(value, Exception):
            raise value
        return value

    def fetch_ticker(self, symbol, params={}):  # noqa: B006
        return self._answer("fetch_ticker", {"mark_price": "100000"}, symbol=symbol)

    def fetch_market(self, symbol):
        return self._answer("fetch_market", MARKET, symbol=symbol)

    def fetch_balance(self, type="sub-account"):  # noqa: A002
        return self._answer("fetch_balance", {"total_equity": "25000"})

    def fetch_positions(self, symbols=[], params={}):  # noqa: B006
        return self._answer("fetch_positions", POSITIONS, symbols=symbols)

    def fetch_ohlcv(self, symbol, timeframe="1m", since=0, limit=10, params={}):  # noqa: B006
        return self._answer(
            "fetch_ohlcv",
            [[1762200000000, "99000", "99500", "98900", "99400", "12"]],
            symbol=symbol,
            timeframe=timeframe,
            since=since,
            limit=limit,
        )

    def create_order(self, **kwargs):
        return self._answer("create_order", {"result": {"order_id": "ex-1"}}, **kwargs)

    def cancel_order(self, id=None, symbol=None, params={}):  # noqa: A002,B006
        return self._answer("cancel_order", True, id=id, symbol=symbol, params=params)


def broker(**payloads) -> GrvtBroker:
    config = load_config(
        data={"grvt": {"api_key": "k", "private_key": "p", "trading_account_id": "a"}}, env={}
    )
    instance = GrvtBroker(config.grvt, client=StubClient(**payloads), fallback_instrument=FALLBACK)
    instance.connect()
    return instance


# --- prices ------------------------------------------------------------------


@pytest.mark.parametrize(
    "payload",
    [
        {"mark_price": "100000.5"},
        {"last": "100000.5"},
        {"last_price": "100000.5"},
        {"result": {"last_price": "100000.5"}},
    ],
)
def test_reads_a_price_from_every_known_shape(payload):
    assert read_price(payload, SYMBOL) == D("100000.5")


def test_a_price_of_zero_is_refused():
    with pytest.raises(BrokerError):
        read_price({"mark_price": "0"}, SYMBOL)


def test_a_raw_scaled_price_is_caught_instead_of_used():
    """The raw API scales prices by 1e9 - using that would size orders wildly."""
    with pytest.raises(BrokerError, match="1e9"):
        read_price({"mark_price": "100000000000000"}, SYMBOL)


def test_an_unknown_ticker_shape_is_refused():
    with pytest.raises(BrokerError, match="no usable price"):
        read_price({"something": 1}, SYMBOL)


def test_the_broker_fetches_a_price():
    assert broker().fetch_mark_price(SYMBOL) == D("100000")


# --- instruments --------------------------------------------------------------


def test_instrument_precision_comes_from_the_market_reply():
    spec = read_instrument(MARKET, SYMBOL, FALLBACK)
    assert spec.tick_size == D("0.5")
    assert spec.lot_step == D("0.0001"), "lot step = 10 ** -base_decimals"
    assert spec.min_qty == D("0.002"), "GRVT publishes min_size in the base asset"
    assert spec.price_sig_figs == 0, "no significant-figure rule on GRVT"
    assert spec.min_notional == FALLBACK.min_notional, "GRVT does not publish a minimum value"


def test_missing_instrument_fields_fall_back():
    spec = read_instrument({"tick_size": "0.5"}, SYMBOL, FALLBACK)
    assert spec.tick_size == D("0.5")
    assert spec.lot_step == FALLBACK.lot_step
    assert spec.min_qty == FALLBACK.min_qty


def test_an_error_reply_falls_back_entirely():
    assert read_instrument({"code": 500}, SYMBOL, FALLBACK) == FALLBACK


def test_the_broker_caches_the_instrument():
    b = broker()
    spec = b.fetch_instrument(SYMBOL)
    assert b.fetch_instrument(SYMBOL) is spec
    assert spec.lot_step == D("0.0001")


def test_a_broker_without_any_instrument_source_says_so():
    config = load_config(env={})
    bare = GrvtBroker(config.grvt, client=StubClient())
    with pytest.raises(BrokerError, match="instrument"):
        bare.fetch_instrument(SYMBOL)


def test_the_default_min_notional_is_documented():
    assert D(500) == DEFAULT_MIN_NOTIONAL


# --- account -----------------------------------------------------------------


@pytest.mark.parametrize(
    "payload",
    [{"total_equity": "25000"}, {"result": {"equity": "25000"}}, {"total": {"USDT": "25000"}}],
)
def test_reads_equity(payload):
    assert read_equity(payload) == D(25000)


def test_an_unreadable_balance_is_an_error():
    with pytest.raises(BrokerError):
        read_equity({"nothing": True})


def test_reads_positions_and_drops_flat_rows():
    positions = read_positions(POSITIONS)
    assert set(positions) == {SYMBOL, "ETH_USDT_Perp"}
    assert positions[SYMBOL].side is Side.BUY
    assert positions[SYMBOL].qty == D("0.25")
    assert positions["ETH_USDT_Perp"].side is Side.SELL, "size ติดลบ = short"
    assert positions["ETH_USDT_Perp"].qty == D(2)


def test_the_broker_filters_positions_by_symbol():
    b = broker()
    assert set(b.fetch_positions(SYMBOL)) == {SYMBOL}
    assert set(b.fetch_positions()) == {SYMBOL, "ETH_USDT_Perp"}


# --- candles ------------------------------------------------------------------


def test_reads_ccxt_style_candles():
    rows = [
        [1762200300000, "99400", "99900", "99300", "99800", "9"],
        [1762200000000, "99000", "99500", "98900", "99400", "12"],
    ]
    candles = read_candles(rows, SYMBOL, "5m")
    assert [c.close for c in candles] == [D("99400"), D("99800")], "เรียงเก่าไปใหม่"
    assert candles[0].high == D("99500")


def test_reads_mapping_style_candles():
    rows = [
        {"open_time": 1762200000000, "open": 1, "high": 2, "low": 0.5, "close": 1.5, "volume": 3}
    ]
    assert read_candles(rows, SYMBOL, "5m")[0].close == D("1.5")


def test_nanosecond_timestamps_are_handled():
    rows = [[1762200000000000000, "1", "2", "0.5", "1.5", "3"]]
    assert read_candles(rows, SYMBOL, "5m")[0].open_time.year == 2025


def test_an_unreadable_candle_is_an_error():
    with pytest.raises(BrokerError, match="unreadable candle"):
        read_candles([["soon", "1", "2", "3", "4"]], SYMBOL, "5m")


def test_no_candles_is_an_empty_list():
    assert read_candles(None, SYMBOL, "5m") == []
    assert read_candles([], SYMBOL, "5m") == []


def test_the_broker_asks_for_the_window_it_wants():
    b = broker()
    b.fetch_candles(SYMBOL, "5m", 50)
    _, kwargs = next(c for c in b.client.calls if c[0] == "fetch_ohlcv")
    assert kwargs["timeframe"] == "5m"
    assert kwargs["limit"] == 51


def test_an_unknown_interval_is_refused():
    with pytest.raises(BrokerError, match="interval"):
        broker().fetch_candles(SYMBOL, "7m", 10)


# --- client order ids ---------------------------------------------------------


def test_the_hex_id_folds_to_a_stable_uint32():
    assert numeric_client_order_id("89c7ffc74d14687c635c3ab535fbec15") == 0x89C7FFC7
    assert numeric_client_order_id("0x89c7ffc74d14687c635c3ab535fbec15") == 0x89C7FFC7


def test_the_same_signal_keeps_the_same_numeric_id():
    a = numeric_client_order_id("a" * 32)
    b = numeric_client_order_id("a" * 32)
    assert a == b
    assert 0 <= a <= 0xFFFFFFFF


def test_a_non_hex_id_is_an_error():
    with pytest.raises(BrokerError, match="hexadecimal"):
        numeric_client_order_id("not-hex!")


# --- placing orders -----------------------------------------------------------


def approved_order():
    config = load_config(env={})
    spec = read_instrument(MARKET, SYMBOL, FALLBACK)
    signal = Signal.build(SYMBOL, "buy", "100000", "99000", reason="unit test")
    state = PortfolioState(equity=D("50000"), peak_equity=D("50000"))
    from tradebot.core.engine import utc_now

    decision = risk_gate(signal, state, config.risk, spec, D("100000"), utc_now())
    assert not hasattr(decision, "code"), decision
    return decision


def test_orders_go_out_as_limit_orders_with_a_numeric_client_id():
    order = approved_order()
    b = broker()
    receipt = b.place_order(order)
    _, kwargs = b.client.calls[-1]
    assert kwargs["order_type"] == "limit"
    assert kwargs["side"] == "buy"
    assert kwargs["params"]["client_order_id"] == numeric_client_order_id(order.client_order_id)
    assert kwargs["params"]["reduce_only"] is False
    assert receipt.broker_order_id == "ex-1"
    assert receipt.simulated is False


@pytest.mark.parametrize(
    "payload",
    [
        {"result": {"order_id": "abc"}},
        {"order_id": "abc"},
        {"metadata": {"client_order_id": "abc"}},
    ],
)
def test_reads_an_order_id(payload):
    assert read_order_id(payload) == "abc"


def test_a_reply_without_an_id_says_to_reconcile():
    with pytest.raises(BrokerError, match="reconcile"):
        read_order_id({"result": {}})


def test_an_error_code_is_not_swallowed():
    with pytest.raises(BrokerError, match="3001"):
        read_order_id({"code": 3001, "message": "rejected", "result": {"order_id": "x"}})


@pytest.mark.parametrize(
    "payload",
    [
        {"code": 3001, "message": "insufficient margin"},
        {"error": "bad signature"},
        {"status": 400, "message": "bad request"},
    ],
)
def test_every_shape_of_failure_is_an_error(payload):
    with pytest.raises(BrokerError):
        raise_for_error(payload, "thing")


def test_a_rejected_order_raises():
    with pytest.raises(BrokerError, match="insufficient"):
        broker(create_order={"code": 3001, "message": "insufficient margin"}).place_order(
            approved_order()
        )


# --- closing, cancelling, leverage --------------------------------------------


def test_closing_uses_a_reduce_only_market_order():
    b = broker()
    receipt = b.close_position(SYMBOL)  # held long 0.25
    _, kwargs = b.client.calls[-1]
    assert kwargs["order_type"] == "market"
    assert kwargs["side"] == "sell"
    assert kwargs["params"]["reduce_only"] is True
    assert receipt.qty == D("0.25")


def test_closing_a_short_buys_back():
    b = broker()
    b.close_position("ETH_USDT_Perp")
    assert b.client.calls[-1][1]["side"] == "buy"


def test_closing_nothing_is_not_an_error():
    assert broker(fetch_positions=[]).close_position(SYMBOL) is None


def test_cancelling_sends_the_numeric_id():
    b = broker()
    b.cancel_order(SYMBOL, "89c7ffc74d14687c635c3ab535fbec15")
    _, kwargs = b.client.calls[-1]
    assert kwargs["params"]["client_order_id"] == 0x89C7FFC7


def test_set_leverage_says_it_cannot_and_why():
    with pytest.raises(BrokerError, match="web interface"):
        broker().set_leverage(SYMBOL, D(5))


def test_an_unconnected_broker_refuses_to_work():
    config = load_config(env={})
    b = GrvtBroker(config.grvt, fallback_instrument=FALLBACK)
    with pytest.raises(BrokerError, match="not connected"):
        b.fetch_equity()


# --- the factory ---------------------------------------------------------------


def test_the_factory_picks_the_broker_for_the_configured_exchange():
    from tradebot.brokers import build_live_broker

    grvt = build_live_broker(load_config(env={}))
    assert grvt.name == "grvt"

    hl = build_live_broker(load_config(data={"exchange": "hyperliquid"}, env={}))
    assert hl.name == "hyperliquid"
