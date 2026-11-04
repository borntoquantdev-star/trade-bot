"""The Hyperliquid broker, exercised with stub Info and Exchange objects.

No network and no SDK. The payloads below are shaped like the ones in the
official API docs, so the reply readers are pinned to the documented format
rather than to whatever happened to work once.
"""

from decimal import Decimal

import pytest

from tradebot.brokers.base import BrokerError
from tradebot.brokers.hyperliquid import (
    DEFAULT_MIN_NOTIONAL,
    HyperliquidBroker,
    instrument_from_universe,
    read_candles,
    read_equity,
    read_mark_price,
    read_mid_price,
    read_order_result,
    read_positions,
    read_universe,
)
from tradebot.core.config import load_config
from tradebot.money import D
from tradebot.risk import risk_gate
from tradebot.types import PortfolioState, Side, Signal

ADDRESS = "0x1234567890abcdef1234567890abcdef12345678"

META = {
    "universe": [
        {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25},
        {"name": "SOL", "szDecimals": 2, "maxLeverage": 20},
    ],
    "marginTables": [],
}

CTXS = [{"markPx": "100000.0"}, {"markPx": "3000.5"}, {"markPx": "150.25"}]

USER_STATE = {
    "marginSummary": {"accountValue": "13109.482328"},
    "assetPositions": [
        {
            "type": "oneWay",
            "position": {
                "coin": "ETH",
                "entryPx": "2986.3",
                "szi": "0.0335",
                "unrealizedPnl": "-0.0134",
            },
        },
        {
            "type": "oneWay",
            "position": {"coin": "SOL", "entryPx": "150.0", "szi": "-3.5"},
        },
        {"type": "oneWay", "position": {"coin": "BTC", "entryPx": "0", "szi": "0.0"}},
    ],
}

CANDLES = [
    {
        "t": 1762200000000,
        "s": "BTC",
        "i": "5m",
        "o": "99000",
        "h": "99500",
        "l": "98900",
        "c": "99400",
        "v": "12",
    },
    {
        "t": 1762200300000,
        "s": "BTC",
        "i": "5m",
        "o": "99400",
        "h": "99900",
        "l": "99300",
        "c": "99800",
        "v": "9",
    },
]


class StubInfo:
    def __init__(self, **payloads):
        self.payloads = payloads
        self.calls: list[tuple] = []

    def _answer(self, name, *args):
        self.calls.append((name, args))
        value = self.payloads.get(name, {})
        if isinstance(value, Exception):
            raise value
        return value

    def meta(self, dex=""):
        return self.payloads.get("meta", META)

    def meta_and_asset_ctxs(self):
        return self._answer("meta_and_asset_ctxs") or [META, CTXS]

    def all_mids(self, dex=""):
        return self._answer("all_mids")

    def user_state(self, address, dex=""):
        self.calls.append(("user_state", (address,)))
        return self.payloads.get("user_state", USER_STATE)

    def candles_snapshot(self, name, interval, start, end):
        self.calls.append(("candles_snapshot", (name, interval, start, end)))
        return self.payloads.get("candles_snapshot", CANDLES)


class StubCloid:
    def __init__(self, raw):
        self.raw = raw

    @classmethod
    def from_str(cls, raw):
        if not raw.startswith("0x") or len(raw) - 2 != 32:
            raise TypeError("cloid is not 16 bytes")
        return cls(raw)

    def to_raw(self):
        return self.raw


class StubExchange:
    def __init__(self, reply=None):
        self.reply = reply or {
            "status": "ok",
            "response": {"type": "order", "data": {"statuses": [{"resting": {"oid": 77}}]}},
        }
        self.orders: list[dict] = []
        self.cancels: list[tuple] = []
        self.leverage: tuple | None = None

    def order(self, name, is_buy, sz, limit_px, order_type, reduce_only=False, cloid=None):
        self.orders.append(
            {
                "name": name,
                "is_buy": is_buy,
                "sz": sz,
                "limit_px": limit_px,
                "order_type": order_type,
                "reduce_only": reduce_only,
                "cloid": cloid,
            }
        )
        if isinstance(self.reply, Exception):
            raise self.reply
        return self.reply

    def cancel_by_cloid(self, name, cloid):
        self.cancels.append((name, cloid.to_raw()))
        return {"status": "ok"}

    def update_leverage(self, leverage, name, is_cross=True):
        self.leverage = (leverage, name, is_cross)
        return {"status": "ok"}


