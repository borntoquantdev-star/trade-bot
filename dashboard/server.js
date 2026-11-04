#!/usr/bin/env node
/* Dashboard ของบอท — เซิร์ฟเวอร์เล็กๆ ที่ไม่มี dependency
 *
 *   node dashboard/server.js                  แล้วเปิด http://127.0.0.1:4317
 *   node dashboard/server.js --port 5000
 *   node dashboard/server.js --logs ./logs --open
 *
 * มันไม่ได้คุยกับตลาดและไม่ได้สั่งอะไรบอททั้งนั้น หน้าที่เดียวคือ
 * "เฝ้าไฟล์ที่บอทเขียน แล้วส่งต่อให้หน้าเว็บแบบ realtime" ผ่าน SSE
 *
 * เหตุผลที่แยกเป็นโปรเซสคนละตัวกับบอท: บอทต้องไม่มีอะไรมาแย่งเวลาในลูปเทรด
 * และ dashboard ล่มต้องไม่ทำให้บอทล่ม
 */

"use strict";

const fs = require("node:fs");
const http = require("node:http");
const path = require("node:path");
const { spawn } = require("node:child_process");

const { readNew, readJson } = require("./lib/tail.js");
const Pipeline = require("./lib/pipeline.js");

const ROOT = path.resolve(__dirname);
const PUBLIC = path.join(ROOT, "public");
const PROJECT = path.resolve(ROOT, "..");

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".ico": "image/x-icon",
};

const HELP = `
Dashboard ของบอท — ดูการทำงานแบบ realtime

  node dashboard/server.js [ตัวเลือก]

  --port <n>     พอร์ต (ค่าตั้งต้น 4317)
  --host <ip>    ค่าตั้งต้น 127.0.0.1 (เปิดเฉพาะในเครื่อง)
  --logs <dir>   โฟลเดอร์ log ของบอท (ค่าตั้งต้น ./logs)
  --poll <ms>    ความถี่ตรวจไฟล์ (ค่าตั้งต้น 500)
  --open         เปิดเบราว์เซอร์ให้เลย
  --help

ต้องมีบอทเขียน log ไว้ก่อน ถ้ายังไม่มีอะไรเลยให้ลองรัน
  tradebot --dry-run --iterations 30
`;

function parseArgs(argv) {
  const opts = {
    port: 4317,
    host: "127.0.0.1",
    logs: path.join(PROJECT, "logs"),
    poll: 500,
    open: false,
    help: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = () => {
      const value = argv[i + 1];
      if (value === undefined) {
        console.error(`ตัวเลือก ${arg} ต้องมีค่าตามหลัง`);
        process.exit(2);
      }
      i += 1;
      return value;
    };
    if (arg === "--help" || arg === "-h") opts.help = true;
    else if (arg === "--open") opts.open = true;
    else if (arg === "--port") opts.port = Number(next());
    else if (arg === "--host") opts.host = next();
    else if (arg === "--logs") opts.logs = path.resolve(next());
    else if (arg === "--poll") opts.poll = Math.max(100, Number(next()));
    else {
      console.error(`ไม่รู้จักตัวเลือก ${arg} (ลอง --help)`);
      process.exit(2);
    }
  }
  if (!Number.isFinite(opts.port) || opts.port < 1 || opts.port > 65535) {
    console.error("--port ต้องเป็นเลขพอร์ตที่ใช้ได้");
    process.exit(2);
  }
  return opts;
}

/** เก็บสถานะล่าสุดไว้ในหน่วยความจำ แล้วส่ง diff ให้ client ที่ต่ออยู่ */
function createWatcher(opts) {
  const auditFile = path.join(opts.logs, "audit.jsonl");
  const stateFile = path.join(opts.logs, "state.json");
  const MAX_ROWS = 2000; // พอสำหรับกราฟและ log ไม่กินหน่วยความจำ

  let rows = [];
  let cursor = { offset: 0, partial: "" };
  let state = null;
  let stateStamp = "";
  const clients = new Set();

  function snapshot() {
    return Pipeline.derive({ rows, state });
  }

  function send(client, event, data) {
    client.write(`event: ${event}\n`);
    client.write(`data: ${JSON.stringify(data)}\n\n`);
  }

  function broadcast(event, data) {
    for (const client of clients) {
      try {
        send(client, event, data);
      } catch {
        clients.delete(client);
      }
    }
  }

  function poll() {
    let changed = false;

    const result = readNew(auditFile, cursor);
    cursor = result.cursor;
    if (result.reset) {
      rows = [];
      changed = true;
    }
    if (result.rows.length) {
      rows = rows.concat(result.rows);
      if (rows.length > MAX_ROWS) rows = rows.slice(-MAX_ROWS);
      changed = true;
    }

    try {
      const stamp = String(fs.statSync(stateFile).mtimeMs);
      if (stamp !== stateStamp) {
        stateStamp = stamp;
        state = readJson(stateFile);
        changed = true;
      }
    } catch {
      if (state !== null) {
        state = null;
        stateStamp = "";
        changed = true;
      }
    }

    if (changed) broadcast("snapshot", snapshot());
  }

  function addClient(res) {
    clients.add(res);
    send(res, "snapshot", snapshot());
    send(res, "hello", { logs: opts.logs, auditFile, stateFile });
  }

  return { poll, addClient, removeClient: (res) => clients.delete(res), snapshot, clients };
}

