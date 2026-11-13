/* เทส lib/backtest.js — การแปลง logs/backtest.json เป็นภาพที่หน้าเว็บวาดได้
 *
 * ไม่มี DOM ไม่มีไฟล์ ไม่มีเน็ต เพราะ derive() เป็น pure function
 * ตัวเลขใน input เป็นสตริงทุกตัว เหมือนที่ report.to_dict ของฝั่ง Python เขียน
 */

"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const Backtest = require("../lib/backtest.js");
const Pipeline = require("../lib/pipeline.js");

/** ผลการทดสอบที่สมบูรณ์พอจะเช็คได้ทุกช่อง */
function sample(extra) {
  return {
    symbol: "BTC_USDT_Perp",
    interval: "1h",
    strategy: "MaCrossStrategy",
    exchange: "grvt",
    start: "2025-01-01T00:00:00+00:00",
    end: "2025-12-31T00:00:00+00:00",
    bars: 8760,
    starting_equity: "10000",
    final_equity: "11500.50",
    net_pnl: "1500.50",
    return_pct: "15.005",
    fees_paid: "120.25",
    max_drawdown: "800",
    max_drawdown_pct: "7.2727",
    cagr_pct: "15.0100000",
    buy_and_hold_pct: "9.5",
    bars_underwater: 1200,
    days_underwater: "50",
    worst_month: { month: "2025-06", return_pct: "-4.25" },
    monthly_returns: [
      { month: "2025-01", return_pct: "2.5" },
      { month: "2025-06", return_pct: "-4.25" },
    ],
    sharpe: "1.35",
    sortino: "1.92",
    win_loss_ratio: "2.1",
    profit_without_best_two: "900",
    trades_per_year: "42.5",
    trades: 42,
    wins: 18,
    losses: 24,
    win_rate_pct: "42.857",
    profit_factor: "1.58",
    expectancy_r: "0.35",
    longest_losing_streak: 5,
    exposure_pct: "61.5",
    signals: 60,
    unfilled_orders: 3,
    rejects: { below_min_notional: 7, slippage_exceeded: 11 },
    halted_at: null,
    halt_reason: "",
    warnings: [],
    no_trade_reason: "",
    thin_sample_below: 30,
    equity_curve: [
      { at: "2025-01-01T00:00:00+00:00", equity: "10000" },
      { at: "2025-06-01T00:00:00+00:00", equity: "9200" },
      { at: "2025-12-31T00:00:00+00:00", equity: "11500.50" },
    ],
    trade_list: [
      {
        side: "buy",
        qty: "0.05",
        entry_at: "2025-01-02T00:00:00+00:00",
        entry_price: "95000",
        exit_at: "2025-01-03T00:00:00+00:00",
        exit_price: "96000",
        net_pnl: "48.5",
        r_multiple: "1.2",
        exit_reason: "target",
      },
      {
        side: "sell",
        qty: "0.04",
        entry_at: "2025-02-02T00:00:00+00:00",
        entry_price: "97000",
        exit_at: "2025-02-03T00:00:00+00:00",
        exit_price: "98000",
        net_pnl: "-41.0",
        r_multiple: "-1",
        exit_reason: "stop (gap)",
      },
    ],
    ...(extra || {}),
  };
}

/** เส้นทุนยาวๆ ที่มีหลุมกับยอดซ่อนอยู่กลางทาง */
function longCurve(length, dipAt, dipTo, peakAt, peakTo) {
  const out = [];
  for (let i = 0; i < length; i += 1) {
    let equity = 10000;
    if (i === dipAt) equity = dipTo;
    if (i === peakAt) equity = peakTo;
    out.push({ at: `2025-01-01T00:${String(i % 60).padStart(2, "0")}:00+00:00`, equity: String(equity) });
  }
  return out;
}

/* ---------- ยังไม่มีไฟล์ ---------- */