def broker(info=None, exchange=None, **config_patch) -> HyperliquidBroker:
    data = {
        "exchange": "hyperliquid",
        "hyperliquid": {"account_address": ADDRESS, "api_wallet_key": "0xkey"},
        "trading": {"symbol": "BTC"},
    }
    data.update(config_patch)
    config = load_config(data=data, env={})
    instance = HyperliquidBroker(
        config.hyperliquid,
        info=info or StubInfo(),
        exchange=exchange if exchange is not None else StubExchange(),
    )
    instance._cloid_factory = StubCloid
    instance.connect()
    return instance


# --- universe and precision -------------------------------------------------


def test_reads_the_universe_and_keeps_the_asset_index():
    universe = read_universe(META)
    assert universe["BTC"]["asset"] == 0
    assert universe["ETH"]["asset"] == 1
    assert universe["BTC"]["sz_decimals"] == 5
    assert universe["ETH"]["max_leverage"] == D(25)


@pytest.mark.parametrize(
    ("symbol", "sz_decimals", "lot_step", "tick_size"),
    [
        ("BTC", 5, "0.00001", "0.1"),
        ("ETH", 4, "0.0001", "0.01"),
        ("SOL", 2, "0.01", "0.0001"),
        ("DOGE", 0, "1", "0.000001"),
    ],
)
def test_precision_comes_from_sz_decimals(symbol, sz_decimals, lot_step, tick_size):
    """Lot step is 10^-szDecimals; prices allow 6 - szDecimals decimals."""
    spec = instrument_from_universe(symbol, {"sz_decimals": sz_decimals, "max_leverage": 10})
    assert spec.lot_step == D(lot_step)
    assert spec.tick_size == D(tick_size)
    assert spec.price_sig_figs == 5
    assert spec.min_notional == DEFAULT_MIN_NOTIONAL


def test_an_impossible_sz_decimals_is_refused():
    with pytest.raises(BrokerError, match="szDecimals"):
        instrument_from_universe("WAT", {"sz_decimals": 9})


def test_an_empty_universe_is_an_error():
    with pytest.raises(BrokerError):
        read_universe({"universe": []})
    with pytest.raises(BrokerError):
        read_universe({})
    with pytest.raises(BrokerError):
        read_universe("nope")


def test_the_broker_exposes_the_instrument_and_caches_it():
    b = broker()
    spec = b.fetch_instrument("BTC")
    assert spec.lot_step == D("0.00001")
    assert b.fetch_instrument("BTC") is spec


def test_an_unknown_coin_says_what_the_real_ones_look_like():
    with pytest.raises(BrokerError, match="BTC"):
        broker().fetch_instrument("BTC_USDT_Perp")


# --- prices ------------------------------------------------------------------


def test_reads_the_mark_price_by_universe_index():
    assert read_mark_price([META, CTXS], "BTC") == D("100000.0")
    assert read_mark_price([META, CTXS], "ETH") == D("3000.5")


def test_falls_back_to_the_oracle_price():
    ctxs = [{"markPx": None, "oraclePx": "99999.5"}, {}, {}]
    assert read_mark_price([META, ctxs], "BTC") == D("99999.5")


def test_a_price_of_zero_is_refused_rather_than_returned():
    with pytest.raises(BrokerError):
        read_mark_price([META, [{"markPx": "0"}, {}, {}]], "BTC")


def test_a_context_with_no_price_is_an_error():
    with pytest.raises(BrokerError, match="no price"):
        read_mark_price([META, [{"funding": "0.0001"}, {}, {}]], "BTC")


def test_a_malformed_meta_and_ctxs_reply_is_an_error():
    with pytest.raises(BrokerError, match=r"\[meta, contexts\]"):
        read_mark_price({"universe": []}, "BTC")


def test_an_unlisted_coin_has_no_price():
    with pytest.raises(BrokerError, match="not in the perp universe"):
        read_mark_price([META, CTXS], "WIF")


def test_reads_an_all_mids_reply():
    assert read_mid_price({"BTC": "100001.0"}, "BTC") == D("100001.0")
    with pytest.raises(BrokerError):
        read_mid_price({"ETH": "3000"}, "BTC")


def test_the_broker_fetches_a_price():
    assert broker().fetch_mark_price("BTC") == D("100000.0")


# --- account -----------------------------------------------------------------


def test_reads_equity_from_the_margin_summary():
    assert read_equity(USER_STATE) == D("13109.482328")
    assert read_equity({"crossMarginSummary": {"accountValue": "5"}}) == D(5)
    assert read_equity({"withdrawable": "7"}) == D(7)


def test_an_empty_user_state_names_the_classic_mistake():
    """Querying with the API wallet address returns nothing, with no error."""
    with pytest.raises(BrokerError, match="API wallet address"):
        read_equity({})


