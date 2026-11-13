/* แคนวาสของ dashboard — ลาก/ซูม/ย้ายการ์ด แล้ววาดสถานะบอทลงไป
 *
 * ถอดแบบกลไกจาก board.js ของ Trade Bot Designer: กล้อง (x, y, scale) ที่ใส่เป็น
 * transform เดียวบน layer, การ์ดวางด้วย absolute position, เส้นต่อเป็น SVG ใน
 * layer เดียวกันจึงขยับตามกล้องเอง, ตำแหน่งการ์ดเก็บลง localStorage
 *
 * มีสองหน้าอยู่บนแคนวาสเดียวกัน สลับด้วยแท็บบนแถบบน:
 *   live      สิ่งที่บอทกำลังทำ อ่านจาก logs/audit.jsonl + state.json
 *   backtest  ผลการทดสอบย้อนหลัง อ่านจาก logs/backtest.json
 * แต่ละหน้าจำผังการ์ดกับตำแหน่งกล้องของตัวเองแยกกัน เพราะจัดจอไว้คนละแบบ
 *
 * ไม่มีไลบรารี ไม่มี build step  ตรรกะการแปลงข้อมูลอยู่ใน lib/ ทั้งหมด
 * (pipeline.js กับ backtest.js) ไฟล์นี้ทำแค่เรื่องการวาดกับการโต้ตอบ
 */

