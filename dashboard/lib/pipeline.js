/* แปลง audit trail ของบอทเป็นภาพ pipeline
 *
 * ฟังก์ชันในไฟล์นี้เป็น pure ทั้งหมด: รับข้อมูลเข้า คืนข้อมูลออก ไม่อ่านไฟล์
 * ไม่ต่อเน็ต ไม่แตะ DOM — เหมือน risk_gate ของตัวบอทเอง เพราะงั้นเทสได้ตรงๆ
 * และใช้ได้ทั้งใน Node (server) และในเบราว์เซอร์ (หน้าเว็บ)
 *
 * แหล่งข้อมูลคือสองไฟล์ที่บอทเขียนไว้แล้ว ไม่ต้องแก้ตัวบอท:
 *   logs/audit.jsonl   1 บรรทัด = 1 การตัดสินใจ
 *   logs/state.json    ทุนต้นวัน จุดสูงสุด สถานะ halt
 */

(function (root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.Pipeline = api;
})(typeof self !== "undefined" ? self : this, function () {
  "use strict";

  /** ขั้นของ pipeline เรียงตามทางที่ข้อมูลเดิน */
  const STAGES = [
    { id: "market", label: "ข้อมูลตลาด", sub: "ราคา · ทุน · position" },
    { id: "strategy", label: "กลยุทธ์", sub: "ออกสัญญาณหรือเงียบ" },
    { id: "gate", label: "ด่านความเสี่ยง", sub: "risk_gate()" },
    { id: "broker", label: "ส่งคำสั่ง", sub: "broker" },
    { id: "position", label: "ไม้ที่ถืออยู่", sub: "position" },
  ];

  /** คำอธิบาย reject code เป็นภาษาคน เรียงตามความร้ายแรงเหมือนในตัวบอท */
  const REJECT_TH = {
    halted: "บอทหยุดตัวเอง",
    max_drawdown: "ชนเพดานขาดทุนจากจุดสูงสุด",
    daily_loss_limit: "ชนเพดานขาดทุนรายวัน",
    expired: "สัญญาณหมดอายุ",
    short_not_allowed: "ตั้งห้าม short",
    invalid_stop: "stop วางผิด",
    slippage_exceeded: "ราคาวิ่งหนีเกินเพดาน",
    already_in_position: "มีไม้ค้างอยู่แล้ว",
    too_many_positions: "ไม้เต็มเพดาน",
    qty_rounds_to_zero: "ทุนน้อยเกินไปสำหรับ stop นี้",
    below_min_notional: "ออเดอร์เล็กกว่าขั้นต่ำของตลาด",
    exceeds_exposure: "เกินเพดาน exposure",
    bad_instrument: "สเปคสินค้าหรือราคาใช้ไม่ได้",
  };

  const num = (value) => {
    const n = Number(value);
    return Number.isFinite(n) ? n : 0;
  };

  /** บรรทัด audit ล่าสุดที่มีข้อมูลตลาด (ข้ามบรรทัดที่อ่านตลาดไม่ได้) */
  function lastWithPrice(rows) {
    for (let i = rows.length - 1; i >= 0; i -= 1) {
      if (num(rows[i].price) > 0) return rows[i];
    }
    return null;
  }

  /** นับผลการตัดสินใจทั้งหมด แยกตามชนิด */
  function tally(rows) {
    const out = { ticks: rows.length, signals: 0, orders: 0, rejects: 0, errors: 0, byCode: {} };
    for (const row of rows) {
      if (row.signal) out.signals += 1;
      if (row.order) out.orders += 1;
      if (row.error) out.errors += 1;
      if (row.reject) {
        out.rejects += 1;
        const code = row.reject.code || "unknown";
        out.byCode[code] = (out.byCode[code] || 0) + 1;
      }
    }
    return out;
  }

  function accountOf(rows, state) {
    const latest = lastWithPrice(rows);
    const equity = latest ? num(latest.equity) : num(state && state.day_start_equity);
    const dayStart = num(state && state.day_start_equity);
    const peak = num(state && state.peak_equity);
    const dayPnl = dayStart > 0 ? equity - dayStart : 0;
    return {
      equity,
      dayStart,
      peak,
      dayPnl,
      dayPnlPct: dayStart > 0 ? (dayPnl / dayStart) * 100 : 0,
      drawdownPct: peak > 0 ? ((peak - equity) / peak) * 100 : 0,
      day: (state && state.day) || "",
      halted: !!(state && state.halted),
      haltReason: (state && state.halt_reason) || "",
      sentOrders: ((state && state.sent_order_ids) || []).length,
    };
  }

  /** สถานะของแต่ละขั้น จากบรรทัดล่าสุดกับสถานะที่บอทเก็บไว้ */
  function nodesOf(rows, state) {
    const latest = rows.length ? rows[rows.length - 1] : null;
    const market = lastWithPrice(rows);
    const halted = !!(state && state.halted);
    const counts = tally(rows);

    const nodes = STAGES.map((stage) => ({
      id: stage.id,
      label: stage.label,
      sub: stage.sub,
      state: "idle",
      detail: "ยังไม่เริ่ม",
      metric: "",
    }));
    const at = (id) => nodes.find((n) => n.id === id);

    if (!latest) {
      at("market").detail = "รอบอทเขียน audit trail บรรทัดแรก";
      return nodes;
    }

    // 1) ข้อมูลตลาด
    const readFailed = latest.error && /broker read failed/i.test(latest.error);
    if (readFailed) {
      at("market").state = "error";
      at("market").detail = latest.error;
    } else if (market) {
      at("market").state = "ready";
      at("market").metric = market.price;
      at("market").detail = `${market.symbol || ""} ${market.price} · ทุน ${market.equity}`.trim();
    }

    // 2) กลยุทธ์
    if (latest.error && /strategy raised/i.test(latest.error)) {
      at("strategy").state = "error";
      at("strategy").detail = latest.error;
    } else if (latest.signal) {
      at("strategy").state = "ready";
      at("strategy").metric = latest.signal.side;
      at("strategy").detail = `${latest.signal.side} @ ${latest.signal.ref_price} · stop ${latest.signal.stop_price}`;
    } else if (counts.ticks) {
      at("strategy").state = "wip";
      at("strategy").detail = `ยังไม่ออกสัญญาณ (ออกไปแล้ว ${counts.signals} ครั้ง)`;
    }

    // 3) ด่านความเสี่ยง
    if (halted) {
      at("gate").state = "blocked";
      at("gate").detail = `หยุดอยู่: ${(state && state.halt_reason) || "ไม่ได้บันทึกเหตุผล"}`;
    } else if (latest.reject) {
      const code = latest.reject.code;
      at("gate").state = "blocked";
      at("gate").metric = code;
      at("gate").detail = `${REJECT_TH[code] || code} — ${latest.reject.detail || ""}`;
    } else if (latest.order) {
      at("gate").state = "ready";
      at("gate").metric = latest.order.qty;
      at("gate").detail = `อนุมัติ ${latest.order.qty} @ ${latest.order.limit_price} · เสี่ยง ${latest.order.risk_amount}`;
    } else if (latest.signal) {
      at("gate").state = "wip";
      at("gate").detail = "กำลังตรวจสัญญาณ";
    } else if (counts.ticks) {
      at("gate").state = "idle";
      at("gate").detail = `ไม่มีสัญญาณให้ตรวจ · ปฏิเสธไปแล้ว ${counts.rejects} ครั้ง`;
    }

    // 4) ส่งคำสั่ง
    if (latest.error && /broker/i.test(latest.error) && !readFailed) {
      at("broker").state = "error";
      at("broker").detail = latest.error;
    } else if (latest.receipt) {
      at("broker").state = latest.receipt.simulated ? "locked" : "ready";
      at("broker").metric = latest.receipt.simulated ? "paper" : "live";
      at("broker").detail = latest.receipt.simulated
        ? `จำลอง (paper) · id ${latest.receipt.broker_order_id}`
        : `ส่งจริงแล้ว · id ${latest.receipt.broker_order_id}`;
    } else if (latest.order) {
      at("broker").state = "wip";
      at("broker").detail = "ส่งแล้วแต่ยังไม่มีใบยืนยัน";
    } else if (counts.orders) {
      at("broker").state = "idle";
      at("broker").detail = `ส่งไปแล้วทั้งหมด ${counts.orders} ออเดอร์`;
    }

    // 5) ไม้ที่ถืออยู่
    const pos = latest.position;
    if (pos) {
      at("position").state = "ready";
      at("position").metric = pos.side;
      at("position").detail = `${pos.side} ${pos.qty} @ ${pos.entry_price}`;
    } else {
      at("position").state = "idle";
      at("position").detail = "ไม่มีไม้เปิดอยู่";
    }

    return nodes;
  }

  /** ราคาสำหรับกราฟ เอาเฉพาะบรรทัดที่อ่านตลาดได้ */
  function chartOf(rows, limit) {
    const points = [];
    for (const row of rows) {
      const price = num(row.price);
      if (price > 0) points.push({ at: row.at, price });
    }
    return limit && points.length > limit ? points.slice(-limit) : points;
  }

  /** บรรทัดสำหรับ log ด้านล่าง ใหม่สุดอยู่บน */
  function logOf(rows, limit) {
    const out = [];
    for (let i = rows.length - 1; i >= 0 && out.length < (limit || 60); i -= 1) {
      const row = rows[i];
      if (row.error) {
        out.push({ at: row.at, kind: "error", title: "ผิดพลาด", text: row.error });
      } else if (row.order) {
        out.push({
          at: row.at,
          kind: "order",
          title: `${row.order.side} ${row.order.qty}`,
          text: `@ ${row.order.limit_price} · เสี่ยง ${row.order.risk_amount}` +
            (row.receipt ? (row.receipt.simulated ? " · paper" : " · live") : " · ไม่มีใบยืนยัน"),
          code: row.order.client_order_id,
        });
      } else if (row.reject) {
        out.push({
          at: row.at,
          kind: "reject",
          title: REJECT_TH[row.reject.code] || row.reject.code,
          text: row.reject.detail || "",
          code: row.reject.code,
        });
      } else if (row.signal) {
        out.push({
          at: row.at,
          kind: "signal",
          title: `สัญญาณ ${row.signal.side}`,
          text: row.signal.reason || "",
        });
      }
      // บรรทัดที่ไม่มีอะไรเลย (บอทดูแล้วไม่ทำอะไร) ไม่ต้องขึ้น log
    }
    return out;
  }

  /** รวมทุกอย่างเป็นภาพเดียวให้หน้าเว็บวาด */
  function derive(input) {
    const rows = (input && input.rows) || [];
    const state = (input && input.state) || null;
    const latest = lastWithPrice(rows);
    return {
      symbol: (latest && latest.symbol) || (input && input.symbol) || "",
      updatedAt: rows.length ? rows[rows.length - 1].at : "",
      account: accountOf(rows, state),
      nodes: nodesOf(rows, state),
      chart: chartOf(rows, (input && input.chartLimit) || 240),
      log: logOf(rows, (input && input.logLimit) || 60),
      counts: tally(rows),
    };
  }

  return { STAGES, REJECT_TH, derive, nodesOf, accountOf, chartOf, logOf, tally };
});