def test_reads_positions_and_drops_flat_rows():
    positions = read_positions(USER_STATE)
    assert set(positions) == {"ETH", "SOL"}
    assert positions["ETH"].side is Side.BUY
    assert positions["ETH"].qty == D("0.0335")
    assert positions["ETH"].entry_price == D("2986.3")
    # szi is signed: negative is short.
    assert positions["SOL"].side is Side.SELL
    assert positions["SOL"].qty == D("3.5")


def test_no_positions_is_an_empty_dict():
    assert read_positions({"assetPositions": []}) == {}
    assert read_positions({"marginSummary": {}}) == {}


def test_the_broker_queries_the_master_address_not_the_api_wallet():
    info = StubInfo()
    b = broker(info=info)
    b.fetch_equity()
    user_calls = [args for name, args in info.calls if name == "user_state"]
    assert user_calls and user_calls[0][0] == ADDRESS


def test_the_broker_filters_positions_by_symbol():
    b = broker()
    assert set(b.fetch_positions("ETH")) == {"ETH"}
    assert set(b.fetch_positions()) == {"ETH", "SOL"}


def test_connecting_without_the_master_address_is_refused():
    config = load_config(
        data={"exchange": "hyperliquid", "hyperliquid": {"api_wallet_key": "0xkey"}}, env={}
    )
    b = HyperliquidBroker(config.hyperliquid, info=StubInfo(), exchange=StubExchange())
    with pytest.raises(BrokerError, match="account_address"):
        b.connect()


# --- candles -----------------------------------------------------------------


def test_reads_candles_oldest_first():
    candles = read_candles(CANDLES, "BTC", "5m")
    assert [c.close for c in candles] == [D("99400"), D("99800")]
    assert candles[0].open_time < candles[1].open_time
    assert candles[0].high == D("99500")


def test_candles_sort_even_if_the_exchange_does_not():
    candles = read_candles(list(reversed(CANDLES)), "BTC", "5m")
    assert [c.close for c in candles] == [D("99400"), D("99800")]


def test_an_unreadable_candle_is_an_error():
    with pytest.raises(BrokerError, match="unreadable candle"):
        read_candles([{"t": "soon", "o": "1", "h": "1", "l": "1", "c": "1"}], "BTC", "5m")


def test_no_candles_is_an_empty_list():
    assert read_candles(None, "BTC", "5m") == []
    assert read_candles([], "BTC", "5m") == []


def test_the_broker_asks_for_the_right_window():
    info = StubInfo()
    b = broker(info=info)
    b.fetch_candles("BTC", "5m", 100)
    name, args = next(c for c in info.calls if c[0] == "candles_snapshot")
    symbol, interval, start, end = args
    assert (symbol, interval) == ("BTC", "5m")
    # 101 bars of 5 minutes, because the newest bar is still forming.
    assert end - start == 101 * 5 * 60 * 1000


def test_an_unknown_interval_is_refused():
    with pytest.raises(BrokerError, match="interval"):
        broker().fetch_candles("BTC", "7m", 10)


def test_asking_for_no_candles_does_nothing():
    assert broker().fetch_candles("BTC", "5m", 0) == []


# --- placing orders ----------------------------------------------------------


def approved_order(symbol="BTC"):
    config = load_config(data={"exchange": "hyperliquid", "trading": {"symbol": symbol}}, env={})
    spec = instrument_from_universe(symbol, {"sz_decimals": 5, "max_leverage": 40})
    signal = Signal.build(symbol, "buy", "100000", "99000", reason="unit test")
    state = PortfolioState(equity=D("10000"), peak_equity=D("10000"))
    from tradebot.core.engine import utc_now

    decision = risk_gate(signal, state, config.risk, spec, D("100000"), utc_now())
    assert not hasattr(decision, "code"), decision
    return decision


def test_orders_go_out_as_gtc_limit_orders_with_our_cloid():
    order = approved_order()
    exchange = StubExchange()
    receipt = broker(exchange=exchange).place_order(order)

    sent = exchange.orders[-1]
    assert sent["name"] == "BTC"
    assert sent["is_buy"] is True
    assert sent["order_type"] == {"limit": {"tif": "Gtc"}}
    assert sent["reduce_only"] is False
    assert sent["limit_px"] == pytest.approx(float(order.limit_price))
    assert sent["cloid"].to_raw() == f"0x{order.client_order_id}"
    assert receipt.broker_order_id == "77"
    assert receipt.simulated is False


def test_our_order_id_is_a_valid_128_bit_cloid():
    """Hyperliquid requires exactly 32 hex characters after the 0x."""
    order = approved_order()
    assert len(order.client_order_id) == 32
    assert StubCloid.from_str(f"0x{order.client_order_id}").to_raw().startswith("0x")