test("ไม่มีไฟล์ผลการทดสอบ คืนรูปร่างเดิมครบทุกช่อง ไม่ throw", () => {
  for (const nothing of [null, undefined, "", 0, "ไม่ใช่ออบเจกต์"]) {
    const v = Backtest.derive(nothing);
    assert.equal(v.present, false);
    assert.equal(v.curve.points.length, 0);
    assert.equal(v.trades.rows.length, 0);
    assert.equal(v.quality.trades, 0);
    assert.equal(v.gate.halted, null);
    assert.ok(Array.isArray(v.notes) && v.notes.length > 0, "ข้อจำกัดต้องขึ้นจอแม้ยังไม่มีผล");
    assert.equal(v.thin, false);
  }
});

test("ไฟล์ที่ไม่มีคีย์อะไรเลย ไม่ทำให้พัง", () => {
  const v = Backtest.derive({});
  assert.equal(v.present, true);
  assert.equal(v.head.symbol, "");
  assert.equal(v.result.netPnl, 0);
  assert.equal(v.quality.sharpe, null);
  assert.equal(v.curve.total, 0);
  assert.equal(v.thin, false, "ไม่มีเกณฑ์ ก็ไม่ควรตัดสินว่าตัวอย่างบาง");
});

/* ---------- ตัวเลขที่เป็นสตริง ---------- */

test("ตัวเลขที่ส่งมาเป็นสตริงกลายเป็น Number ที่คำนวณต่อได้", () => {
  const v = Backtest.derive(sample());
  assert.equal(v.result.netPnl, 1500.5);
  assert.equal(v.result.returnPct, 15.005);
  assert.equal(v.result.startEquity, 10000);
  assert.equal(v.risk.maxDrawdown, 800);
  assert.equal(v.quality.winRatePct, 42.857);
  assert.equal(typeof v.result.netPnl, "number");
});

test("null แปลว่าวัดไม่ได้ ต้องไม่กลายเป็นศูนย์", () => {
  const v = Backtest.derive(
    sample({
      sharpe: null,
      sortino: null,
      profit_factor: null,
      win_loss_ratio: null,
      cagr_pct: null,
      buy_and_hold_pct: null,
      worst_month: null,
    })
  );
  assert.equal(v.quality.sharpe, null);
  assert.equal(v.quality.sortino, null);
  assert.equal(v.quality.profitFactor, null);
  assert.equal(v.quality.winLossRatio, null);
  assert.equal(v.result.cagrPct, null);
  assert.equal(v.hold.hold, null);
  assert.equal(v.hold.winner, null, "ไม่มีราคาเทียบ ก็ตัดสินผู้ชนะไม่ได้");
  assert.equal(v.risk.worstMonth, null);
});

test("ศูนย์ที่วัดได้จริง ไม่ถูกกลืนเป็นวัดไม่ได้", () => {
  const v = Backtest.derive(sample({ profit_factor: "0", buy_and_hold_pct: "0" }));
  assert.equal(v.quality.profitFactor, 0);
  assert.equal(v.hold.hold, 0);
});

/* ---------- เทียบกับการอยู่เฉยๆ ---------- */

test("บอทชนะเมื่อผลตอบแทนมากกว่าซื้อแล้วถือ", () => {
  assert.equal(Backtest.derive(sample()).hold.winner, "bot");
});

test("ซื้อแล้วถือชนะเมื่อบอททำได้ไม่ดีกว่า รวมกรณีเท่ากัน", () => {
  assert.equal(Backtest.derive(sample({ buy_and_hold_pct: "20" })).hold.winner, "hold");
  assert.equal(
    Backtest.derive(sample({ buy_and_hold_pct: "15.005" })).hold.winner,
    "hold",
    "เสมอกันไม่ใช่ชนะ"
  );
});

/* ---------- ผลที่ยืนอยู่บนสองไม้ ---------- */

test("กำไรที่หายหมดเมื่อตัดไม้ดีสุดสองไม้ ถูกตั้งธงไว้", () => {
  const v = Backtest.derive(sample({ profit_without_best_two: "-50" }));
  assert.equal(v.quality.restsOnTwo, true);
});

