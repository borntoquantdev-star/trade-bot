/* เทสตัวแปลง audit trail เป็นภาพ pipeline
 *
 * แถวตัวอย่างข้างล่างคัดลอกรูปแบบมาจาก logs/audit.jsonl จริงที่ engine เขียน
 * ถ้ารูปแบบนั้นเปลี่ยน เทสชุดนี้ต้องตกก่อนที่ dashboard จะแสดงผิด
 */

"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const P = require("../lib/pipeline.js");

const idle = {
  at: "2026-11-04T06:50:09.000000+00:00",
  symbol: "BTC",
  price: "100000",
  equity: "25000",
};

const withSignal = {
  ...idle,
  at: "2026-11-04T06:50:10.000000+00:00",
  signal: { side: "sell", ref_price: "100000", stop_price: "101000.00", reason: "coin flip rolled 0.771" },
};

const approved = {
  ...withSignal,
  order: {
    client_order_id: "89c7ffc74d14687c635c3ab535fbec15",
    side: "sell",
    qty: "0.22926",
    limit_price: "99900",
    risk_amount: "249.996567000000",
  },
  receipt: { broker_order_id: "paper-89c7ff", simulated: true },
};

const refused = {
  ...withSignal,
  at: "2026-11-04T06:50:11.000000+00:00",
  reject: { code: "below_min_notional", detail: "order value 5 is below the exchange minimum 10" },
};

const holding = {
  ...idle,
  at: "2026-11-04T06:50:12.000000+00:00",
  position: { side: "sell", qty: "0.22926", entry_price: "99900" },
};

const state = {
  day: "2026-11-04",
  day_start_equity: "25000",
  peak_equity: "25500",
  halted: false,
  halt_reason: "",
  sent_order_ids: ["89c7ff"],
};

const stageOf = (view, id) => view.nodes.find((n) => n.id === id);

test("ยังไม่มีข้อมูล: ทุกขั้นเป็น idle และบอกให้รันบอท", () => {
  const view = P.derive({ rows: [], state: null });
  assert.equal(view.nodes.length, P.STAGES.length);
  assert.ok(view.nodes.every((n) => n.state === "idle"));
  assert.match(stageOf(view, "market").detail, /audit trail/);
  assert.equal(view.counts.ticks, 0);
});

test("มีราคาแล้ว ขั้นข้อมูลตลาดเป็น ready", () => {
  const view = P.derive({ rows: [idle], state });
  assert.equal(stageOf(view, "market").state, "ready");
  assert.equal(stageOf(view, "market").metric, "100000");
  assert.match(stageOf(view, "market").detail, /BTC/);
});

test("ไม่มีสัญญาณ: กลยุทธ์เป็น wip ไม่ใช่ error", () => {
  const view = P.derive({ rows: [idle, idle], state });
  assert.equal(stageOf(view, "strategy").state, "wip");
  assert.equal(stageOf(view, "gate").state, "idle");
});

test("ออเดอร์ผ่านด่าน: gate ready และ broker บอกว่าเป็น paper", () => {
  const view = P.derive({ rows: [idle, approved], state });
  assert.equal(stageOf(view, "strategy").state, "ready");
  assert.equal(stageOf(view, "gate").state, "ready");
  assert.equal(stageOf(view, "gate").metric, "0.22926");
  assert.equal(stageOf(view, "broker").state, "locked", "paper fill ต้องไม่ขึ้นเป็น live");
  assert.equal(stageOf(view, "broker").metric, "paper");
});

test("ออเดอร์จริง (ไม่ใช่ paper) broker เป็น ready", () => {
  const live = { ...approved, receipt: { broker_order_id: "77", simulated: false } };
  const view = P.derive({ rows: [live], state });
  assert.equal(stageOf(view, "broker").state, "ready");
  assert.equal(stageOf(view, "broker").metric, "live");
});

test("ถูกปฏิเสธ: gate เป็น blocked พร้อมคำอธิบายภาษาไทย", () => {
  const view = P.derive({ rows: [refused], state });
  const gate = stageOf(view, "gate");
  assert.equal(gate.state, "blocked");
  assert.equal(gate.metric, "below_min_notional");
  assert.match(gate.detail, /ขั้นต่ำ/);
  assert.equal(stageOf(view, "broker").state, "idle", "ถูกปฏิเสธแล้วต้องไม่ส่งออเดอร์");
});

test("reject code ทุกตัวของบอทมีคำแปล", () => {
  const codes = [
    "halted", "max_drawdown", "daily_loss_limit", "expired", "short_not_allowed",
    "invalid_stop", "slippage_exceeded", "already_in_position", "too_many_positions",
    "qty_rounds_to_zero", "below_min_size", "below_min_notional", "exceeds_exposure",
    "bad_instrument",
  ];
  assert.equal(codes.length, 14);
  for (const code of codes) {
    assert.ok(P.REJECT_TH[code], `ขาดคำแปลของ ${code}`);
  }
});

