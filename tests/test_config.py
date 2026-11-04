"""Configuration tests: two exchanges, one config file, and regressions for
two credential bugs that shipped in the project this grew out of.
"""

from dataclasses import FrozenInstanceError
from decimal import Decimal
from pathlib import Path

import pytest

from tradebot.core.config import (
    DEFAULTS,
    BotConfig,
    ConfigError,
    Secret,
    describe,
    load_config,
    raw_defaults,
)
from tradebot.money import D

ADDRESS = "0x1234567890abcdef1234567890abcdef12345678"
KEY = "0xaaaabbbbccccddddeeeeffff00001111222233334444555566667777888899990"

GRVT_MINIMAL = {
    "grvt": {"api_key": "k", "private_key": "p", "trading_account_id": "acct-1"},
}
HL_MINIMAL = {
    "exchange": "hyperliquid",
    "hyperliquid": {"account_address": ADDRESS, "api_wallet_key": KEY},
}


def write(tmp_path, text: str):
    path = tmp_path / "config.yaml"
    path.write_text(text, encoding="utf-8")
    return path


# --- choosing the exchange ---------------------------------------------------


def test_grvt_is_the_default():
    config = load_config(env={})
    assert config.exchange == "grvt"
    assert config.trading.symbol == "BTC_USDT_Perp"
    assert config.venue is config.grvt


def test_switching_exchange_switches_the_defaults():
    config = load_config(data={"exchange": "hyperliquid"}, env={})
    assert config.trading.symbol == "BTC"
    assert config.venue is config.hyperliquid


def test_an_unknown_exchange_is_refused():
    with pytest.raises(ConfigError, match="exchange"):
        load_config(data={"exchange": "nasdaq"}, env={})


def test_the_exchange_can_come_from_the_environment():
    config = load_config(env={"TRADEBOT_EXCHANGE": "hyperliquid"})
    assert config.exchange == "hyperliquid"


def test_instrument_defaults_follow_the_exchange():
    grvt = load_config(env={}).instrument
    assert grvt.lot_step == D("0.001")
    assert grvt.min_qty == D("0.001"), "GRVT publishes a minimum order size"
    assert grvt.price_sig_figs == 0, "GRVT uses a plain tick"

    hl = load_config(data={"exchange": "hyperliquid"}, env={}).instrument
    assert hl.lot_step == D("0.00001")
    assert hl.min_qty == 0, "Hyperliquid has no minimum size rule"
    assert hl.price_sig_figs == 5


def test_the_user_can_still_override_instrument_values():
    config = load_config(data={"instrument": {"tick_size": 0.5}}, env={})
    assert config.instrument.tick_size == D("0.5")
    assert config.instrument.lot_step == D("0.001"), "ค่าอื่นยังมาจาก exchange"


# --- a symbol written for the wrong venue -----------------------------------


def test_a_hyperliquid_symbol_on_grvt_is_refused():
    with pytest.raises(ConfigError, match="BTC_USDT_Perp"):
        load_config(data={"exchange": "grvt", "trading": {"symbol": "BTC"}}, env={})


def test_a_grvt_symbol_on_hyperliquid_is_refused():
    with pytest.raises(ConfigError, match="coin on its own"):
        load_config(
            data={"exchange": "hyperliquid", "trading": {"symbol": "BTC_USDT_Perp"}}, env={}
        )


def test_an_empty_symbol_falls_back_to_the_venue_default():
    assert load_config(data={"trading": {"symbol": ""}}, env={}).trading.symbol == "BTC_USDT_Perp"


# --- live credentials, per venue --------------------------------------------


def test_grvt_live_needs_its_three_values():
    config = load_config(data={"grvt": {"api_key": "only-this"}}, env={})
    with pytest.raises(ConfigError) as exc:
        config.require_live_credentials()
    assert "private_key" in str(exc.value)
    assert "trading_account_id" in str(exc.value)


def test_hyperliquid_live_needs_its_two_values():
    config = load_config(
        data={"exchange": "hyperliquid", "hyperliquid": {"account_address": ADDRESS}}, env={}
    )
    with pytest.raises(ConfigError, match="api_wallet_key"):
        config.require_live_credentials()


def test_complete_configs_pass_the_live_check():
    load_config(data=GRVT_MINIMAL, env={}).require_live_credentials()
    load_config(data=HL_MINIMAL, env={}).require_live_credentials()