(function () {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const SVG = "http://www.w3.org/2000/svg";
  const LAYOUT_KEY = "tradebot.canvas.layout.v2";
  const CAMERA_KEY = "tradebot.canvas.camera.v2";
  const PAGE_KEY = "tradebot.canvas.page.v1";

  const STATE_TH = {
    idle: "รอ",
    wip: "กำลังทำ",
    ready: "พร้อม",
    blocked: "ถูกปฏิเสธ",
    locked: "จำลอง",
    error: "ผิดพลาด",
  };

  /* ---------- เครื่องมือจัดรูปตัวเลข ---------- */

  const fmt = (v, d) => {
    const n = Number(v);
    if (!Number.isFinite(n)) return "-";
    return n.toLocaleString("en-US", {
      minimumFractionDigits: d === undefined ? 0 : d,
      maximumFractionDigits: d === undefined ? 2 : d,
    });
  };
  const signed = (v, d) => (Number(v) >= 0 ? "+" : "") + fmt(v, d);
  /** null จาก lib/backtest.js แปลว่า "วัดไม่ได้" ไม่ใช่ศูนย์ ห้ามพิมพ์เป็น 0 */
  const measured = (v, d, suffix) =>
    v === null || v === undefined ? "วัดไม่ได้" : fmt(v, d) + (suffix || "");
  const pct = (v, d) => fmt(v, d === undefined ? 2 : d) + "%";
  const signedPct = (v, d) => signed(v, d === undefined ? 2 : d) + "%";
  const sign = (v) => (Number(v) > 0 ? "pos" : Number(v) < 0 ? "neg" : "");
  const clock = (iso) => {
    if (!iso) return "";
    const d = new Date(iso);
    return Number.isNaN(d.getTime())
      ? String(iso).slice(11, 19)
      : d.toLocaleTimeString("th-TH", { hour12: false });
  };
  /** วันที่แบบ ISO ไม่ต้องเดาว่าผู้ใช้อยากได้ พ.ศ. หรือ ค.ศ. */
  const day = (iso) => String(iso || "").slice(0, 10);

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

  const kpi = (k, value, note, cls) =>
    el(
      "div",
      { class: "kpi" },
      el("div", { class: "k", text: k }),
      el("div", { class: "v " + (cls || ""), text: value }),
      el("div", { class: "n", text: note || "" })
    );

  const rowsCard = (...pairs) => el("div", { class: "kpis" }, ...pairs);

  const hint = (...lines) =>
    el("div", { class: "hint-empty" }, ...lines.map((line) =>
      line.code ? el("code", { text: line.code }) : el("div", { text: line })
    ));

  /* ---------- หน้า live ---------- */

  const LIVE_STAGE_IDS = ["market", "strategy", "gate", "broker", "position"];

  const LIVE = {
    label: "live",
    layout: {
      market: { x: 60, y: 140, w: 290, h: 180 },
      strategy: { x: 392, y: 140, w: 290, h: 180 },
      gate: { x: 724, y: 140, w: 290, h: 180 },
      broker: { x: 1056, y: 140, w: 290, h: 180 },
      position: { x: 1388, y: 140, w: 290, h: 180 },
      chart: { x: 60, y: 400, w: 622, h: 300 },
      account: { x: 724, y: 400, w: 290, h: 300 },
      log: { x: 1056, y: 400, w: 622, h: 440 },
    },
    titles: { chart: "ราคาที่บอทเห็น", account: "บัญชี", log: "การตัดสินใจล่าสุด" },
    group: {
      ids: LIVE_STAGE_IDS,
      label: (v) =>
        v
          ? `PIPELINE · ${v.counts.ticks} รอบ · ${v.counts.orders} ออเดอร์ · ${v.counts.rejects} ปฏิเสธ`
          : "PIPELINE · สายการทำงานของบอท",
    },
    wires: [
      ["market", "strategy"],
      ["strategy", "gate"],
      ["gate", "broker"],
      ["broker", "position"],
      ["market", "chart"],
      ["account", "gate"],
      ["broker", "log"],
    ],
    paint: paintLive,
    chrome: chromeLive,
  };

  /* ---------- หน้า backtest ---------- */

  const BACKTEST = {
    label: "backtest",
    layout: {
      btResult: { x: 60, y: 140, w: 430, h: 360 },
      btHold: { x: 530, y: 140, w: 300, h: 250 },
      btRisk: { x: 870, y: 140, w: 320, h: 360 },
      btQuality: { x: 1230, y: 140, w: 320, h: 400 },
      btEquity: { x: 60, y: 540, w: 770, h: 330 },
      btTrades: { x: 870, y: 580, w: 680, h: 590 },
      btGate: { x: 60, y: 910, w: 370, h: 260 },
      btNotes: { x: 470, y: 910, w: 360, h: 260 },
    },
    titles: {
      btResult: "ผลการทดสอบ",
      btHold: "เทียบกับไม่ทำอะไร",
      btRisk: "ความเสี่ยงที่ต้องทน",
      btQuality: "คุณภาพของไม้",
      btEquity: "เส้นทุน",
      btTrades: "ไม้ที่ปิดแล้ว",
      btGate: "ด่านความเสี่ยงปฏิเสธอะไร",
      btNotes: "อ่านก่อนเชื่อผลนี้",
    },
    group: {
      ids: ["btResult", "btHold", "btRisk", "btQuality"],
      label: (v) =>
        v && v.present
          ? `สรุปผล · ${v.head.strategy} บน ${v.head.symbol} ${v.head.interval}`
          : "สรุปผล · ยังไม่มีผลการทดสอบ",
    },
    // ทางที่ตัวเลขเดิน: ด่าน -> ไม้ -> เส้นทุน -> ผลลัพธ์ -> เทียบกับการอยู่เฉย
    wires: [
      ["btGate", "btTrades"],
      ["btTrades", "btEquity"],
      ["btEquity", "btResult"],
      ["btResult", "btHold"],
      ["btEquity", "btRisk"],
      ["btTrades", "btQuality"],
    ],
    paint: paintBacktest,
    chrome: chromeBacktest,
  };

  const PAGES = { live: LIVE, backtest: BACKTEST };

  /* ---------- สถานะหน้าจอ ---------- */

  let pageId = loadPage();
  let page = PAGES[pageId];
  let layout = loadLayout(pageId);
  let camera = loadCamera(pageId);
  const views = { live: null, backtest: null };
  let cardStates = {};
  let paused = false;
  let wiresOn = true;
  const cards = new Map(); // id -> { el, body, head }

  function loadPage() {
    try {
      const saved = localStorage.getItem(PAGE_KEY);
      if (saved && PAGES[saved]) return saved;
    } catch {
      /* ignore */
    }
    return "live";
  }

  function loadLayout(id) {
    const defaults = PAGES[id].layout;
    try {
      const saved = JSON.parse(localStorage.getItem(LAYOUT_KEY + "." + id));
      if (saved && typeof saved === "object") {
        const merged = {};
        for (const [key, box] of Object.entries(defaults)) {
          const s = saved[key];
          merged[key] =
            s && Number.isFinite(s.x) && Number.isFinite(s.y)
              ? { ...box, x: s.x, y: s.y, w: s.w || box.w, h: s.h || box.h }
              : { ...box };
        }
        return merged;
      }
    } catch {
      /* เก็บไม่ได้ก็ใช้ค่าเริ่มต้น */
    }
    return JSON.parse(JSON.stringify(defaults));
  }

  const saveLayout = () => {
    try {
      localStorage.setItem(LAYOUT_KEY + "." + pageId, JSON.stringify(layout));
    } catch {
      /* โหมดส่วนตัวบางแบบเขียนไม่ได้ */
    }
  };

  function loadCamera(id) {
    try {
      const saved = JSON.parse(localStorage.getItem(CAMERA_KEY + "." + id));
      if (saved && Number.isFinite(saved.x) && Number.isFinite(saved.scale)) return saved;
    } catch {
      /* ignore */
    }
    return { x: 40, y: 20, scale: 0.78 };
  }

  const saveCamera = () => {
    try {
      localStorage.setItem(CAMERA_KEY + "." + pageId, JSON.stringify(camera));
      localStorage.setItem(PAGE_KEY, pageId);
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

  function buildCards() {
    const host = $("nodes");
    host.replaceChildren();
    cards.clear();
    for (const id of Object.keys(page.layout)) {
      const head = el(
        "div",
        { class: "node-head" },
        el("span", { class: "tag" }),
        el("span", { class: "title", text: page.titles[id] || id }),
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

  /** กรอบ group ล้อมเฉพาะการ์ดที่หน้านั้นบอกว่าเป็นกลุ่มหลัก */
  function positionGroup() {
    const boxes = page.group.ids.map((id) => layout[id]);
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

  /** ตั้งหัวการ์ดกับสีขอบ แล้วจำสถานะไว้ให้มินิแมปและเส้นต่อใช้ */
  function setCard(id, state, stateText) {
    const card = cards.get(id);
    if (!card) return null;
    card.el.className = "node " + state;
    card.head.querySelector(".state").textContent =
      stateText === undefined ? STATE_TH[state] || state : stateText;
    cardStates[id] = state;
    return card;
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

    for (const [from, to] of page.wires) {
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

  /** เส้นวิ่งเฉพาะช่วงที่ข้อมูลเดินผ่านจริง */
  function isFlowing(from, to) {
    const a = cardStates[from];
    const b = cardStates[to];
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
      rect.setAttribute("class", "mm-node " + (cardStates[id] || ""));
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

  /* ---------- หน้า live: วาดเนื้อหา ---------- */

  function paintLive(v) {
    for (const node of v.nodes) {
      const card = setCard(node.id, node.state);
      if (!card) continue;
      card.head.querySelector(".title").textContent = node.label;
      card.body.replaceChildren(
        el("div", { class: "stage-metric", text: node.metric || "" }),
        el("div", { class: "stage-detail", text: node.detail }),
        el("div", { class: "stage-sub", text: node.sub })
      );
    }
    paintLiveAccount(v);
    paintLiveChart(v);
    paintLiveLog(v);
  }

  function paintLiveAccount(v) {
    const a = v.account;
    const card = setCard("account", a.halted ? "blocked" : "ready", a.halted ? "หยุดอยู่" : "ปกติ");
    card.body.replaceChildren(
      rowsCard(
        kpi("ทุน", fmt(a.equity, 2), a.day || ""),
        kpi("วันนี้", signed(a.dayPnl, 2), signedPct(a.dayPnlPct), sign(a.dayPnl)),
        kpi("ห่างจากพีค", pct(a.drawdownPct), "peak " + fmt(a.peak, 2)),
        kpi("รอบที่ตรวจ", fmt(v.counts.ticks), "สัญญาณ " + v.counts.signals),
        kpi("ออเดอร์", fmt(v.counts.orders), "ส่งแล้ว " + a.sentOrders),
        kpi("ถูกปฏิเสธ", fmt(v.counts.rejects), v.counts.errors ? "ผิดพลาด " + v.counts.errors : "")
      )
    );
  }

  function paintLiveChart(v) {
    const points = v.chart;
    const card = setCard("chart", points.length ? "ready" : "idle", points.length ? points.length + " จุด" : "รอ");

    if (!points.length) {
      card.body.replaceChildren(hint("ยังไม่มีราคา — บอทยังไม่ได้เขียน audit trail"));
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

  function paintLiveLog(v) {
    const card = setCard("log", v.log.length ? "ready" : "idle", v.log.length ? v.log.length + " รายการ" : "ว่าง");

    if (!v.log.length) {
      card.body.replaceChildren(
        hint(
          "ยังไม่มีการตัดสินใจที่น่าบันทึก",
          "ลองรันบอทแบบกระดาษ แล้วหน้านี้จะขยับเอง",
          { code: "tradebot --dry-run --paper-walk --iterations 60" }
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

  function chromeLive(v) {
    $("pillSymbol").textContent = v.symbol || "—";
    const broker = v.nodes.find((n) => n.id === "broker");
    const isPaper = !broker || broker.metric !== "live";
    const mode = $("pillMode");
    mode.className = "pill" + (isPaper ? "" : " live-real");
    mode.querySelector("span").textContent = isPaper ? "paper" : "LIVE";

    $("halt").hidden = !v.account.halted;
    $("haltReason").textContent = v.account.haltReason || "ไม่ได้บันทึกเหตุผล";
    $("statusLeft").textContent = v.updatedAt
      ? `อัปเดตล่าสุด ${clock(v.updatedAt)} · ${v.counts.ticks} รอบ`
      : "ยังไม่มีข้อมูลจากบอท";
  }

  /* ---------- หน้า backtest: วาดเนื้อหา ---------- */

  const EMPTY_HINT = [
    "ยังไม่มีไฟล์ผลการทดสอบ",
    "โหลดแท่งเทียนก่อน แล้วค่อยรันย้อนหลัง (ไม่ต้องมีคีย์ ไม่ต่อเน็ตตอนรัน)",
    { code: "tradebot --fetch-candles --days 365" },
    { code: "tradebot --backtest --days 365" },
  ];

  function paintBacktest(v) {
    if (!v.present) {
      for (const id of Object.keys(page.layout)) {
        const card = setCard(id, "idle", "ว่าง");
        card.body.replaceChildren(
          id === "btResult" ? hint(...EMPTY_HINT) : hint("รอผลการทดสอบ")
        );
      }
      return;
    }
    paintBtResult(v);
    paintBtHold(v);
    paintBtRisk(v);
    paintBtQuality(v);
    paintBtEquity(v);
    paintBtTrades(v);
    paintBtGate(v);
    paintBtNotes(v);
  }

  function paintBtResult(v) {
    const r = v.result;
    const state = r.netPnl > 0 ? "ready" : r.netPnl < 0 ? "blocked" : "idle";
    const card = setCard(
      "btResult",
      state,
      r.netPnl > 0 ? "กำไร" : r.netPnl < 0 ? "ขาดทุน" : "เท่าทุน"
    );
    card.body.replaceChildren(
      el(
        "div",
        { class: "bt-head" },
        el("span", { class: "who", text: `${v.head.strategy} · ${v.head.symbol} ${v.head.interval}` }),
        el("span", {
          class: "when",
          text: `${day(v.head.start)} → ${day(v.head.end)} · ${fmt(v.head.bars)} แท่ง` +
            (v.head.exchange ? ` · ${v.head.exchange}` : ""),
        })
      ),
      rowsCard(
        kpi("กำไรสุทธิ", signed(r.netPnl, 2), signedPct(r.returnPct), sign(r.netPnl)),
        kpi("ต่อปี", r.cagrPct === null ? "ช่วงสั้นเกินไป" : signedPct(r.cagrPct),
          r.cagrPct === null ? "ต้องยาวกว่าราวหนึ่งเดือน" : "ทบต้น", r.cagrPct === null ? "" : sign(r.cagrPct)),
        kpi("ทุนเริ่ม", fmt(r.startEquity, 2)),
        kpi("ทุนปลาย", fmt(r.finalEquity, 2)),
        kpi("ค่าธรรมเนียมที่จ่าย", fmt(r.feesPaid, 2),
          r.feesPaid > 0 && Math.abs(r.netPnl) > 0
            ? `เท่ากับ ${pct((r.feesPaid / Math.abs(r.netPnl)) * 100, 0)} ของผลลัพธ์`
            : ""),
        kpi("funding", "ไม่ถูกหัก", "ของจริงแพงกว่านี้")
      )
    );
  }

  function paintBtHold(v) {
    const h = v.hold;
    const state = h.winner === null ? "idle" : h.winner === "bot" ? "ready" : "blocked";
    const card = setCard(
      "btHold",
      state,
      h.winner === null ? "เทียบไม่ได้" : h.winner === "bot" ? "บอทชนะ" : "ซื้อถือชนะ"
    );
    card.body.replaceChildren(
      el(
        "div",
        { class: "versus" },
        el("div", { class: "vs-row" + (h.winner === "bot" ? " win" : "") },
          el("span", { class: "vs-k", text: "บอท" }),
          el("span", { class: "vs-v " + sign(h.bot), text: signedPct(h.bot) })),
        el("div", { class: "vs-row" + (h.winner === "hold" ? " win" : "") },
          el("span", { class: "vs-k", text: "ซื้อแล้วถือเฉยๆ" }),
          el("span", {
            class: "vs-v " + (h.hold === null ? "" : sign(h.hold)),
            text: h.hold === null ? "วัดไม่ได้" : signedPct(h.hold),
          }))
      ),
      el("div", {
        class: "stage-detail",
        text:
          h.winner === null
            ? "ไม่มีราคาต้นและปลายช่วงให้เทียบ"
            : h.winner === "bot"
              ? "บอทชนะการอยู่เฉย — ยังไม่ได้แปลว่ามันจะชนะช่วงอื่น"
              : "เครื่องจักรทั้งหมดนี้แพ้การนั่งเฉยๆ",
      })
    );
  }

  function paintBtRisk(v) {
    const r = v.risk;
    // ไม่ตั้งเพดานเอาเองว่า drawdown เท่าไรถึงเรียกว่าแย่ — เป็นข้อมูล ไม่ใช่คำตัดสิน
    const card = setCard("btRisk", v.gate.halted ? "blocked" : "wip", pct(r.maxDrawdownPct));
    card.body.replaceChildren(
      rowsCard(
        kpi("ขาดทุนจากพีคสูงสุด", fmt(r.maxDrawdown, 2), pct(r.maxDrawdownPct), "neg"),
        kpi("เวลาใต้พีค", fmt(r.daysUnderwater, 1) + " วัน", fmt(r.barsUnderwater) + " แท่ง"),
        kpi(
          "เดือนแย่สุด",
          r.worstMonth ? signedPct(r.worstMonth.returnPct) : "-",
          r.worstMonth ? r.worstMonth.month : "",
          r.worstMonth ? sign(r.worstMonth.returnPct) : ""
        ),
        kpi("แพ้ติดกันสูงสุด", fmt(r.longestLosingStreak) + " ไม้"),
        kpi("เวลาที่ถือของ", pct(r.exposurePct), "ของทุกแท่ง"),
        kpi("เดือนที่วัดได้", fmt(v.monthly.length))
      )
    );
  }

  function paintBtQuality(v) {
    const q = v.quality;
    const state =
      q.trades === 0 ? "idle" : v.thin ? "wip" : q.expectancyR > 0 ? "ready" : "blocked";
    const card = setCard(
      "btQuality",
      state,
      q.trades === 0 ? "ไม่มีไม้" : v.thin ? "ตัวอย่างบาง" : fmt(q.trades) + " ไม้"
    );

    if (q.trades === 0) {
      card.body.replaceChildren(
        hint(v.noTrades || "การทดสอบนี้ไม่ได้เปิดไม้เลย")
      );
      return;
    }

    card.body.replaceChildren(
      rowsCard(
        kpi("อัตราชนะ", pct(q.winRatePct), `${q.wins} ชนะ / ${q.losses} แพ้`),
        kpi("ชนะเฉลี่ย ÷ แพ้เฉลี่ย", measured(q.winLossRatio, 2), "อ่านคู่กับอัตราชนะเท่านั้น"),
        kpi("expectancy", signed(q.expectancyR, 2) + "R", "ต่อไม้", sign(q.expectancyR)),
        kpi("profit factor", measured(q.profitFactor, 2), "กำไรรวม ÷ ขาดทุนรวม"),
        kpi("sharpe / sortino", `${measured(q.sharpe, 2)} / ${measured(q.sortino, 2)}`),
        kpi("ไม้ต่อปี", fmt(q.tradesPerYear, 1)),
        kpi(
          "ตัดไม้ดีสุด 2 ไม้",
          signed(q.withoutBestTwo, 2),
          q.restsOnTwo ? "ผลนี้ยืนอยู่บนสองไม้" : "",
          q.restsOnTwo ? "neg" : sign(q.withoutBestTwo)
        ),
        kpi("ดีสุด / แย่สุด", `${signed(v.trades.best, 2)} / ${signed(v.trades.worst, 2)}`)
      )
    );
  }

  /** กราฟเส้นทุน พร้อมแรเงาช่วงที่อยู่ใต้พีคเดิม */
  function paintBtEquity(v) {
    const points = v.curve.points;
    const card = setCard(
      "btEquity",
      points.length ? "ready" : "idle",
      points.length ? `${fmt(v.curve.kept)} จาก ${fmt(v.curve.total)} จุด` : "ว่าง"
    );

    if (!points.length) {
      card.body.replaceChildren(hint("ไม่มีเส้นทุน — การทดสอบนี้ไม่มีแท่งให้เดิน"));
      return;
    }

    const w = 740;
    const h = 220;
    const pad = 10;
    const base = v.result.startEquity;
    const values = points.map((p) => p.equity);
    const min = Math.min(...values, base);
    const max = Math.max(...values, base);
    const span = max - min || 1;
    const X = (i) => (i / Math.max(1, points.length - 1)) * w;
    const Y = (val) => h - pad - ((val - min) / span) * (h - pad * 2);

    // พีคที่เดินไปข้างหน้า: ช่องว่างระหว่างพีคกับเส้นทุนคือ drawdown ที่ต้องนั่งทน
    const peaks = [];
    let running = -Infinity;
    for (const p of points) {
      running = Math.max(running, p.equity);
      peaks.push(running);
    }

    const line = points.map((p, i) => `${X(i).toFixed(1)},${Y(p.equity).toFixed(1)}`).join(" ");
    const down = v.result.finalEquity < base;

    const svg = document.createElementNS(SVG, "svg");
    svg.setAttribute("class", "spark equity" + (down ? " down" : ""));
    svg.setAttribute("viewBox", `0 0 ${w} ${h}`);
    svg.setAttribute("preserveAspectRatio", "none");

    // ขอบบนคือเส้นพีค ขอบล่างคือเส้นทุนย้อนกลับ ที่เหลือคือรูปของ drawdown
    const top = peaks.map((peak, i) => `${X(i).toFixed(1)},${Y(peak).toFixed(1)}`);
    const bottom = points
      .map((p, i) => `${X(i).toFixed(1)},${Y(p.equity).toFixed(1)}`)
      .reverse();
    const under = document.createElementNS(SVG, "polygon");
    under.setAttribute("class", "under");
    under.setAttribute("points", top.concat(bottom).join(" "));
    svg.append(under);

    const baseLine = document.createElementNS(SVG, "line");
    baseLine.setAttribute("class", "base");
    baseLine.setAttribute("x1", 0);
    baseLine.setAttribute("x2", w);
    baseLine.setAttribute("y1", Y(base).toFixed(1));
    baseLine.setAttribute("y2", Y(base).toFixed(1));
    svg.append(baseLine);

    const poly = document.createElementNS(SVG, "polyline");
    poly.setAttribute("class", "line");
    poly.setAttribute("points", line);
    svg.append(poly);

    const dot = document.createElementNS(SVG, "circle");
    dot.setAttribute("class", "mark");
    dot.setAttribute("cx", X(points.length - 1).toFixed(1));
    dot.setAttribute("cy", Y(points[points.length - 1].equity).toFixed(1));
    dot.setAttribute("r", 3.5);
    svg.append(dot);

    card.body.replaceChildren(
      el(
        "div",
        { class: "chart-top" },
        el("span", { class: "last " + sign(v.result.netPnl), text: fmt(v.result.finalEquity, 2) }),
        el("span", {
          class: "range",
          text: `ต่ำสุด ${fmt(v.curve.min, 2)} · สูงสุด ${fmt(v.curve.max, 2)} · เส้นประ = ทุนเริ่มต้น ${fmt(base, 2)}`,
        })
      ),
      svg,
      el("div", {
        class: "stage-sub",
        text: `แรเงาคือช่วงที่อยู่ใต้พีคเดิม · ${day(v.curve.from)} → ${day(v.curve.to)}`,
      })
    );
  }

  function paintBtTrades(v) {
    const t = v.trades;
    const card = setCard(
      "btTrades",
      t.rows.length ? "ready" : "idle",
      t.rows.length ? fmt(t.total) + " ไม้" : "ไม่มีไม้"
    );

    if (!t.rows.length) {
      card.body.replaceChildren(
        hint(
          v.noTrades || "การทดสอบนี้ไม่ได้เปิดไม้เลย",
          "ไม่ได้แปลว่าพัง — กลยุทธ์ที่เงียบทั้งปีก็เป็นคำตอบอย่างหนึ่ง"
        )
      );
      return;
    }

    card.body.replaceChildren(
      el(
        "div",
        { class: "feed" },
        ...t.rows.map((row) =>
          el(
            "div",
            { class: "trade " + (row.netPnl > 0 ? "win" : row.netPnl < 0 ? "loss" : "flat") },
            el(
              "div",
              { class: "t" },
              el("div", { text: day(row.entryAt) }),
              el("div", { class: "cd", text: clock(row.entryAt) })
            ),
            el(
              "div",
              {},
              el("div", { class: "ti", text: `${row.side} ${fmt(row.qty, 3)}` }),
              el("div", {
                class: "tx",
                text: `${fmt(row.entryPrice, 2)} → ${fmt(row.exitPrice, 2)} · ${row.exitLabel}`,
              })
            ),
            el("div", { class: "money " + sign(row.netPnl), text: signed(row.netPnl, 2) }),
            el("div", { class: "r " + sign(row.r), text: measured(row.r, 2) + "R" })
          )
        ),
        t.hidden
          ? el("div", { class: "stage-sub", text: `(เก่ากว่านี้อีก ${fmt(t.hidden)} ไม้ ไม่ขึ้นจอ)` })
          : null
      )
    );
  }

  function paintBtGate(v) {
    const g = v.gate;
    const state = g.halted ? "blocked" : g.rejectTotal ? "wip" : "idle";
    const card = setCard(
      "btGate",
      state,
      g.halted ? "หยุดกลางทาง" : g.rejectTotal ? fmt(g.rejectTotal) + " ครั้ง" : "ไม่ปฏิเสธเลย"
    );

    const kids = [
      el("div", {
        class: "stage-detail",
        text: `กลยุทธ์ออกสัญญาณ ${fmt(g.signals)} ครั้ง · ด่านปฏิเสธ ${fmt(g.rejectTotal)} ครั้ง`,
      }),
    ];

    if (g.rejects.length) {
      const top = g.rejects[0].count || 1;
      kids.push(
        el(
          "div",
          { class: "bars" },
          ...g.rejects.map((row) =>
            el(
              "div",
              { class: "bar-row" },
              el("span", { class: "bar-k", text: row.label }),
              el("span", { class: "bar-track" }, el("span", {
                class: "bar-fill",
                style: `width:${Math.max(3, (row.count / top) * 100)}%`,
              })),
              el("span", { class: "bar-v", text: fmt(row.count) })
            )
          )
        )
      );
    }

    if (g.unfilled) {
      kids.push(
        el("div", {
          class: "stage-detail",
          text: `${fmt(g.unfilled)} ออเดอร์ผ่านด่านแล้ว แต่ราคาไม่กลับมาที่ limit จึงไม่ได้ไม้`,
        })
      );
    }

    if (g.halted) {
      kids.push(
        el(
          "div",
          { class: "halt-inline" },
          el("div", { class: "ti", text: "บอทจริงจะหยุดตรงนี้" }),
          el("div", { class: "tx", text: `${day(g.halted.at)} ${clock(g.halted.at)} · ${g.halted.reason}` })
        )
      );
    }

    card.body.replaceChildren(...kids);
  }

  function paintBtNotes(v) {
    const card = setCard("btNotes", "wip", v.thin ? "ตัวอย่างบาง" : "ข้อจำกัด");
    card.body.replaceChildren(
      el(
        "ul",
        { class: "notes" },
        ...v.warnings.map((w) => el("li", { class: "warn", text: w })),
        ...v.notes.map((note) => el("li", { text: note }))
      )
    );
  }

  function chromeBacktest(v) {
    $("pillSymbol").textContent = v.present ? v.head.symbol || "—" : "—";
    const mode = $("pillMode");
    mode.className = "pill";
    mode.querySelector("span").textContent = v.present
      ? `${v.head.interval} · ${v.head.strategy}`
      : "ไม่มีผล";
    // แบนเนอร์ halt เป็นเรื่องของบอทที่รันอยู่จริง ไม่ใช่ของการทดสอบ
    $("halt").hidden = true;
    $("statusLeft").textContent = v.present
      ? `ทดสอบ ${day(v.head.start)} → ${day(v.head.end)} · ${fmt(v.head.bars)} แท่ง · ${fmt(v.trades.total)} ไม้`
      : "ยังไม่มี logs/backtest.json — รัน tradebot --backtest ก่อน";
  }

  /* ---------- วาดทั้งหน้า ---------- */

  function render() {
    const v = views[pageId];
    cardStates = {};
    $("groupLabel").textContent = page.group.label(v);
    if (v) {
      page.paint(v);
      page.chrome(v);
    } else {
      for (const id of Object.keys(page.layout)) setCard(id, "idle", "รอ");
      $("statusLeft").textContent = "กำลังต่อ...";
    }
    drawWires();
    drawMinimap();
  }

  function switchPage(next) {
    if (!PAGES[next] || next === pageId) return;
    saveLayout();
    saveCamera();
    pageId = next;
    page = PAGES[next];
    layout = loadLayout(next);
    camera = loadCamera(next);
    for (const tab of $("tabs").querySelectorAll(".tab")) {
      tab.classList.toggle("on", tab.dataset.page === next);
    }
    buildCards();
    applyCamera();
    render();
    saveCamera();
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
    const take = (key) => (event) => {
      if (paused) return;
      let data;
      try {
        data = JSON.parse(event.data);
      } catch {
        return;
      }
      setLive(true, "ต่ออยู่");
      views[key] = data;
      // หน้าที่ไม่ได้เปิดอยู่ก็เก็บไว้ สลับแท็บแล้วไม่ต้องรอรอบถัดไป
      if (key === pageId) render();
    };
    source.addEventListener("snapshot", take("live"));
    source.addEventListener("backtest", take("backtest"));
    source.addEventListener("error", () => setLive(false, "หลุด — ต่อใหม่"));
  }

  /* ---------- ปุ่มและคีย์ลัด ---------- */

  function setupControls() {
    $("zoomIn").onclick = () => zoomAtCentre(1.15);
    $("zoomOut").onclick = () => zoomAtCentre(1 / 1.15);
    $("zoomFit").onclick = fit;
    $("toolFit").onclick = fit;

    for (const tab of $("tabs").querySelectorAll(".tab")) {
      tab.onclick = () => switchPage(tab.dataset.page);
    }

    $("toolReset").onclick = () => {
      layout = JSON.parse(JSON.stringify(page.layout));
      saveLayout();
      buildCards();
      render();
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
      if (event.key === "1") switchPage("live");
      if (event.key === "2") switchPage("backtest");
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

  for (const tab of $("tabs").querySelectorAll(".tab")) {
    tab.classList.toggle("on", tab.dataset.page === pageId);
  }
  buildCards();
  setupPan();
  setupControls();
  applyCamera();
  render();
  connect();
})();
