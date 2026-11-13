/* เทสว่าเซิร์ฟเวอร์ส่งข้อมูลให้หน้าเว็บแบบ realtime จริง
 *
 * วิธีเทส: สร้างโฟลเดอร์ log ปลอม เปิดเซิร์ฟเวอร์บนพอร์ตว่าง ต่อ SSE
 * แล้วเขียนบรรทัดใหม่ลง audit.jsonl ระหว่างที่ต่ออยู่ — ต้องได้ snapshot
 * รอบใหม่ที่มีข้อมูลนั้นโดยไม่ต้องรีเฟรช
 *
 * ไม่แตะเน็ตข้างนอก ไม่ต้องมีบอทรันอยู่
 */

"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");
const { spawn } = require("node:child_process");

const SERVER = path.join(__dirname, "..", "server.js");

const ROW = {
  at: "2026-11-04T07:00:00.000000+00:00",
  symbol: "BTC",
  price: "100000",
  equity: "25000",
};

function freshLogs() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "tradebot-dash-srv-"));
  fs.writeFileSync(path.join(dir, "audit.jsonl"), JSON.stringify(ROW) + "\n", "utf8");
  fs.writeFileSync(
    path.join(dir, "state.json"),
    JSON.stringify({ day: "2026-11-04", day_start_equity: "25000", peak_equity: "25000" }),
    "utf8"
  );
  return dir;
}

/** เปิดเซิร์ฟเวอร์แล้วรอจนมันบอกว่าพร้อม คืน { port, stop } */
function startServer(logs, port, extra) {
  return new Promise((resolve, reject) => {
    const child = spawn(
      process.execPath,
      [SERVER, "--port", String(port), "--logs", logs, "--poll", "120"].concat(extra || []),
      { stdio: ["ignore", "pipe", "pipe"] }
    );
    let out = "";
    const timer = setTimeout(() => {
      child.kill();
      reject(new Error("เซิร์ฟเวอร์ไม่พร้อมในเวลาที่กำหนด: " + out));
    }, 10000);
    child.stdout.on("data", (chunk) => {
      out += chunk.toString();
      if (out.includes("Dashboard")) {
        clearTimeout(timer);
        resolve({
          port,
          stop: () =>
            new Promise((done) => {
              child.on("exit", done);
              child.kill();
            }),
        });
      }
    });
    child.stderr.on("data", (chunk) => {
      out += chunk.toString();
    });
    child.on("error", reject);
  });
}

function getJson(port, route) {
  return new Promise((resolve, reject) => {
    http
      .get({ host: "127.0.0.1", port, path: route }, (res) => {
        let body = "";
        res.on("data", (d) => (body += d));
        res.on("end", () => {
          try {
            resolve({ status: res.statusCode, json: JSON.parse(body) });
          } catch (err) {
            reject(err);
          }
        });
      })
      .on("error", reject);
  });
}

function getRaw(port, route) {
  return new Promise((resolve, reject) => {
    http
      .get({ host: "127.0.0.1", port, path: route }, (res) => {
        let body = "";
        res.on("data", (d) => (body += d));
        res.on("end", () => resolve({ status: res.statusCode, body, headers: res.headers }));
      })
      .on("error", reject);
  });
}

/** ต่อ SSE แล้วรอ snapshot ตามจำนวนที่ต้องการ */
function collectSnapshots(port, count, onFirst) {
  return new Promise((resolve, reject) => {
    const snapshots = [];
    const req = http.get({ host: "127.0.0.1", port, path: "/api/stream" }, (res) => {
      let buffer = "";
      res.on("data", (chunk) => {
        buffer += chunk.toString();
        let cut;
        while ((cut = buffer.indexOf("\n\n")) !== -1) {
          const frame = buffer.slice(0, cut);
          buffer = buffer.slice(cut + 2);
          const isSnapshot = frame.includes("event: snapshot");
          const dataLine = frame.split("\n").find((l) => l.startsWith("data: "));
          if (!isSnapshot || !dataLine) continue;
          snapshots.push(JSON.parse(dataLine.slice(6)));
          if (snapshots.length === 1 && onFirst) onFirst();
          if (snapshots.length >= count) {
            req.destroy();
            resolve(snapshots);
            return;
          }
        }
      });
      res.on("error", reject);
    });
    req.on("error", (err) => {
      if (err.code !== "ECONNRESET") reject(err);
    });
    setTimeout(() => {
      req.destroy();
      reject(new Error(`ได้ snapshot แค่ ${snapshots.length}/${count} ก่อนหมดเวลา`));
    }, 12000);
  });
}