test("ขาดทุนอยู่แล้ว ไม่ต้องตั้งธงว่ายืนบนสองไม้", () => {
  const v = Backtest.derive(sample({ net_pnl: "-500", profit_without_best_two: "-900" }));
  assert.equal(v.quality.restsOnTwo, false);
});

/* ---------- กลุ่มตัวอย่างบาง ---------- */

test("เกณฑ์ตัวอย่างบางมาจากไฟล์ ไม่ใช่เลขที่เขียนไว้ใน JS", () => {
  assert.equal(Backtest.derive(sample({ trades: 29 })).thin, true);
  assert.equal(Backtest.derive(sample({ trades: 30 })).thin, false);
  // ฝั่ง Python ขยับเกณฑ์ ฝั่งนี้ต้องขยับตามเองโดยไม่ต้องแก้โค้ด
  assert.equal(Backtest.derive(sample({ trades: 40, thin_sample_below: 100 })).thin, true);
});

test("ตัวอย่างบางขึ้นเป็นข้อแรกของรายการข้อจำกัด", () => {
  const v = Backtest.derive(sample({ trades: 4 }));
  assert.ok(v.notes[0].includes("4 ไม้"), v.notes[0]);
  assert.ok(v.notes[0].includes("30"), "ต้องอ้างเกณฑ์ที่ไฟล์บอกมา");
  assert.ok(v.notes.length > Backtest.NOTES.length);
});

test("ตัวอย่างไม่บาง รายการข้อจำกัดเหลือเท่าเดิม และ funding อยู่ในนั้นเสมอ", () => {
  const v = Backtest.derive(sample());
  assert.equal(v.notes.length, Backtest.NOTES.length);
  assert.ok(v.notes.some((n) => n.includes("funding")), "funding ที่ไม่ถูกหักต้องขึ้นจอทุกครั้ง");
});

/* ---------- ย่อเส้นทุน ---------- */

test("เส้นทุนสั้นกว่าเพดาน ไม่ถูกย่อ", () => {
  const v = Backtest.derive(sample(), { curveLimit: 600 });
  assert.equal(v.curve.total, 3);
  assert.equal(v.curve.kept, 3);
  assert.equal(v.curve.points[1].equity, 9200);
});

test("เส้นทุนยาวถูกย่อ แต่หลุมที่ลึกสุดกับยอดที่สูงสุดต้องรอด", () => {
  const raw = sample({ equity_curve: longCurve(5000, 2500, 4000, 3700, 16000) });
  const v = Backtest.derive(raw, { curveLimit: 200 });

  assert.equal(v.curve.total, 5000);
  assert.ok(v.curve.kept <= 420, "ย่อแล้วต้องเหลือไม่เกินสองจุดต่อช่วง: " + v.curve.kept);
  assert.ok(v.curve.kept < v.curve.total);

  const kept = v.curve.points.map((p) => p.equity);
  assert.ok(kept.includes(4000), "drawdown ที่ลึกสุดหายไปจากกราฟ = กราฟโกหกในทางที่สวยขึ้น");
  assert.ok(kept.includes(16000), "ยอดสูงสุดต้องยังอยู่");
  assert.equal(v.curve.min, 4000);
  assert.equal(v.curve.max, 16000);
});

test("จุดแรกและจุดสุดท้ายยังอยู่หลังย่อ และเวลายังเรียงจากต้นไปปลาย", () => {
  const curve = longCurve(3000, 1500, 7000, 2000, 13000);
  const v = Backtest.derive(sample({ equity_curve: curve }), { curveLimit: 100 });
  assert.equal(v.curve.points[0].at, curve[0].at);
  assert.equal(v.curve.points[v.curve.points.length - 1].at, curve[curve.length - 1].at);
  assert.equal(v.curve.from, curve[0].at);
  assert.equal(v.curve.to, curve[curve.length - 1].at);
});

