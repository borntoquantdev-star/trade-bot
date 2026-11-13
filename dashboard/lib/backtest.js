/* แปลง logs/backtest.json เป็นภาพที่หน้าเว็บวาดได้
 *
 * พี่น้องกับ lib/pipeline.js และเล่นกฎเดียวกัน: pure ทั้งไฟล์ ไม่อ่านไฟล์
 * ไม่ต่อเน็ต ไม่แตะ DOM เพราะงั้นเทสได้ตรงๆ และใช้ได้ทั้งใน Node (server)
 * และในเบราว์เซอร์ (หน้าเว็บ) — คิดไม่ตรงกันไม่ได้เพราะเป็นไฟล์เดียวกัน
 *
 * สองเรื่องที่ไฟล์นี้มีอยู่เพื่อแก้:
 *
 * 1) ตัวเลขใน backtest.json เป็น "สตริง" ทุกตัวโดยเจตนา (report.to_dict เขียน
 *    Decimal ลงไปทั้งหลัก) ที่นี่คือจุดเดียวที่มันกลายเป็น Number และ null
 *    ที่แปลว่า "วัดไม่ได้" ต้องไม่กลายเป็น 0 ที่แปลว่า "วัดได้ว่าศูนย์"
 *
 * 2) equity_curve มีหนึ่งจุดต่อหนึ่งแท่ง — ทดสอบ 400 วันบน 5m คือ 115,199 จุด
 *    ไฟล์ 11 MB ส่งทั้งก้อนผ่าน SSE ไม่ได้ จึงย่อก่อนออกจากเซิร์ฟเวอร์
 *    โดยเก็บจุดต่ำสุดและสูงสุดของทุกช่วงไว้ ไม่งั้น drawdown จะหายไปจากกราฟ
 */