test("หน้าเว็บโหลดได้ และ pipeline.js เสิร์ฟจากไฟล์เดียวกับเซิร์ฟเวอร์", async () => {
  const logs = freshLogs();
  const srv = await startServer(logs, 4391);
  try {
    const page = await getRaw(srv.port, "/");
    assert.equal(page.status, 200);
    assert.match(page.headers["content-type"], /text\/html/);
    assert.match(page.body, /สายการทำงานของบอท/);

    const lib = await getRaw(srv.port, "/lib/pipeline.js");
    assert.equal(lib.status, 200);
    assert.match(lib.headers["content-type"], /javascript/);
    assert.match(lib.body, /STAGES/);
  } finally {
    await srv.stop();
  }
});

test("ขอไฟล์ออกนอกโฟลเดอร์ public ไม่ได้", async () => {
  const logs = freshLogs();
  const srv = await startServer(logs, 4392);
  try {
    const res = await getRaw(srv.port, "/../server.js");
    assert.ok(res.status === 403 || res.status === 404, `ต้องไม่เสิร์ฟ แต่ได้ ${res.status}`);
  } finally {
    await srv.stop();
  }
});

test("/api/snapshot คืนภาพปัจจุบันของ pipeline", async () => {
  const logs = freshLogs();
  const srv = await startServer(logs, 4393);
  try {
    const { status, json } = await getJson(srv.port, "/api/snapshot");
    assert.equal(status, 200);
    assert.equal(json.symbol, "BTC");
    assert.equal(json.counts.ticks, 1);
    assert.equal(json.nodes.find((n) => n.id === "market").state, "ready");
  } finally {
    await srv.stop();
  }
});

test("บรรทัดใหม่ใน audit.jsonl ถึงหน้าเว็บทันทีผ่าน SSE", async () => {
  const logs = freshLogs();
  const srv = await startServer(logs, 4394);
  try {
    const refused = {
      ...ROW,
      at: "2026-11-04T07:00:05.000000+00:00",
      signal: { side: "buy", ref_price: "100000", stop_price: "99000", reason: "test" },
      reject: { code: "below_min_notional", detail: "order value 5 is below the exchange minimum 10" },
    };
    const snapshots = await collectSnapshots(srv.port, 2, () => {
      // เขียนหลังจากได้ snapshot แรกแล้ว เพื่อให้แน่ใจว่าเป็นการ push รอบใหม่จริง
      fs.appendFileSync(path.join(logs, "audit.jsonl"), JSON.stringify(refused) + "\n", "utf8");
    });

    assert.equal(snapshots[0].counts.ticks, 1);
    assert.equal(snapshots[1].counts.ticks, 2, "snapshot รอบสองต้องเห็นบรรทัดใหม่");
    const gate = snapshots[1].nodes.find((n) => n.id === "gate");
    assert.equal(gate.state, "blocked");
    assert.equal(gate.metric, "below_min_notional");
    assert.equal(snapshots[1].log[0].kind, "reject");
  } finally {
    await srv.stop();
  }
});

test("state.json ที่เปลี่ยนเป็น halted ถูกส่งต่อให้หน้าเว็บ", async () => {
  const logs = freshLogs();
  const srv = await startServer(logs, 4395);
  try {
    const snapshots = await collectSnapshots(srv.port, 2, () => {
      fs.writeFileSync(
        path.join(logs, "state.json"),
        JSON.stringify({
          day: "2026-11-04",
          day_start_equity: "25000",
          peak_equity: "25000",
          halted: true,
          halt_reason: "daily_loss_limit: ชนเพดาน",
        }),
        "utf8"
      );
    });
    assert.equal(snapshots[0].account.halted, false);
    assert.equal(snapshots[1].account.halted, true);
    assert.equal(snapshots[1].nodes.find((n) => n.id === "gate").state, "blocked");
  } finally {
    await srv.stop();
  }
});

test("โฟลเดอร์ log ที่ยังไม่มีไฟล์ ไม่ทำให้เซิร์ฟเวอร์ล่ม", async () => {
  const empty = fs.mkdtempSync(path.join(os.tmpdir(), "tradebot-dash-empty-"));
  const srv = await startServer(empty, 4396);
  try {
    const { json } = await getJson(srv.port, "/api/snapshot");
    assert.equal(json.counts.ticks, 0);
    assert.ok(json.nodes.every((n) => n.state === "idle"));
  } finally {
    await srv.stop();
  }
});

