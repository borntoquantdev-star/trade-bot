/* แคนวาสของ dashboard — ลาก/ซูม/ย้ายการ์ด แล้ววาดสถานะบอทลงไป
 *
 * ถอดแบบกลไกจาก board.js ของ Trade Bot Designer: กล้อง (x, y, scale) ที่ใส่เป็น
 * transform เดียวบน layer, การ์ดวางด้วย absolute position, เส้นต่อเป็น SVG ใน
 * layer เดียวกันจึงขยับตามกล้องเอง, ตำแหน่งการ์ดเก็บลง localStorage
 *
 * ไม่มีไลบรารี ไม่มี build step  ตรรกะการแปลงข้อมูลอยู่ใน lib/pipeline.js ทั้งหมด
 * ไฟล์นี้ทำแค่เรื่องการวาดกับการโต้ตอบ
 */

(function () {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const SVG = "http://www.w3.org/2000/svg";
  const LAYOUT_KEY = "hyperbot.canvas.layout.v1";
  const CAMERA_KEY = "hyperbot.canvas.camera.v1";

  const STATE_TH = {
    idle: "รอ",
    wip: "กำลังทำ",
    ready: "พร้อม",
    blocked: "ถูกปฏิเสธ",
    locked: "จำลอง",
    error: "ผิดพลาด",
  };

  /* ---------- ผังเริ่มต้นของการ์ด ---------- */

  const DEFAULT_LAYOUT = {
    market: { x: 60, y: 140, w: 290, h: 180 },
    strategy: { x: 392, y: 140, w: 290, h: 180 },
    gate: { x: 724, y: 140, w: 290, h: 180 },
    broker: { x: 1056, y: 140, w: 290, h: 180 },
    position: { x: 1388, y: 140, w: 290, h: 180 },
    chart: { x: 60, y: 400, w: 622, h: 300 },
    account: { x: 724, y: 400, w: 290, h: 300 },
    log: { x: 1056, y: 400, w: 622, h: 440 },
  };

  // ขั้นในสายโซ่ (อยู่ในกรอบ group) กับการ์ดข้อมูลรอบๆ
  const STAGE_IDS = ["market", "strategy", "gate", "broker", "position"];
  const PANEL_TITLE = { chart: "ราคาที่บอทเห็น", account: "บัญชี", log: "การตัดสินใจล่าสุด" };

  const WIRES = [
    ["market", "strategy"],
    ["strategy", "gate"],
    ["gate", "broker"],
    ["broker", "position"],
    ["market", "chart"],
    ["account", "gate"],
    ["broker", "log"],
  ];

  /* ---------- สถานะหน้าจอ ---------- */

  let layout = loadLayout();
  let camera = loadCamera();
  let view = null;
  let paused = false;
  let wiresOn = true;
  const cards = new Map(); // id -> { el, body }

  function loadLayout() {
    try {
      const saved = JSON.parse(localStorage.getItem(LAYOUT_KEY));
      if (saved && typeof saved === "object") {
        const merged = {};
        for (const [id, box] of Object.entries(DEFAULT_LAYOUT)) {
          const s = saved[id];
          merged[id] =
            s && Number.isFinite(s.x) && Number.isFinite(s.y)
              ? { ...box, x: s.x, y: s.y, w: s.w || box.w, h: s.h || box.h }
              : { ...box };
        }
        return merged;
      }
    } catch {
      /* เก็บไม่ได้ก็ใช้ค่าเริ่มต้น */
    }
    return JSON.parse(JSON.stringify(DEFAULT_LAYOUT));
  }

  const saveLayout = () => {
    try {
      localStorage.setItem(LAYOUT_KEY, JSON.stringify(layout));
    } catch {
      /* โหมดส่วนตัวบางแบบเขียนไม่ได้ */
    }
  };

  function loadCamera() {
    try {
      const saved = JSON.parse(localStorage.getItem(CAMERA_KEY));
      if (saved && Number.isFinite(saved.x) && Number.isFinite(saved.scale)) return saved;
    } catch {
      /* ignore */
    }
    return { x: 40, y: 20, scale: 0.78 };
  }

  const saveCamera = () => {
    try {
      localStorage.setItem(CAMERA_KEY, JSON.stringify(camera));
    } catch {
      /* ignore */
    }
  };

  /* ---------- กล้อง ---------- */

  function applyCamera() {
    $("layer").style.transform = `translate(${camera.x}px, ${camera.y}px) scale(${camera.scale})`;
    $("zoomLevel").textContent = Math.round(camera.scale * 100) + "%";
    drawMinimap();
  }

  function zoomAt(factor, px, py) {
    const next = Math.min(2.2, Math.max(0.25, camera.scale * factor));
    const ratio = next / camera.scale;
    camera.x = px - (px - camera.x) * ratio;
    camera.y = py - (py - camera.y) * ratio;
    camera.scale = next;
    applyCamera();
    saveCamera();
  }

  function fit() {
    const box = contentBox();
    const area = $("canvas").getBoundingClientRect();
    const pad = 60;
    const scale = Math.min(
      (area.width - pad * 2) / box.w,
      (area.height - pad * 2) / box.h,
      1.4
    );
    camera.scale = Math.max(0.25, scale);
    camera.x = pad - box.x * camera.scale + (area.width - pad * 2 - box.w * camera.scale) / 2;
    camera.y = pad - box.y * camera.scale;
    applyCamera();
    saveCamera();
  }

  function contentBox() {
    const boxes = Object.values(layout);
    const x = Math.min(...boxes.map((b) => b.x));
    const y = Math.min(...boxes.map((b) => b.y));
    const r = Math.max(...boxes.map((b) => b.x + b.w));
    const b = Math.max(...boxes.map((b) => b.y + b.h));
    return { x: x - 30, y: y - 50, w: r - x + 60, h: b - y + 80 };
  }

  /* ---------- สร้างการ์ด ---------- */

  function el(tag, attrs, ...kids) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (k === "class") node.className = v;
      else if (k === "text") node.textContent = v;
      else if (v !== null && v !== undefined) node.setAttribute(k, v);
    }
    for (const kid of kids) if (kid) node.append(kid);
    return node;
  }

  function buildCards() {
    const host = $("nodes");
    host.replaceChildren();
    cards.clear();
    for (const id of Object.keys(DEFAULT_LAYOUT)) {
      const head = el(
        "div",
        { class: "node-head" },
        el("span", { class: "tag" }),
        el("span", { class: "title", text: PANEL_TITLE[id] || id }),
        el("span", { class: "state" })
      );
      const body = el("div", { class: "node-body" });
      const card = el("div", { class: "node idle", "data-id": id }, head, body);
      placeCard(card, layout[id]);
      head.addEventListener("pointerdown", (e) => startDragCard(e, id, card));
      host.append(card);
      cards.set(id, { el: card, body, head });
    }
    positionGroup();
  }

  function placeCard(card, box) {
    card.style.left = box.x + "px";
    card.style.top = box.y + "px";
    card.style.width = box.w + "px";
    card.style.height = box.h + "px";
  }

  /** กรอบ group ล้อมเฉพาะการ์ดในสายโซ่ */
  function positionGroup() {
    const boxes = STAGE_IDS.map((id) => layout[id]);
    const pad = 22;
    const x = Math.min(...boxes.map((b) => b.x)) - pad;
    const y = Math.min(...boxes.map((b) => b.y)) - pad;
    const r = Math.max(...boxes.map((b) => b.x + b.w)) + pad;
    const b = Math.max(...boxes.map((b) => b.y + b.h)) + pad;
    const group = $("group");
    group.style.left = x + "px";
    group.style.top = y + "px";
    group.style.width = r - x + "px";
    group.style.height = b - y + "px";
  }

  /* ---------- ลากการ์ด ---------- */

  function startDragCard(event, id, card) {
    if (event.button !== 0) return;
    event.stopPropagation();
    event.preventDefault();
    const start = { x: event.clientX, y: event.clientY };
    const origin = { x: layout[id].x, y: layout[id].y };
    card.setPointerCapture(event.pointerId);

    const move = (e) => {
      layout[id].x = Math.round(origin.x + (e.clientX - start.x) / camera.scale);
      layout[id].y = Math.round(origin.y + (e.clientY - start.y) / camera.scale);
      placeCard(card, layout[id]);
      positionGroup();
      drawWires();
      drawMinimap();
    };
    const up = () => {
      card.removeEventListener("pointermove", move);
      card.removeEventListener("pointerup", up);
      saveLayout();
    };
    card.addEventListener("pointermove", move);
    card.addEventListener("pointerup", up);
  }

  /* ---------- ลากพื้นที่ว่าง (pan) ---------- */

  function setupPan() {
    const canvas = $("canvas");
    canvas.addEventListener("pointerdown", (event) => {
      if (event.button !== 0 || event.target.closest(".node")) return;
      canvas.classList.add("panning");
      const start = { x: event.clientX, y: event.clientY };
      const origin = { x: camera.x, y: camera.y };
      canvas.setPointerCapture(event.pointerId);
      const move = (e) => {
        camera.x = origin.x + (e.clientX - start.x);
        camera.y = origin.y + (e.clientY - start.y);
        applyCamera();
      };
      const up = () => {
        canvas.classList.remove("panning");
        canvas.removeEventListener("pointermove", move);
        canvas.removeEventListener("pointerup", up);
        saveCamera();
      };
      canvas.addEventListener("pointermove", move);
      canvas.addEventListener("pointerup", up);
    });

    canvas.addEventListener(
      "wheel",
      (event) => {
        event.preventDefault();
        const rect = canvas.getBoundingClientRect();
        zoomAt(
          event.deltaY < 0 ? 1.12 : 1 / 1.12,
          event.clientX - rect.left,
          event.clientY - rect.top
        );
      },
      { passive: false }
    );
  }

  /* ---------- เส้นต่อ ---------- */

  function drawWires() {
    const svg = $("wires");
    svg.replaceChildren();
    if (!wiresOn) return;
    const box = contentBox();
    svg.setAttribute("width", box.x + box.w + 400);
    svg.setAttribute("height", box.y + box.h + 400);

    for (const [from, to] of WIRES) {
      const a = layout[from];
      const b = layout[to];
      const horizontal = Math.abs(b.y - a.y) < Math.abs(b.x - a.x);
      let x1, y1, x2, y2;
      if (horizontal) {
        const leftToRight = b.x >= a.x;
        x1 = leftToRight ? a.x + a.w : a.x;
        x2 = leftToRight ? b.x : b.x + b.w;
        y1 = a.y + a.h / 2;
        y2 = b.y + b.h / 2;
      } else {
        const topToBottom = b.y >= a.y;
        x1 = a.x + a.w / 2;
        x2 = b.x + b.w / 2;
        y1 = topToBottom ? a.y + a.h : a.y;
        y2 = topToBottom ? b.y : b.y + b.h;
      }
      const path = document.createElementNS(SVG, "path");
      const d = horizontal
        ? `M ${x1} ${y1} C ${(x1 + x2) / 2} ${y1}, ${(x1 + x2) / 2} ${y2}, ${x2} ${y2}`
        : `M ${x1} ${y1} C ${x1} ${(y1 + y2) / 2}, ${x2} ${(y1 + y2) / 2}, ${x2} ${y2}`;
      path.setAttribute("d", d);
      if (isFlowing(from, to)) path.setAttribute("class", "flow");
      svg.append(path);

      // หัวลูกศร
      const head = document.createElementNS(SVG, "path");
      head.setAttribute("class", "head");
      head.setAttribute(
        "d",
        horizontal
          ? `M ${x2 - Math.sign(x2 - x1) * 8} ${y2 - 5} L ${x2} ${y2} L ${x2 - Math.sign(x2 - x1) * 8} ${y2 + 5}`
          : `M ${x2 - 5} ${y2 - Math.sign(y2 - y1) * 8} L ${x2} ${y2} L ${x2 + 5} ${y2 - Math.sign(y2 - y1) * 8}`
      );
      svg.append(head);
    }
  }

  /** เส้นวิ่งเฉพาะช่วงที่ข้อมูลเพิ่งเดินผ่านจริงในรอบล่าสุด */
  function isFlowing(from, to) {
    if (!view) return false;
    const stateOf = (id) => (view.nodes.find((n) => n.id === id) || {}).state;
    const a = stateOf(from);
    const b = stateOf(to);
    if (!a) return false;
    return a === "ready" && b !== undefined && b !== "idle";
  }

  /* ---------- มินิแมป ---------- */

  function drawMinimap() {
    const svg = $("minimapSvg");
    svg.replaceChildren();
    const box = contentBox();
    svg.setAttribute("viewBox", `${box.x} ${box.y} ${box.w} ${box.h}`);

    for (const [id, b] of Object.entries(layout)) {
      const rect = document.createElementNS(SVG, "rect");
      const state = view ? (view.nodes.find((n) => n.id === id) || {}).state : null;
      rect.setAttribute("class", "mm-node " + (state || ""));
      rect.setAttribute("x", b.x);
      rect.setAttribute("y", b.y);
      rect.setAttribute("width", b.w);
      rect.setAttribute("height", b.h);
      rect.setAttribute("rx", 10);
      svg.append(rect);
    }

    const area = $("canvas").getBoundingClientRect();
    const viewRect = document.createElementNS(SVG, "rect");
    viewRect.setAttribute("class", "mm-view");
    viewRect.setAttribute("x", -camera.x / camera.scale);
    viewRect.setAttribute("y", -camera.y / camera.scale);
    viewRect.setAttribute("width", area.width / camera.scale);
    viewRect.setAttribute("height", area.height / camera.scale);
    svg.append(viewRect);
  }

  /* ---------- วาดเนื้อหาลงการ์ด ---------- */

  const fmt = (v, d) => {
    const n = Number(v);
    if (!Number.isFinite(n)) return "-";
    return n.toLocaleString("en-US", {
      minimumFractionDigits: d === undefined ? 0 : d,
      maximumFractionDigits: d === undefined ? 2 : d,
    });
  };
  const clock = (iso) => {
    if (!iso) return "";
    const d = new Date(iso);
    return Number.isNaN(d.getTime()) ? String(iso).slice(11, 19) : d.toLocaleTimeString("th-TH", { hour12: false });
  };

  function paintStages(v) {
    for (const node of v.nodes) {
      const card = cards.get(node.id);
      if (!card) continue;
      card.el.className = "node " + node.state;
      card.head.querySelector(".title").textContent = node.label;
      card.head.querySelector(".state").textContent = STATE_TH[node.state] || node.state;
      card.body.replaceChildren(
        el("div", { class: "stage-metric", text: node.metric || "" }),
        el("div", { class: "stage-detail", text: node.detail }),
        el("div", { class: "stage-sub", text: node.sub })
      );
    }
  }

  function paintAccount(v) {
    const a = v.account;
    const card = cards.get("account");
    card.el.className = "node " + (a.halted ? "blocked" : "ready");
    card.head.querySelector(".state").textContent = a.halted ? "หยุดอยู่" : "ปกติ";
    const kpi = (k, val, note, cls) =>
      el(
        "div",
        { class: "kpi" },
        el("div", { class: "k", text: k }),
        el("div", { class: "v " + (cls || ""), text: val }),
        el("div", { class: "n", text: note || "" })
      );
    card.body.replaceChildren(
      el(
        "div",
        { class: "kpis" },
        kpi("ทุน", fmt(a.equity, 2), a.day || ""),
        kpi(
          "วันนี้",
          (a.dayPnl >= 0 ? "+" : "") + fmt(a.dayPnl, 2),
          (a.dayPnlPct >= 0 ? "+" : "") + fmt(a.dayPnlPct, 2) + "%",
          a.dayPnl > 0 ? "pos" : a.dayPnl < 0 ? "neg" : ""
        ),
        kpi("ห่างจากพีค", fmt(a.drawdownPct, 2) + "%", "peak " + fmt(a.peak, 2)),
        kpi("รอบที่ตรวจ", fmt(v.counts.ticks), "สัญญาณ " + v.counts.signals),
        kpi("ออเดอร์", fmt(v.counts.orders), "ส่งแล้ว " + a.sentOrders),
        kpi("ถูกปฏิเสธ", fmt(v.counts.rejects), v.counts.errors ? "ผิดพลาด " + v.counts.errors : "")
      )
    );
  }

  function paintChart(v) {
    const card = cards.get("chart");
    const points = v.chart;
    card.el.className = "node " + (points.length ? "ready" : "idle");
    card.head.querySelector(".state").textContent = points.length ? points.length + " จุด" : "รอ";

    if (!points.length) {
      card.body.replaceChildren(
        el("div", { class: "hint-empty", text: "ยังไม่มีราคา — บอทยังไม่ได้เขียน audit trail" })
      );
      return;
    }

    const w = 600;
    const h = 200;
    const pad = 8;
    const values = points.map((p) => p.price);
    const min = Math.min(...values);
    const max = Math.max(...values);
    const span = max - min || 1;
    const X = (i) => (i / Math.max(1, points.length - 1)) * w;
    const Y = (val) => h - pad - ((val - min) / span) * (h - pad * 2);
    const line = points.map((p, i) => `${X(i).toFixed(1)},${Y(p.price).toFixed(1)}`).join(" ");

    const svg = document.createElementNS(SVG, "svg");
    svg.setAttribute("class", "spark");
    svg.setAttribute("viewBox", `0 0 ${w} ${h}`);
    svg.setAttribute("preserveAspectRatio", "none");

    const area = document.createElementNS(SVG, "polygon");
    area.setAttribute("class", "area");
    area.setAttribute("points", `0,${h} ${line} ${w},${h}`);
    svg.append(area);

    const poly = document.createElementNS(SVG, "polyline");
    poly.setAttribute("class", "line");
    poly.setAttribute("points", line);
    svg.append(poly);

    const dot = document.createElementNS(SVG, "circle");
    dot.setAttribute("class", "mark");
    dot.setAttribute("cx", X(points.length - 1).toFixed(1));
    dot.setAttribute("cy", Y(points[points.length - 1].price).toFixed(1));
    dot.setAttribute("r", 3.5);
    svg.append(dot);

    card.body.replaceChildren(
      el(
        "div",
        { class: "chart-top" },
        el("span", { class: "last", text: fmt(points[points.length - 1].price, 2) }),
        el("span", { class: "range", text: `ต่ำสุด ${fmt(min, 2)} · สูงสุด ${fmt(max, 2)}` })
      ),
      svg
    );
  }

  function paintLog(v) {
    const card = cards.get("log");
    card.el.className = "node " + (v.log.length ? "ready" : "idle");
    card.head.querySelector(".state").textContent = v.log.length ? v.log.length + " รายการ" : "ว่าง";

    if (!v.log.length) {
      card.body.replaceChildren(
        el(
          "div",
          { class: "hint-empty" },
          el("div", { text: "ยังไม่มีการตัดสินใจที่น่าบันทึก" }),
          el("div", { text: "ลองรันบอทแบบกระดาษ แล้วหน้านี้จะขยับเอง" }),
          el("code", { text: "hyperbot --dry-run --paper-walk --iterations 60" })
        )
      );
      return;
    }

    card.body.replaceChildren(
      el(
        "div",
        { class: "feed" },
        ...v.log.map((row) =>
          el(
            "div",
            { class: "entry " + row.kind },
            el("div", { class: "t", text: clock(row.at) }),
            el(
              "div",
              {},
              el("div", { class: "ti", text: row.title }),
              row.text ? el("div", { class: "tx", text: row.text }) : null,
              row.code ? el("div", { class: "cd", text: row.code }) : null
            )
          )
        )
      )
    );
  }

  function paint(v) {
    view = v;
    $("pillSymbol").textContent = v.symbol || "—";

    const simulated = v.nodes.find((n) => n.id === "broker");
    const isPaper = !simulated || simulated.metric !== "live";
    const mode = $("pillMode");
    mode.className = "pill" + (isPaper ? "" : " live-real");
    mode.querySelector("span").textContent = isPaper ? "paper" : "LIVE";

    $("halt").hidden = !v.account.halted;
    $("haltReason").textContent = v.account.haltReason || "ไม่ได้บันทึกเหตุผล";
    $("groupLabel").textContent =
      `PIPELINE · ${v.counts.ticks} รอบ · ${v.counts.orders} ออเดอร์ · ${v.counts.rejects} ปฏิเสธ`;
    $("statusLeft").textContent = v.updatedAt
      ? `อัปเดตล่าสุด ${clock(v.updatedAt)} · ${v.counts.ticks} รอบ`
      : "ยังไม่มีข้อมูลจากบอท";

    paintStages(v);
    paintAccount(v);
    paintChart(v);
    paintLog(v);
    drawWires();
    drawMinimap();
  }

  /* ---------- ต่อ SSE ---------- */

  function setLive(on, text) {
    const pill = $("pillLive");
    pill.className = "pill " + (on ? "on" : "off");
    pill.querySelector("span").textContent = text;
  }

  function connect() {
    const source = new EventSource("/api/stream");
    source.addEventListener("open", () => setLive(true, "ต่ออยู่"));
    source.addEventListener("snapshot", (event) => {
      if (paused) return;
      let data;
      try {
        data = JSON.parse(event.data);
      } catch {
        return;
      }
      setLive(true, "ต่ออยู่");
      paint(data);
    });
    source.addEventListener("error", () => setLive(false, "หลุด — ต่อใหม่"));
  }

  /* ---------- ปุ่มและคีย์ลัด ---------- */

  function setupControls() {
    $("zoomIn").onclick = () => zoomAtCentre(1.15);
    $("zoomOut").onclick = () => zoomAtCentre(1 / 1.15);
    $("zoomFit").onclick = fit;
    $("toolFit").onclick = fit;

    $("toolReset").onclick = () => {
      layout = JSON.parse(JSON.stringify(DEFAULT_LAYOUT));
      saveLayout();
      buildCards();
      if (view) paint(view);
      fit();
    };

    $("toolWires").onclick = () => {
      wiresOn = !wiresOn;
      $("toolWires").classList.toggle("on", wiresOn);
      $("wires").classList.toggle("hidden", !wiresOn);
      drawWires();
    };
    $("toolWires").classList.add("on");

    $("toolPause").onclick = () => {
      paused = !paused;
      $("toolPause").classList.toggle("on", paused);
      $("toolPause").textContent = paused ? "▶" : "⏸";
      setLive(!paused, paused ? "หยุดดูไว้" : "ต่ออยู่");
    };

    window.addEventListener("keydown", (event) => {
      if (event.target.matches("input, textarea")) return;
      if (event.key === "f" || event.key === "F") fit();
      if (event.key === "w" || event.key === "W") $("toolWires").click();
      if (event.key === " ") {
        event.preventDefault();
        $("toolPause").click();
      }
    });

    window.addEventListener("resize", () => {
      drawMinimap();
    });
  }

  function zoomAtCentre(factor) {
    const area = $("canvas").getBoundingClientRect();
    zoomAt(factor, area.width / 2, area.height / 2);
  }

  /* ---------- เริ่มทำงาน ---------- */

  buildCards();
  setupPan();
  setupControls();
  applyCamera();
  drawWires();
  connect();
})();