def test_credentials_of_the_other_venue_do_not_count():
    """A filled-in Hyperliquid block must not satisfy a GRVT session."""
    data = dict(HL_MINIMAL)
    data["exchange"] = "grvt"
    config = load_config(data=data, env={})
    with pytest.raises(ConfigError, match="GRVT"):
        config.require_live_credentials()


def test_live_detection_per_venue():
    assert load_config(env={}).is_live is False
    assert load_config(data={"grvt": {"env": "prod"}}, env={}).is_live is True
    assert load_config(data={"exchange": "hyperliquid"}, env={}).is_live is False
    live_hl = load_config(
        data={"exchange": "hyperliquid", "hyperliquid": {"network": "mainnet"}}, env={}
    )
    assert live_hl.is_live is True
    # A prod GRVT block does not make a Hyperliquid session live.
    mixed = load_config(data={"exchange": "hyperliquid", "grvt": {"env": "prod"}}, env={})
    assert mixed.is_live is False


def test_venue_name_reports_the_right_field():
    assert load_config(env={}).venue_name == "testnet"
    assert load_config(data={"grvt": {"env": "prod"}}, env={}).venue_name == "prod"
    assert (
        load_config(
            data={"exchange": "hyperliquid", "hyperliquid": {"network": "mainnet"}}, env={}
        ).venue_name
        == "mainnet"
    )


@pytest.mark.parametrize(
    "patch",
    [
        {"grvt": {"env": "staging"}},
        {"exchange": "hyperliquid", "hyperliquid": {"network": "devnet"}},
    ],
)
def test_unknown_environments_are_refused(patch):
    with pytest.raises(ConfigError):
        load_config(data=patch, env={})


def test_an_api_wallet_key_in_the_address_field_is_rejected():
    """The Hyperliquid docs warn that mixing these two up returns nothing."""
    with pytest.raises(ConfigError, match="master account"):
        load_config(
            data={"exchange": "hyperliquid", "hyperliquid": {"account_address": KEY}}, env={}
        )


# --- regression: shallow copy leaked credentials between objects ------------


def test_loading_one_config_does_not_touch_the_defaults():
    load_config(data=GRVT_MINIMAL, env={})
    assert DEFAULTS["grvt"]["api_key"] == ""
    assert DEFAULTS["trading"]["symbol"] == ""


def test_two_configs_never_share_state():
    first = load_config(data=GRVT_MINIMAL, env={})
    second = load_config(env={})
    assert first.grvt.api_key.reveal() == "k"
    assert second.grvt.api_key.reveal() == ""


def test_raw_defaults_hands_back_an_independent_tree():
    mine = raw_defaults()
    mine["grvt"]["api_key"] = "mine"
    mine["risk"]["max_positions"] = 99
    assert DEFAULTS["grvt"]["api_key"] == ""
    assert DEFAULTS["risk"]["max_positions"] == 1


# --- regression: printing the config used to destroy the credentials -------


def test_describing_a_config_leaves_the_credentials_intact():
    config = load_config(data={"grvt": {"api_key": "SECRET-A"}}, env={})
    text = describe(config)
    assert "SECRET-A" not in text
    assert config.grvt.api_key.reveal() == "SECRET-A"


def test_secrets_refuse_to_appear_in_output():
    secret = Secret("hunter2")
    assert "hunter2" not in repr(secret)
    assert "hunter2" not in str(secret)
    assert "hunter2" not in f"{secret}"
    assert secret.reveal() == "hunter2"
    assert bool(Secret("")) is False


def test_repr_of_the_whole_config_hides_secrets():
    config = load_config(data={"grvt": {"api_key": "SECRET-A"}}, env={})
    assert "SECRET-A" not in repr(config)


def test_describe_names_the_exchange_and_flags_real_money():
    paper = describe(load_config(env={}))
    live = describe(load_config(data={"grvt": {"env": "prod"}}, env={}))
    assert "GRVT testnet" in paper
    assert "REAL MONEY" not in paper
    assert "REAL MONEY" in live


# --- reading files and the environment -------------------------------------