test("thin() เรียกตรงๆ ก็คืนจุดเดิมเมื่อไม่ต้องย่อ", () => {
  const points = [{ at: "a", equity: 1 }, { at: "b", equity: 2 }];
  const out = Backtest.thin(points, 10);
  assert.deepEqual(out, points);
  assert.notEqual(out, points, "ต้องคืนสำเนา ไม่ใช่ตัวเดิม");
});

test("จุดที่อ่านค่าทุนไม่ได้ถูกข้าม ไม่กลายเป็นศูนย์กลางกราฟ", () => {
  const v = Backtest.derive(
    sample({
      equity_curve: [
        { at: "2025-01-01T00:00:00+00:00", equity: "10000" },
        { at: "2025-01-02T00:00:00+00:00", equity: null },
        { at: "2025-01-03T00:00:00+00:00", equity: "10500" },
      ],
    })
  );
  assert.equal(v.curve.total, 2);
  assert.equal(v.curve.min, 10000);
});

/* ---------- ด่านความเสี่ยง ---------- */

test("เหตุผลที่ด่านปฏิเสธเรียงจากบ่อยสุด และแปลเป็นไทยด้วยตารางเดียวกับหน้า live", () => {
  const v = Backtest.derive(sample());
  assert.equal(v.gate.rejects[0].code, "slippage_exceeded");
  assert.equal(v.gate.rejects[0].count, 11);
  assert.equal(v.gate.rejects[0].label, Pipeline.REJECT_TH.slippage_exceeded);
  assert.equal(v.gate.rejects[1].code, "below_min_notional");
  assert.equal(v.gate.rejectTotal, 18);
  assert.equal(v.gate.unfilled, 3);
});

test("reject code ที่ยังไม่มีคำแปล แสดงรหัสดิบไปก่อน ไม่ซ่อนทิ้ง", () => {
  const v = Backtest.derive(sample({ rejects: { a_brand_new_code: 2 } }));
  assert.equal(v.gate.rejects[0].label, "a_brand_new_code");
});

test("การทดสอบที่ชนเพดานแล้วหยุด บอกเวลาและเหตุผล", () => {
  const v = Backtest.derive(
    sample({ halted_at: "2025-07-01T12:00:00+00:00", halt_reason: "daily_loss_limit: ชนเพดาน" })
  );
  assert.equal(v.gate.halted.at, "2025-07-01T12:00:00+00:00");
  assert.match(v.gate.halted.reason, /daily_loss_limit/);
});

/* ---------- ไม่มีไม้เลย ---------- */

test("เหตุผลที่ไม่มีไม้เลยถูกแปลเป็นไทยจากรหัสที่ Python ส่งมา", () => {
  const cases = {
    no_candles: /แท่งเทียน/,
    no_signal: /ไม่ออกสัญญาณ/,
    unrecorded: /ไม่มีบันทึก/,
  };
  for (const [code, pattern] of Object.entries(cases)) {
    const v = Backtest.derive(sample({ trades: 0, trade_list: [], no_trade_reason: code }));
    assert.match(v.noTrades, pattern, code);
  }
});

test("ถูกปฏิเสธทุกสัญญาณ บอกด้วยว่าเหตุผลไหนบ่อยสุด เป็นภาษาคน", () => {
  const v = Backtest.derive(
    sample({ trades: 0, trade_list: [], no_trade_reason: "all_refused" })
  );
  assert.ok(
    v.noTrades.includes(Pipeline.REJECT_TH.slippage_exceeded),
    "ต้องอ้างเหตุผลที่บ่อยสุดเป็นภาษาคน ไม่ใช่รหัส: " + v.noTrades
  );
  assert.match(v.noTrades, /11 ครั้ง/);
});

test("ผ่านด่านแล้วแต่ไม่เคย fill บอกจำนวนออเดอร์ที่ค้าง", () => {
  const v = Backtest.derive(
    sample({ trades: 0, trade_list: [], rejects: {}, no_trade_reason: "never_filled" })
  );
  assert.match(v.noTrades, /3 ออเดอร์/);
  assert.match(v.noTrades, /limit/);
});