/** ผลการทดสอบย้อนหลังชิ้นเล็กที่สุดที่ยังเป็นผลจริง */
function backtestFile(dir, netPnl, curve) {
  const body = {
    symbol: "BTC",
    interval: "1h",
    strategy: "MaCrossStrategy",
    exchange: "grvt",
    start: "2025-01-01T00:00:00+00:00",
    end: "2025-03-01T00:00:00+00:00",
    bars: 1416,
    starting_equity: "10000",
    final_equity: String(10000 + netPnl),
    net_pnl: String(netPnl),
    return_pct: String((netPnl / 10000) * 100),
    fees_paid: "12",
    max_drawdown: "100",
    max_drawdown_pct: "1",
    cagr_pct: null,
    buy_and_hold_pct: "1.5",
    bars_underwater: 10,
    days_underwater: "0.4",
    worst_month: null,
    monthly_returns: [],
    sharpe: null,
    sortino: null,
    win_loss_ratio: "1.5",
    profit_without_best_two: "0",
    trades_per_year: "12",
    trades: 2,
    wins: 1,
    losses: 1,
    win_rate_pct: "50",
    profit_factor: "1.2",
    expectancy_r: "0.1",
    longest_losing_streak: 1,
    exposure_pct: "20",
    signals: 4,
    unfilled_orders: 0,
    rejects: { slippage_exceeded: 2 },
    halted_at: null,
    halt_reason: "",
    warnings: [],
    no_trade_reason: "",
    thin_sample_below: 30,
    equity_curve: (curve || [10000, 10000 + netPnl]).map((equity, i) => ({
      at: new Date(Date.UTC(2025, 0, 1 + i)).toISOString(),
      equity: String(equity),
    })),
    trade_list: [],
  };
  const target = path.join(dir, "backtest.json");
  fs.writeFileSync(target, JSON.stringify(body), "utf8");
  return target;
}

/** ต่อ SSE แล้วรอ event ชนิดที่ขอ ตามจำนวนที่ต้องการ */
function collectEvents(port, kind, count, onFirst) {
  return new Promise((resolve, reject) => {
    const seen = [];
    const req = http.get({ host: "127.0.0.1", port, path: "/api/stream" }, (res) => {
      let buffer = "";
      res.on("data", (chunk) => {
        buffer += chunk.toString();
        let cut;
        while ((cut = buffer.indexOf("\n\n")) !== -1) {
          const frame = buffer.slice(0, cut);
          buffer = buffer.slice(cut + 2);
          if (!frame.includes("event: " + kind)) continue;
          const dataLine = frame.split("\n").find((l) => l.startsWith("data: "));
          if (!dataLine) continue;
          seen.push(JSON.parse(dataLine.slice(6)));
          if (seen.length === 1 && onFirst) onFirst();
          if (seen.length >= count) {
            req.destroy();
            resolve(seen);
            return;
          }
        }
      });
      res.on("error", reject);
    });
    req.on("error", (err) => {
      if (err.code !== "ECONNRESET") reject(err);
    });
    setTimeout(() => {
      req.destroy();
      reject(new Error(`ได้ event ${kind} แค่ ${seen.length}/${count} ก่อนหมดเวลา`));
    }, 12000);
  });
}

test("/api/backtest บอกว่ายังไม่มีผล เมื่อไม่มีไฟล์", async () => {
  const logs = freshLogs();
  const srv = await startServer(logs, 4397);
  try {
    const { status, json } = await getJson(srv.port, "/api/backtest");
    assert.equal(status, 200);
    assert.equal(json.present, false);
    assert.equal(json.curve.points.length, 0);
    assert.ok(json.notes.length > 0, "ข้อจำกัดต้องขึ้นจอแม้ยังไม่มีผล");
  } finally {
    await srv.stop();
  }
});

test("/api/backtest คืนผลที่อ่านจาก backtest.json ในโฟลเดอร์ log", async () => {
  const logs = freshLogs();
  backtestFile(logs, 250);
  const srv = await startServer(logs, 4398);
  try {
    const { json } = await getJson(srv.port, "/api/backtest");
    assert.equal(json.present, true);
    assert.equal(json.head.strategy, "MaCrossStrategy");
    assert.equal(json.result.netPnl, 250);
    assert.equal(json.hold.winner, "bot");
    assert.equal(json.gate.rejects[0].code, "slippage_exceeded");
    assert.equal(json.thin, true, "2 ไม้ต้องถูกตั้งธงว่าตัวอย่างบาง");
  } finally {
    await srv.stop();
  }
});

