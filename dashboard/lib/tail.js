/* อ่านไฟล์ JSONL แบบต่อท้าย (tail) โดยจำตำแหน่งที่อ่านค้างไว้
 *
 * ตัวบอทเขียน logs/audit.jsonl แบบ append-only เราจึงอ่านเฉพาะส่วนที่เพิ่มมา
 * ไม่ต้องอ่านทั้งไฟล์ใหม่ทุกครั้ง
 *
 * กรณีที่ต้องรับมือ:
 *   - ไฟล์ยังไม่มี            → คืนค่าว่าง ไม่ throw
 *   - ไฟล์ถูกลบหรือเริ่มใหม่   → ขนาดเล็กลงกว่าตำแหน่งที่จำไว้ → อ่านใหม่ทั้งไฟล์
 *   - บรรทัดสุดท้ายยังเขียนไม่จบ → เก็บไว้รออ่านรอบหน้า ไม่ parse ครึ่งบรรทัด
 *   - บรรทัดที่ parse ไม่ได้    → ข้าม แล้วนับไว้ ไม่ทำให้ทั้งไฟล์ใช้ไม่ได้
 */

"use strict";

const fs = require("node:fs");

/**
 * @param {string} file     path ของไฟล์ JSONL
 * @param {object} cursor   { offset, partial } จากรอบก่อน (ครั้งแรกส่ง {} ได้)
 * @returns {{rows: object[], cursor: object, reset: boolean, missing: boolean, bad: number}}
 */
function readNew(file, cursor) {
  const prev = { offset: 0, partial: "", ...(cursor || {}) };
  let stat;
  try {
    stat = fs.statSync(file);
  } catch {
    // ไฟล์ยังไม่มี: บอทอาจยังไม่ได้รัน
    return { rows: [], cursor: { offset: 0, partial: "" }, reset: false, missing: true, bad: 0 };
  }

  let offset = prev.offset;
  let partial = prev.partial;
  let reset = false;
  if (stat.size < offset) {
    // ไฟล์สั้นลง = ถูกลบหรือเริ่มรอบใหม่ อ่านใหม่ทั้งไฟล์
    offset = 0;
    partial = "";
    reset = true;
  }
  if (stat.size === offset) {
    return { rows: [], cursor: { offset, partial }, reset, missing: false, bad: 0 };
  }

  const length = stat.size - offset;
  const buffer = Buffer.alloc(length);
  const fd = fs.openSync(file, "r");
  try {
    fs.readSync(fd, buffer, 0, length, offset);
  } finally {
    fs.closeSync(fd);
  }

  const text = partial + buffer.toString("utf8");
  const pieces = text.split("\n");
  // ชิ้นสุดท้ายคือบรรทัดที่ยังไม่จบ (ถ้าไฟล์จบด้วย \n ชิ้นนี้จะเป็นสตริงว่าง)
  const tailPiece = pieces.pop();

  const rows = [];
  let bad = 0;
  for (const line of pieces) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      rows.push(JSON.parse(trimmed));
    } catch {
      bad += 1;
    }
  }

  return {
    rows,
    cursor: { offset: stat.size, partial: tailPiece },
    reset,
    missing: false,
    bad,
  };
}

/** อ่านไฟล์ JSON ธรรมดา คืน null ถ้าไม่มีหรือเสีย (ไม่ throw) */
function readJson(file) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return null;
  }
}

module.exports = { readNew, readJson };