test("รหัสที่ยังไม่รู้จัก ส่งผ่านไปตรงๆ ไม่กลืนหาย", () => {
  const v = Backtest.derive(sample({ trades: 0, no_trade_reason: "something_new" }));
  assert.equal(v.noTrades, "something_new");
});

test("มีไม้แล้วไม่ต้องอธิบายว่าทำไมไม่มีไม้", () => {
  assert.equal(Backtest.derive(sample()).noTrades, "");
});

/* ---------- รายการไม้ ---------- */

test("ไม้ใหม่สุดอยู่บน และเหตุผลที่ออกถูกแปลเป็นไทย", () => {
  const v = Backtest.derive(sample());
  assert.equal(v.trades.total, 2);
  assert.equal(v.trades.rows[0].entryAt, "2025-02-02T00:00:00+00:00");
  assert.equal(v.trades.rows[0].exitLabel, Backtest.EXIT_TH["stop (gap)"]);
  assert.equal(v.trades.rows[1].exitLabel, Backtest.EXIT_TH.target);
  assert.equal(v.trades.rows[0].netPnl, -41);
  assert.equal(v.trades.rows[0].r, -1);
});

test("ไม้ดีสุดและแย่สุดมาจากทุกไม้ ไม่ใช่แค่ที่ขึ้นจอ", () => {
  const list = [];
  for (let i = 0; i < 300; i += 1) {
    list.push({
      side: "buy",
      qty: "1",
      entry_at: "2025-01-01T00:00:00+00:00",
      entry_price: "100",
      exit_at: "2025-01-02T00:00:00+00:00",
      exit_price: "100",
      net_pnl: String(i === 0 ? -999 : i === 1 ? 888 : 1),
      r_multiple: "0.1",
      exit_reason: "stop",
    });
  }
  const v = Backtest.derive(sample({ trade_list: list }), { tradeLimit: 50 });
  assert.equal(v.trades.total, 300);
  assert.equal(v.trades.rows.length, 50);
  assert.equal(v.trades.hidden, 250);
  assert.equal(v.trades.best, 888, "ไม้ดีสุดอยู่ในกลุ่มที่ไม่ขึ้นจอ แต่ตัวเลขต้องยังถูก");
  assert.equal(v.trades.worst, -999);
});

test("เหตุผลที่ออกซึ่งยังไม่มีคำแปล แสดงข้อความเดิม", () => {
  const v = Backtest.derive(
    sample({
      trade_list: [
        {
          side: "buy",
          qty: "1",
          entry_at: "2025-01-01T00:00:00+00:00",
          entry_price: "1",
          exit_at: "2025-01-02T00:00:00+00:00",
          exit_price: "1",
          net_pnl: "0",
          r_multiple: "0",
          exit_reason: "funding squeeze",
        },
      ],
    })
  );
  assert.equal(v.trades.rows[0].exitLabel, "funding squeeze");
});

/* ---------- หัวเรื่องและรายเดือน ---------- */

test("หัวเรื่องบอกว่าเป็นผลของกลยุทธ์ไหน บนอะไร ช่วงไหน", () => {
  const v = Backtest.derive(sample());
  assert.equal(v.head.strategy, "MaCrossStrategy");
  assert.equal(v.head.symbol, "BTC_USDT_Perp");
  assert.equal(v.head.interval, "1h");
  assert.equal(v.head.exchange, "grvt");
  assert.equal(v.head.bars, 8760);
});

test("ผลตอบแทนรายเดือนถูกแปลงเป็นตัวเลขทุกแถว", () => {
  const v = Backtest.derive(sample());
  assert.equal(v.monthly.length, 2);
  assert.equal(v.monthly[1].month, "2025-06");
  assert.equal(v.monthly[1].returnPct, -4.25);
});

test("คำเตือนจากตัวรัน ถูกส่งต่อขึ้นจอ", () => {
  const v = Backtest.derive(sample({ warnings: ["the strategy stamped its signals", ""] }));
  assert.deepEqual(v.warnings, ["the strategy stamped its signals"]);
});