def test_a_filled_order_is_reported_as_filled():
    reply = {
        "status": "ok",
        "response": {
            "type": "order",
            "data": {"statuses": [{"filled": {"totalSz": "0.09", "avgPx": "100000", "oid": 9}}]},
        },
    }
    receipt = broker(exchange=StubExchange(reply)).place_order(approved_order())
    assert receipt.broker_order_id == "9"


def test_an_error_status_raises():
    reply = {
        "status": "ok",
        "response": {
            "type": "order",
            "data": {"statuses": [{"error": "Order must have minimum value of $10."}]},
        },
    }
    with pytest.raises(BrokerError, match="minimum value"):
        broker(exchange=StubExchange(reply)).place_order(approved_order())


def test_a_top_level_failure_raises():
    with pytest.raises(BrokerError, match="rejected"):
        broker(exchange=StubExchange({"status": "err", "response": "bad nonce"})).place_order(
            approved_order()
        )


def test_a_reply_without_an_oid_says_to_reconcile():
    reply = {"status": "ok", "response": {"type": "order", "data": {"statuses": [{}]}}}
    with pytest.raises(BrokerError, match="reconcile"):
        broker(exchange=StubExchange(reply)).place_order(approved_order())


def test_a_reply_with_no_statuses_says_to_reconcile():
    with pytest.raises(BrokerError, match="reconcile"):
        read_order_result({"status": "ok", "response": {"type": "order", "data": {}}})


def test_statuses_can_sit_at_the_top_level():
    assert read_order_result({"statuses": [{"resting": {"oid": 3}}]}) == ("3", False)


def test_a_read_only_session_cannot_trade():
    """No API wallet key: the market and the account are readable, nothing else."""
    config = load_config(
        data={"exchange": "hyperliquid", "hyperliquid": {"account_address": ADDRESS}}, env={}
    )
    b = HyperliquidBroker(config.hyperliquid, info=StubInfo())
    b.connect()
    assert b.fetch_mark_price("BTC") == D("100000.0")
    with pytest.raises(BrokerError, match="api_wallet_key"):
        b.place_order(approved_order())


# --- closing and leverage ----------------------------------------------------


def test_closing_uses_a_reduce_only_ioc_order_that_crosses_the_book():
    exchange = StubExchange()
    b = broker(exchange=exchange)
    receipt = b.close_position("ETH")  # held long 0.0335 in USER_STATE
    sent = exchange.orders[-1]
    assert sent["name"] == "ETH"
    assert sent["is_buy"] is False  # selling to close a long
    assert sent["reduce_only"] is True
    assert sent["order_type"] == {"limit": {"tif": "Ioc"}}
    # 5% through the mark price of 3000.5, so it actually fills.
    assert sent["limit_px"] < float(D("3000.5"))
    assert receipt.qty == D("0.0335")


def test_closing_a_short_buys_back():
    exchange = StubExchange()
    broker(exchange=exchange).close_position("SOL")  # held short 3.5
    assert exchange.orders[-1]["is_buy"] is True
    assert exchange.orders[-1]["limit_px"] > float(D("150.25"))


def test_closing_nothing_is_not_an_error():
    assert broker().close_position("BTC") is None


def test_cancelling_uses_our_own_client_order_id():
    exchange = StubExchange()
    b = broker(exchange=exchange)
    b.cancel_order("BTC", "a" * 32)
    assert exchange.cancels[-1] == ("BTC", "0x" + "a" * 32)


def test_leverage_can_be_set_and_is_capped_by_the_market():
    exchange = StubExchange()
    b = broker(exchange=exchange)
    b.set_leverage("BTC", Decimal(10))
    assert exchange.leverage == (10, "BTC", True)
    b.set_leverage("BTC", Decimal(999))  # BTC maxLeverage is 40
    assert exchange.leverage == (40, "BTC", True)


def test_an_unconnected_broker_refuses_to_work():
    config = load_config(
        data={"exchange": "hyperliquid", "hyperliquid": {"account_address": ADDRESS}}, env={}
    )
    b = HyperliquidBroker(config.hyperliquid)
    with pytest.raises(BrokerError, match="not connected"):
        b.fetch_equity()


def test_the_base_url_follows_the_network():
    testnet = broker()
    assert "testnet" in testnet.base_url
    mainnet = broker(
        hyperliquid={"network": "mainnet", "account_address": ADDRESS, "api_wallet_key": "0xkey"}
    )
    assert mainnet.base_url == "https://api.hyperliquid.xyz"