def test_reads_a_yaml_file(tmp_path):
    path = write(
        tmp_path,
        """
exchange: hyperliquid
trading:
  symbol: SOL
  timeframe: 15m
  loop_interval: 15
  warmup_candles: 50
risk:
  risk_per_trade_pct: 2.5
""",
    )
    config = load_config(path, env={})
    assert config.exchange == "hyperliquid"
    assert config.trading.symbol == "SOL"
    assert config.trading.timeframe == "15m"
    assert config.trading.warmup_candles == 50
    assert config.risk.risk_per_trade == D("0.025")
    assert config.source == str(path)


def test_a_missing_file_is_an_error_not_a_silent_default(tmp_path):
    with pytest.raises(ConfigError, match="not found"):
        load_config(tmp_path / "nope.yaml")


def test_a_non_mapping_file_is_rejected(tmp_path):
    path = write(tmp_path, "- just\n- a list\n")
    with pytest.raises(ConfigError, match="mapping"):
        load_config(path)


def test_dollar_brace_values_come_from_the_environment(monkeypatch, tmp_path):
    monkeypatch.setenv("MY_KEY", "from-env")
    path = write(tmp_path, 'grvt:\n  api_key: "${MY_KEY}"\n')
    assert load_config(path, env={}).grvt.api_key.reveal() == "from-env"


def test_a_missing_referenced_variable_is_reported(monkeypatch, tmp_path):
    monkeypatch.delenv("ABSENT_KEY", raising=False)
    path = write(tmp_path, 'grvt:\n  api_key: "${ABSENT_KEY}"\n')
    with pytest.raises(ConfigError, match="ABSENT_KEY"):
        load_config(path)


def test_environment_variables_win_over_the_file(tmp_path):
    path = write(tmp_path, "trading:\n  symbol: BTC_USDT_Perp\n")
    config = load_config(path, env={"TRADEBOT_SYMBOL": "ETH_USDT_Perp"})
    assert config.trading.symbol == "ETH_USDT_Perp"
    assert config.instrument.symbol == "ETH_USDT_Perp"


def test_blank_environment_variables_are_ignored(tmp_path):
    path = write(tmp_path, "trading:\n  symbol: BTC_USDT_Perp\n")
    assert load_config(path, env={"TRADEBOT_SYMBOL": ""}).trading.symbol == "BTC_USDT_Perp"


# --- validation -------------------------------------------------------------


def test_percentages_become_fractions():
    config = load_config(
        data={"risk": {"max_daily_loss_pct": 3, "max_drawdown_pct": 10, "max_slippage_pct": 0.1}},
        env={},
    )
    assert config.risk.max_daily_loss == D("0.03")
    assert config.risk.max_drawdown == D("0.1")
    assert config.risk.max_slippage == D("0.001")


@pytest.mark.parametrize(
    "patch",
    [
        {"trading": {"loop_interval": 0}},
        {"trading": {"loop_interval": "soon"}},
        {"trading": {"timeframe": "7m"}},
        {"trading": {"warmup_candles": -1}},
        {"risk": {"risk_per_trade_pct": 0}},
        {"risk": {"risk_per_trade_pct": 80}},
        {"risk": {"max_positions": 0}},
        {"risk": {"starting_equity_usdt": -1}},
        {"instrument": {"lot_step": 0}},
        {"instrument": {"tick_size": -1}},
        {"instrument": {"price_sig_figs": -1}},
        {"instrument": {"min_qty": -1}},
    ],
)
def test_unusable_numbers_are_refused(patch):
    with pytest.raises(ConfigError):
        load_config(data=patch, env={})


def test_a_config_is_frozen():
    config = load_config(env={})
    assert isinstance(config, BotConfig)
    with pytest.raises(FrozenInstanceError):
        config.trading = None  # type: ignore[misc]


def test_decimals_all_the_way_down():
    config = load_config(env={})
    assert isinstance(config.risk.risk_per_trade, Decimal)
    assert isinstance(config.instrument.tick_size, Decimal)
    assert isinstance(config.instrument.min_qty, Decimal)


def test_the_shipped_example_config_loads_as_is():
    """`cp config.example.yaml config.yaml` then `--dry-run` must just work."""
    example = Path(__file__).resolve().parent.parent / "config" / "config.example.yaml"
    config = load_config(example, env={})
    assert config.exchange in ("grvt", "hyperliquid")
    with pytest.raises(ConfigError):
        config.require_live_credentials()
