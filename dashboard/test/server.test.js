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
function startServer(logs, port) {
  return new Promise((resolve, reject) => {
    const child = spawn(
      process.execPath,
      [SERVER, "--port", String(port), "--logs", logs, "--poll", "120"],
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