(function (root, factory) {
  if (typeof module === "object" && module.exports) {
    module.exports = factory(require("./pipeline.js"));
  } else {
    root.Backtest = factory(root.Pipeline);
  }
})(typeof self !== "undefined" ? self : this, function (Pipeline) {
  "use strict";

  const REJECT_TH = (Pipeline && Pipeline.REJECT_TH) || {};

  /** เหตุผลที่ออกจากไม้ ตามที่ runner.py บันทึกไว้ */
  const EXIT_TH = {
    stop: "โดน stop",
    "stop (gap)": "โดน stop (เปิดกระโดดข้าม)",
    target: "ถึงเป้า",
    "target (gap)": "ถึงเป้า (เปิดกระโดดข้าม)",
    "still open at the end": "ยังถืออยู่ตอนจบการทดสอบ",
  };

  /** รหัสจาก BacktestResult.no_trade_reason — บันไดเงื่อนไขอยู่ฝั่ง Python ที่เดียว */
  const NO_TRADE_TH = {
    no_candles: () => "ไม่มีแท่งเทียนให้รัน",
    no_signal: () => "กลยุทธ์ไม่ออกสัญญาณเลยบนข้อมูลชุดนี้",
    all_refused: (v) => {
      const top = v.gate.rejects[0];
      return top
        ? `ทุกสัญญาณถูกด่านความเสี่ยงปฏิเสธ บ่อยสุดคือ ${top.label} (${top.count} ครั้ง)`
        : "ทุกสัญญาณถูกด่านความเสี่ยงปฏิเสธ";
    },
    never_filled: (v) =>
      `${v.gate.unfilled} ออเดอร์ผ่านด่านแล้ว แต่ราคาไม่กลับมาที่ limit จึงไม่ได้ไม้`,
    unrecorded: () => "ไม่มีไม้เปิดเลย และไม่มีบันทึกว่าทำไม",
  };

  /** ข้อจำกัดที่ต้องขึ้นจอทุกครั้ง ไม่ใช่เชิงอรรถ — คู่ไทยของ report.caveats() */
  const NOTES = [
    "สัญลักษณ์เดียว ช่วงเดียว ค่าพารามิเตอร์ชุดเดียว เปลี่ยนอะไรก็เปลี่ยนเลข นั่นคือการทดสอบ ไม่ใช่ผลลัพธ์",
    "funding ของ perpetual ไม่ถูกหัก การถือข้ามวันจริงแพงกว่าที่เห็นนี้",
    "แบบจำลองการ fill ถือว่าออเดอร์เล็กพอที่จะไม่ขยับตลาด",
    "ขั้นต่อไปคือเทรดกระดาษ ไม่ใช่เงินจริง",
  ];

  /** ตัวเลขที่ต้องมีค่า */
  const num = (value) => {
    const n = Number(value);
    return Number.isFinite(n) ? n : 0;
  };

  /** ตัวเลขที่ "วัดไม่ได้" ได้ — null ต้องรอดออกไปเป็น null ไม่ใช่ 0 */
  const maybe = (value) => {
    if (value === null || value === undefined || value === "") return null;
    const n = Number(value);
    return Number.isFinite(n) ? n : null;
  };

  const text = (value) => (typeof value === "string" ? value : "");

  /** ย่อเส้นทุนให้พอวาด โดยไม่กินยอดกับก้นของทุกช่วงทิ้ง
   *
   * เก็บจุดต่ำสุดกับสูงสุดของแต่ละช่วง เรียงตามเวลาเดิม การเก็บทุกจุดที่ n
   * (เช่น เอาจุดที่ 100, 200, ...) จะทำให้ drawdown ที่เกิดระหว่างช่วงหายไป
   * ทั้งก้อน แล้วกราฟจะโกหกในทางที่สวยขึ้น ซึ่งเป็นทางที่ห้ามโกหกที่สุด
   */
  function thin(points, limit) {
    const cap = limit || 600;
    if (points.length <= cap) return points.slice();

    const size = points.length / cap;
    const out = [];
    for (let i = 0; i < cap; i += 1) {
      const from = Math.floor(i * size);
      const to = Math.min(points.length, Math.floor((i + 1) * size));
      if (to <= from) continue;
      let lo = from;
      let hi = from;
      for (let j = from + 1; j < to; j += 1) {
        if (points[j].equity < points[lo].equity) lo = j;
        if (points[j].equity > points[hi].equity) hi = j;
      }
      const first = Math.min(lo, hi);
      const second = Math.max(lo, hi);
      out.push(points[first]);
      if (second !== first) out.push(points[second]);
    }
    if (out[0] !== points[0]) out.unshift(points[0]);
    if (out[out.length - 1] !== points[points.length - 1]) out.push(points[points.length - 1]);
    return out;
  }

  function curveOf(raw, limit) {
    const all = [];
    for (const point of (raw && raw.equity_curve) || []) {
      const equity = maybe(point && point.equity);
      if (equity === null) continue;
      all.push({ at: text(point.at), equity });
    }
    const points = thin(all, limit);
    const values = all.map((p) => p.equity);
    return {
      points,
      total: all.length,
      kept: points.length,
      min: values.length ? Math.min(...values) : 0,
      max: values.length ? Math.max(...values) : 0,
      from: all.length ? all[0].at : "",
      to: all.length ? all[all.length - 1].at : "",
    };
  }

  function tradesOf(raw, limit) {
    const cap = limit || 200;
    const all = ((raw && raw.trade_list) || []).map((trade) => ({
      side: text(trade.side),
      qty: maybe(trade.qty),
      entryAt: text(trade.entry_at),
      entryPrice: maybe(trade.entry_price),
      exitAt: text(trade.exit_at),
      exitPrice: maybe(trade.exit_price),
      netPnl: num(trade.net_pnl),
      r: maybe(trade.r_multiple),
      exitReason: text(trade.exit_reason),
      exitLabel: EXIT_TH[text(trade.exit_reason)] || text(trade.exit_reason),
    }));
    const pnls = all.map((t) => t.netPnl);
    // ใหม่สุดอยู่บน เหมือน log ของหน้า live
    const rows = all.slice(-cap).reverse();
    return {
      rows,
      total: all.length,
      hidden: all.length - rows.length,
      best: pnls.length ? Math.max(...pnls) : null,
      worst: pnls.length ? Math.min(...pnls) : null,
    };
  }

  function gateOf(raw) {
    const counts = (raw && raw.rejects) || {};
    const rejects = Object.keys(counts)
      .map((code) => ({ code, label: REJECT_TH[code] || code, count: num(counts[code]) }))
      .sort((a, b) => b.count - a.count || a.code.localeCompare(b.code));
    const haltedAt = text(raw && raw.halted_at);
    return {
      signals: num(raw && raw.signals),
      rejects,
      rejectTotal: rejects.reduce((sum, row) => sum + row.count, 0),
      unfilled: num(raw && raw.unfilled_orders),
      halted: haltedAt ? { at: haltedAt, reason: text(raw && raw.halt_reason) } : null,
    };
  }

  /** รวมทุกอย่างเป็นภาพเดียว  raw = null (ยังไม่มีไฟล์) ก็ต้องคืนรูปร่างเดิม */
  function derive(raw, options) {
    const opts = options || {};
    if (!raw || typeof raw !== "object") {
      return {
        present: false,
        head: { symbol: "", interval: "", strategy: "", exchange: "", bars: 0, start: "", end: "" },
        result: {
          startEquity: 0,
          finalEquity: 0,
          netPnl: 0,
          returnPct: 0,
          cagrPct: null,
          feesPaid: 0,
        },
        hold: { bot: 0, hold: null, winner: null },
        risk: {
          maxDrawdown: 0,
          maxDrawdownPct: 0,
          daysUnderwater: 0,
          barsUnderwater: 0,
          worstMonth: null,
          longestLosingStreak: 0,
          exposurePct: 0,
        },
        quality: {
          trades: 0,
          wins: 0,
          losses: 0,
          winRatePct: 0,
          winLossRatio: null,
          profitFactor: null,
          expectancyR: 0,
          sharpe: null,
          sortino: null,
          tradesPerYear: 0,
          withoutBestTwo: 0,
          restsOnTwo: false,
        },
        gate: { signals: 0, rejects: [], rejectTotal: 0, unfilled: 0, halted: null },
        curve: { points: [], total: 0, kept: 0, min: 0, max: 0, from: "", to: "" },
        trades: { rows: [], total: 0, hidden: 0, best: null, worst: null },
        monthly: [],
        warnings: [],
        notes: NOTES.slice(),
        noTrades: "",
        thin: false,
      };
    }

    const returnPct = num(raw.return_pct);
    const hold = maybe(raw.buy_and_hold_pct);
    const netPnl = num(raw.net_pnl);
    const withoutBestTwo = num(raw.profit_without_best_two);
    const worst = raw.worst_month;
    const trades = num(raw.trades);
    const below = maybe(raw.thin_sample_below);

    const view = {
      present: true,
      head: {
        symbol: text(raw.symbol),
        interval: text(raw.interval),
        strategy: text(raw.strategy),
        exchange: text(raw.exchange),
        bars: num(raw.bars),
        start: text(raw.start),
        end: text(raw.end),
      },
      result: {
        startEquity: num(raw.starting_equity),
        finalEquity: num(raw.final_equity),
        netPnl,
        returnPct,
        cagrPct: maybe(raw.cagr_pct),
        feesPaid: num(raw.fees_paid),
      },
      hold: {
        bot: returnPct,
        hold,
        winner: hold === null ? null : returnPct > hold ? "bot" : "hold",
      },
      risk: {
        maxDrawdown: num(raw.max_drawdown),
        maxDrawdownPct: num(raw.max_drawdown_pct),
        daysUnderwater: num(raw.days_underwater),
        barsUnderwater: num(raw.bars_underwater),
        worstMonth: worst
          ? { month: text(worst.month), returnPct: num(worst.return_pct) }
          : null,
        longestLosingStreak: num(raw.longest_losing_streak),
        exposurePct: num(raw.exposure_pct),
      },
      quality: {
        trades,
        wins: num(raw.wins),
        losses: num(raw.losses),
        winRatePct: num(raw.win_rate_pct),
        winLossRatio: maybe(raw.win_loss_ratio),
        profitFactor: maybe(raw.profit_factor),
        expectancyR: num(raw.expectancy_r),
        sharpe: maybe(raw.sharpe),
        sortino: maybe(raw.sortino),
        tradesPerYear: num(raw.trades_per_year),
        withoutBestTwo,
        // ผลรวมเป็นบวกแต่ตัดไม้ดีสุดสองไม้แล้วไม่เหลือ = ผลนี้ยืนบนโชคสองครั้ง
        restsOnTwo: netPnl > 0 && withoutBestTwo <= 0,
      },
      gate: gateOf(raw),
      curve: curveOf(raw, opts.curveLimit),
      trades: tradesOf(raw, opts.tradeLimit),
      monthly: ((raw && raw.monthly_returns) || []).map((row) => ({
        month: text(row.month),
        returnPct: num(row.return_pct),
      })),
      warnings: ((raw && raw.warnings) || []).map(text).filter(Boolean),
      notes: NOTES.slice(),
      noTrades: "",
      // เกณฑ์ "กลุ่มตัวอย่างบางเกินไป" มาจาก report.THIN_SAMPLE ไม่ใช่เลขที่เขียนซ้ำที่นี่
      thin: below !== null && trades < below,
    };

    if (view.thin) {
      view.notes.unshift(
        `${trades} ไม้น้อยเกินกว่าจะมีความหมาย ต่ำกว่าราวๆ ${below} ไม้ โชคเสียงดังกว่าความได้เปรียบ`
      );
    }

    const reason = text(raw.no_trade_reason);
    if (reason && NO_TRADE_TH[reason]) view.noTrades = NO_TRADE_TH[reason](view);
    else if (reason) view.noTrades = reason;

    return view;
  }

  return { EXIT_TH, NO_TRADE_TH, NOTES, derive, thin, curveOf, tradesOf, gateOf };
});