test("--backtest-file ชี้ไปไฟล์นอกโฟลเดอร์ log ได้", async () => {
  const logs = freshLogs();
  const elsewhere = fs.mkdtempSync(path.join(os.tmpdir(), "tradebot-dash-bt-"));
  const file = backtestFile(elsewhere, 999);
  const srv = await startServer(logs, 4399, ["--backtest-file", file]);
  try {
    const { json } = await getJson(srv.port, "/api/backtest");
    assert.equal(json.result.netPnl, 999);
  } finally {
    await srv.stop();
  }
});

test("backtest.json ที่เขียนใหม่ระหว่างเปิดหน้าอยู่ ถึงหน้าเว็บทันทีผ่าน SSE", async () => {
  const logs = freshLogs();
  backtestFile(logs, -300);
  const srv = await startServer(logs, 4400);
  try {
    const events = await collectEvents(srv.port, "backtest", 2, () => {
      // รันใหม่ด้วยพารามิเตอร์อื่น แล้วเขียนทับไฟล์เดิม
      backtestFile(logs, 450);
    });
    assert.equal(events[0].result.netPnl, -300);
    assert.equal(events[0].hold.winner, "hold");
    assert.equal(events[1].result.netPnl, 450, "event รอบสองต้องเห็นผลรันใหม่");
    assert.equal(events[1].hold.winner, "bot");
  } finally {
    await srv.stop();
  }
});

test("เส้นทุนยาวถูกย่อก่อนออกจากเซิร์ฟเวอร์ ไม่ส่งทั้งก้อนให้เบราว์เซอร์", async () => {
  const logs = freshLogs();
  const curve = [];
  for (let i = 0; i < 20000; i += 1) curve.push(i === 9000 ? 6000 : 10000);
  backtestFile(logs, 0, curve);
  const srv = await startServer(logs, 4401);
  try {
    const { json } = await getJson(srv.port, "/api/backtest");
    assert.equal(json.curve.total, 20000);
    assert.ok(json.curve.kept < 2000, "ต้องย่อแล้ว แต่ได้ " + json.curve.kept + " จุด");
    assert.ok(
      json.curve.points.some((p) => p.equity === 6000),
      "หลุมที่ลึกสุดต้องรอดการย่อ"
    );
  } finally {
    await srv.stop();
  }
});

test("ไฟล์ที่ยังเขียนไม่จบ ไม่ทับผลที่ใช้ได้ด้วยคำว่าไม่มีผล", async () => {
  const logs = freshLogs();
  backtestFile(logs, 500);
  const srv = await startServer(logs, 4402);
  try {
    const first = await getJson(srv.port, "/api/backtest");
    assert.equal(first.json.result.netPnl, 500);

    // JSON ครึ่งๆ แบบที่เจอได้ตอนอ่านพร้อมกับที่ตัวรันกำลังเขียน
    fs.writeFileSync(path.join(logs, "backtest.json"), '{"symbol": "BTC", "equity_c', "utf8");
    await new Promise((done) => setTimeout(done, 500));
    const during = await getJson(srv.port, "/api/backtest");
    assert.equal(during.json.result.netPnl, 500, "ของเดิมที่ใช้ได้ต้องยังอยู่");

    backtestFile(logs, 700);
    await new Promise((done) => setTimeout(done, 500));
    const after = await getJson(srv.port, "/api/backtest");
    assert.equal(after.json.result.netPnl, 700, "เขียนจบแล้วต้องอ่านของใหม่ได้เอง");
  } finally {
    await srv.stop();
  }
});

test("lib/backtest.js เสิร์ฟจากไฟล์เดียวกับที่เซิร์ฟเวอร์ require", async () => {
  const logs = freshLogs();
  const srv = await startServer(logs, 4403);
  try {
    const lib = await getRaw(srv.port, "/lib/backtest.js");
    assert.equal(lib.status, 200);
    assert.match(lib.headers["content-type"], /javascript/);
    assert.match(lib.body, /NO_TRADE_TH/);

    const page = await getRaw(srv.port, "/");
    assert.match(page.body, /lib\/backtest\.js/, "หน้าเว็บต้องโหลดไฟล์นั้นจริง");
    assert.match(page.body, /data-page="backtest"/, "ต้องมีแท็บ backtest");
  } finally {
    await srv.stop();
  }
});