test("บอท halt: gate เป็น blocked และการ์ดบัญชีบอกว่าหยุด", () => {
  const halted = { ...state, halted: true, halt_reason: "daily_loss_limit: ..." };
  const view = P.derive({ rows: [idle], state: halted });
  assert.equal(stageOf(view, "gate").state, "blocked");
  assert.equal(view.account.halted, true);
  assert.match(view.account.haltReason, /daily_loss_limit/);
});

test("มีไม้เปิดอยู่: ขั้น position แสดงฝั่งและขนาด", () => {
  const view = P.derive({ rows: [holding], state });
  const pos = stageOf(view, "position");
  assert.equal(pos.state, "ready");
  assert.equal(pos.metric, "sell");
  assert.match(pos.detail, /0.22926/);
});

test("อ่านตลาดไม่ได้: ขั้นข้อมูลตลาดเป็น error", () => {
  const broken = { at: idle.at, symbol: "BTC", price: "0", equity: "0", error: "broker read failed: exchange down" };
  const view = P.derive({ rows: [broken], state });
  assert.equal(stageOf(view, "market").state, "error");
  assert.match(stageOf(view, "market").detail, /exchange down/);
});

test("กลยุทธ์ throw: ขั้นกลยุทธ์เป็น error ไม่ใช่ขั้นอื่น", () => {
  const broken = { ...idle, error: "strategy raised: RuntimeError('boom')" };
  const view = P.derive({ rows: [broken], state });
  assert.equal(stageOf(view, "strategy").state, "error");
  assert.equal(stageOf(view, "market").state, "ready");
});

test("บัญชี: กำไรวันนี้คิดจากทุนต้นวัน และ drawdown คิดจากจุดสูงสุด", () => {
  const richer = { ...idle, equity: "26000" };
  const view = P.derive({ rows: [richer], state });
  assert.equal(view.account.dayPnl, 1000);
  assert.equal(view.account.dayPnlPct, 4);
  // peak 25500 เทียบทุน 26000 -> ไม่ติดลบ
  assert.ok(view.account.drawdownPct <= 0);
  assert.equal(view.account.sentOrders, 1);
});

test("กราฟเอาเฉพาะแถวที่มีราคา และจำกัดจำนวนจุด", () => {
  const rows = [idle, { ...idle, price: "0", error: "broker read failed: x" }, { ...idle, price: "100500" }];
  const view = P.derive({ rows, state });
  assert.equal(view.chart.length, 2);
  assert.equal(view.chart[1].price, 100500);

  const many = Array.from({ length: 500 }, (_, i) => ({ ...idle, price: String(100000 + i) }));
  assert.equal(P.derive({ rows: many, state, chartLimit: 120 }).chart.length, 120);
});

test("log ใหม่สุดอยู่บน และไม่แสดงรอบที่บอทไม่ทำอะไร", () => {
  const view = P.derive({ rows: [idle, approved, refused], state });
  assert.equal(view.log.length, 2, "แถวเฉยๆ ต้องไม่ขึ้น log");
  assert.equal(view.log[0].kind, "reject");
  assert.equal(view.log[1].kind, "order");
  assert.match(view.log[1].text, /paper/);
});

test("ออเดอร์ที่ไม่มีใบยืนยัน ขึ้น log ว่าไม่มีใบยืนยัน", () => {
  const noReceipt = { ...approved, receipt: undefined };
  const view = P.derive({ rows: [noReceipt], state });
  assert.match(view.log[0].text, /ไม่มีใบยืนยัน/);
  assert.equal(stageOf(view, "broker").state, "wip");
});

test("นับผลรวมได้ถูก", () => {
  const view = P.derive({ rows: [idle, approved, refused, refused], state });
  assert.equal(view.counts.ticks, 4);
  assert.equal(view.counts.orders, 1);
  assert.equal(view.counts.rejects, 2);
  assert.equal(view.counts.byCode.below_min_notional, 2);
});

test("symbol มาจากแถวล่าสุดที่มีราคา", () => {
  const view = P.derive({ rows: [{ ...idle, symbol: "ETH" }], state });
  assert.equal(view.symbol, "ETH");
});

test("derive ไม่แก้ข้อมูลที่รับเข้ามา", () => {
  const rows = [JSON.parse(JSON.stringify(approved))];
  const copy = JSON.parse(JSON.stringify(rows));
  P.derive({ rows, state });
  assert.deepEqual(rows, copy);
});