function serveStatic(req, res) {
  const rel = req.url === "/" ? "index.html" : decodeURIComponent(req.url.split("?")[0]).slice(1);
  // หน้าเว็บกับเซิร์ฟเวอร์ใช้ lib/pipeline.js ไฟล์เดียวกัน ไม่คัดลอกให้ซ้ำ
  const target = rel === "lib/pipeline.js"
    ? path.join(ROOT, "lib", "pipeline.js")
    : path.join(PUBLIC, rel);
  if (target === path.join(ROOT, "lib", "pipeline.js")) {
    fs.readFile(target, (err, data) => {
      if (err) { res.writeHead(404).end("ไม่พบไฟล์"); return; }
      res.writeHead(200, { "content-type": MIME[".js"], "cache-control": "no-store" });
      res.end(data);
    });
    return;
  }
  // กันการขอไฟล์ออกนอกโฟลเดอร์ public
  if (!target.startsWith(PUBLIC + path.sep) && target !== PUBLIC) {
    res.writeHead(403).end("forbidden");
    return;
  }
  fs.readFile(target, (err, data) => {
    if (err) {
      res.writeHead(404, { "content-type": "text/plain; charset=utf-8" }).end("ไม่พบไฟล์");
      return;
    }
    res.writeHead(200, {
      "content-type": MIME[path.extname(target)] || "application/octet-stream",
      "cache-control": "no-store",
    });
    res.end(data);
  });
}

function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (opts.help) {
    console.log(HELP);
    return;
  }

  const watcher = createWatcher(opts);
  const timer = setInterval(watcher.poll, opts.poll);
  timer.unref?.();
  watcher.poll();

  const server = http.createServer((req, res) => {
    if (req.url.startsWith("/api/stream")) {
      res.writeHead(200, {
        "content-type": "text/event-stream; charset=utf-8",
        "cache-control": "no-store",
        connection: "keep-alive",
      });
      watcher.addClient(res);
      const beat = setInterval(() => res.write(": ping\n\n"), 25000);
      req.on("close", () => {
        clearInterval(beat);
        watcher.removeClient(res);
      });
      return;
    }
    if (req.url.startsWith("/api/snapshot")) {
      res.writeHead(200, { "content-type": MIME[".json"], "cache-control": "no-store" });
      res.end(JSON.stringify(watcher.snapshot(), null, 2));
      return;
    }
    serveStatic(req, res);
  });

  server.on("error", (err) => {
    if (err.code === "EADDRINUSE") {
      console.error(`พอร์ต ${opts.port} มีคนใช้อยู่ — ลอง --port ${opts.port + 1}`);
      process.exit(1);
    }
    throw err;
  });

  server.listen(opts.port, opts.host, () => {
    const url = `http://${opts.host}:${opts.port}`;
    const exists = fs.existsSync(path.join(opts.logs, "audit.jsonl"));
    console.log(`
Dashboard พร้อมแล้ว   ${url}
เฝ้าไฟล์ที่            ${opts.logs}
${exists ? "" : "ยังไม่มี audit.jsonl — ลองรัน  tradebot --dry-run --iterations 30\n"}กด Ctrl+C เพื่อหยุด (ไม่กระทบบอทที่กำลังรันอยู่)
`);
    if (opts.open) {
      const cmd = process.platform === "win32" ? "cmd" : process.platform === "darwin" ? "open" : "xdg-open";
      const args = process.platform === "win32" ? ["/c", "start", "", url] : [url];
      try {
        spawn(cmd, args, { detached: true, stdio: "ignore" }).unref();
      } catch {
        /* เปิดไม่ได้ก็ไม่เป็นไร ผู้ใช้เปิดเองได้ */
      }
    }
  });

  const stop = () => {
    clearInterval(timer);
    server.close(() => process.exit(0));
  };
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);
}

if (require.main === module) main();

module.exports = { parseArgs, createWatcher };
