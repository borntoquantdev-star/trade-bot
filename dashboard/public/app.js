/* หน้าเว็บของ dashboard — วาด node graph, กราฟราคา และ log การตัดสินใจ
 *
 * ไม่มีไลบรารี ไม่มี build step: เปิดไฟล์แล้วทำงานเลย เหมือนโปรเจค Trade Bot
 * Designer ที่ถอดแบบมา  ตรรกะการแปลงข้อมูลทั้งหมดอยู่ใน lib/pipeline.js
 * ไฟล์นี้ทำแค่เรื่องการวาด
 */

(function () {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const SVG = "http://www.w3.org/2000/svg";

  const STATE_TH = {
    idle: "รอ",
    wip: "กำลังทำ",
    ready: "พร้อม",
    blocked: "ถูกปฏิเสธ",
    locked: "จำลอง",
    error: "ผิดพลาด",
  };

  const fmt = (value, digits) => {
    const n = Number(value);
    if (!Number.isFinite(n)) return "-";
    return n.toLocaleString("en-US", {
      minimumFractionDigits: digits === undefined ? 0 : digits,
      maximumFractionDigits: digits === undefined ? 2 : digits,
    });
  };
  const clock = (iso) => {
    if (!iso) return "";
    const d = new Date(iso);
    return Number.isNaN(d.getTime()) ? iso.slice(11, 19) : d.toLocaleTimeString("th-TH", { hour12: false });
  };

  function el(tag, attrs, ...kids) {
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(attrs || {})) {
      if (key === "class") node.className = value;
      else if (key === "text") node.textContent = value;
      else if (value !== null && value !== undefined) node.setAttribute(key, value);
    }
    for (const kid of kids) if (kid) node.append(kid);
    return node;
  }

  // ---------- การ์ดสรุปบัญชี ----------

  function paintCards(view) {
    const a = view.account;
    const pnlClass = a.dayPnl > 0 ? "v pos" : a.dayPnl < 0 ? "v neg" : "v";
    const cards = [
      { k: "ทุนปัจจุบัน", v: fmt(a.equity, 2), n: a.day ? `วัน ${a.day}` : "" },
      {
        k: "กำไร/ขาดทุนวันนี้",
        v: `${a.dayPnl >= 0 ? "+" : ""}${fmt(a.dayPnl, 2)}`,
        n: `${a.dayPnlPct >= 0 ? "+" : ""}${fmt(a.dayPnlPct, 2)}%`,
        cls: pnlClass,
      },
      { k: "ห่างจากจุดสูงสุด", v: `${fmt(a.drawdownPct, 2)}%`, n: `peak ${fmt(a.peak, 2)}` },
      { k: "รอบที่ตรวจแล้ว", v: fmt(view.counts.ticks), n: `สัญญาณ ${view.counts.signals}` },
      { k: "ออเดอร์ที่ส่ง", v: fmt(view.counts.orders), n: `ถูกปฏิเสธ ${view.counts.rejects}` },
    ];
    $("cards").replaceChildren(
      ...cards.map((c) =>
        el(
          "div",
          { class: "card" },
          el("div", { class: "k", text: c.k }),
          el("div", { class: c.cls || "v", text: c.v }),
          el("div", { class: "n", text: c.n || "" })
        )
      )
    );
  }

  // ---------- สายโซ่ + เส้นต่อ ----------

  function paintChain(view) {
    const chain = $("chain");
    chain.replaceChildren(
      ...view.nodes.map((n) =>
        el(
          "div",
          { class: "node " + n.state, "data-stage": n.id },
          el(
            "div",
            { class: "head" },
            el("b", { text: n.label }),
            el("span", { class: "st", text: STATE_TH[n.state] || n.state })
          ),
          el("div", { class: "sub", text: n.sub }),
          el("div", { class: "metric", text: n.metric || "" }),
          el("small", { text: n.detail })
        )
      )
    );
    requestAnimationFrame(() => paintWires(view));
  }

  /** เส้นประต่อระหว่างการ์ด วาดทับด้วย SVG เหมือน .board-wires ของโปรเจคต้นแบบ */
  function paintWires(view) {
    const svg = $("wires");
    const chain = $("chain");
    const cards = [...chain.querySelectorAll(".node")];
    svg.replaceChildren();
    if (cards.length < 2) return;

    const box = chain.getBoundingClientRect();
    svg.setAttribute("viewBox", `0 0 ${box.width} ${box.height}`);

    for (let i = 0; i < cards.length - 1; i += 1) {
      const a = cards[i].getBoundingClientRect();
      const b = cards[i + 1].getBoundingClientRect();
      const x1 = a.right - box.left;
      const x2 = b.left - box.left;
      const y = a.top - box.top + a.height / 2;
      const mid = (x1 + x2) / 2;
      const path = document.createElementNS(SVG, "path");
      path.setAttribute("d", `M ${x1} ${y} C ${mid} ${y}, ${mid} ${y}, ${x2} ${y}`);
      // ให้เส้นวิ่งเฉพาะช่วงที่ข้อมูลเดินผ่านจริงในรอบนี้
      const flowing = view.nodes[i].state === "ready" && view.nodes[i + 1].state !== "idle";
      if (flowing) path.setAttribute("class", "flow");
      svg.append(path);

      const head = document.createElementNS(SVG, "path");
      head.setAttribute("d", `M ${x2 - 7} ${y - 4} L ${x2} ${y} L ${x2 - 7} ${y + 4}`);
      head.setAttribute("style", "stroke-dasharray:none");
      svg.append(head);
    }
  }

  // ---------- กราฟราคา ----------

  function paintChart(view) {
    const svg = $("spark");
    const points = view.chart;
    svg.replaceChildren();
    if (!points.length) {
      $("chartLast").textContent = "-";
      $("chartRange").textContent = "ยังไม่มีราคา";
      return;
    }

    const w = 600;
    const h = 120;
    const pad = 6;
    const values = points.map((p) => p.price);
    const min = Math.min(...values);
    const max = Math.max(...values);
    const span = max - min || 1;
    const x = (i) => (i / Math.max(1, points.length - 1)) * w;
    const y = (v) => h - pad - ((v - min) / span) * (h - pad * 2);

    const line = points.map((p, i) => `${x(i).toFixed(1)},${y(p.price).toFixed(1)}`).join(" ");

    const area = document.createElementNS(SVG, "polygon");
    area.setAttribute("class", "area");
    area.setAttribute("points", `0,${h} ${line} ${w},${h}`);
    svg.append(area);

    const poly = document.createElementNS(SVG, "polyline");
    poly.setAttribute("class", "line");
    poly.setAttribute("points", line);
    svg.append(poly);

    const last = points[points.length - 1];
    const dot = document.createElementNS(SVG, "circle");
    dot.setAttribute("class", "mark");
    dot.setAttribute("cx", x(points.length - 1).toFixed(1));
    dot.setAttribute("cy", y(last.price).toFixed(1));
    dot.setAttribute("r", "3");
    svg.append(dot);

    $("chartLast").textContent = fmt(last.price, 2);
    $("chartRange").textContent = `${points.length} จุด · ต่ำสุด ${fmt(min, 2)} · สูงสุด ${fmt(max, 2)}`;
  }

  // ---------- log ----------

  function paintLog(view) {
    const log = $("log");
    $("empty").hidden = view.counts.ticks > 0;
    log.replaceChildren(
      ...view.log.map((row) =>
        el(
          "div",
          { class: "row " + row.kind },
          el("div", { class: "t", text: clock(row.at) }),
          el(
            "div",
            {},
            el("div", { class: "title", text: row.title }),
            row.text ? el("div", { class: "text", text: row.text }) : null,
            row.code ? el("div", { class: "code", text: row.code }) : null
          )
        )
      )
    );
  }

  // ---------- วาดทั้งหน้า ----------

  function paint(view) {
    $("symbol").textContent = view.symbol || "";
    const halted = view.account.halted;
    $("halt").hidden = !halted;
    $("haltReason").textContent = view.account.haltReason || "ไม่ได้บันทึกเหตุผล";
    paintCards(view);
    paintChain(view);
    paintChart(view);
    paintLog(view);
  }

  function setLive(on, text) {
    const box = $("live");
    box.className = "live " + (on ? "on" : "off");
    $("liveText").textContent = text;
  }

  // ---------- ต่อ SSE ----------

  let last = null;

  function connect() {
    const source = new EventSource("/api/stream");

    source.addEventListener("open", () => setLive(true, "ต่ออยู่"));
    source.addEventListener("snapshot", (event) => {
      try {
        last = JSON.parse(event.data);
      } catch {
        return;
      }
      setLive(true, last.updatedAt ? `อัปเดต ${clock(last.updatedAt)}` : "ต่ออยู่");
      paint(last);
    });
    source.addEventListener("error", () => {
      setLive(false, "หลุด — กำลังต่อใหม่");
      // EventSource ต่อใหม่เองอยู่แล้ว ไม่ต้องทำอะไรเพิ่ม
    });
  }

  window.addEventListener("resize", () => {
    if (last) paintWires(last);
  });

  connect();
})();
