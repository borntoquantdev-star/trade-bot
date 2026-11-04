/* เทสตัวอ่าน JSONL แบบต่อท้าย — ไม่แตะเน็ต ใช้ไฟล์ชั่วคราวในเครื่อง */

"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { readNew, readJson } = require("../lib/tail.js");

function tmpFile(name) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "hyperbot-dash-"));
  return path.join(dir, name || "audit.jsonl");
}

test("ไฟล์ที่ยังไม่มี ไม่ทำให้พัง", () => {
  const result = readNew(path.join(os.tmpdir(), "ไม่มีจริงๆ-12345.jsonl"), {});
  assert.equal(result.missing, true);
  assert.deepEqual(result.rows, []);
  assert.equal(result.cursor.offset, 0);
});

test("อ่านทุกบรรทัดในครั้งแรก", () => {
  const file = tmpFile();
  fs.writeFileSync(file, '{"a":1}\n{"a":2}\n', "utf8");
  const result = readNew(file, {});
  assert.equal(result.rows.length, 2);
  assert.equal(result.rows[1].a, 2);
  assert.equal(result.missing, false);
});

test("รอบถัดไปอ่านเฉพาะส่วนที่เพิ่มมา", () => {
  const file = tmpFile();
  fs.writeFileSync(file, '{"a":1}\n', "utf8");
  const first = readNew(file, {});
  assert.equal(first.rows.length, 1);

  fs.appendFileSync(file, '{"a":2}\n{"a":3}\n', "utf8");
  const second = readNew(file, first.cursor);
  assert.deepEqual(
    second.rows.map((r) => r.a),
    [2, 3]
  );

  // ไม่มีอะไรเพิ่ม = ไม่มีแถวใหม่
  const third = readNew(file, second.cursor);
  assert.deepEqual(third.rows, []);
});

test("บรรทัดที่ยังเขียนไม่จบ ถูกเก็บไว้รอรอบหน้า", () => {
  const file = tmpFile();
  fs.writeFileSync(file, '{"a":1}\n{"a":2', "utf8"); // บรรทัดสองยังไม่จบ
  const first = readNew(file, {});
  assert.equal(first.rows.length, 1, "ต้องไม่ parse บรรทัดที่ยังไม่จบ");
  assert.equal(first.cursor.partial, '{"a":2');

  fs.appendFileSync(file, '}\n', "utf8");
  const second = readNew(file, first.cursor);
  assert.equal(second.rows.length, 1);
  assert.equal(second.rows[0].a, 2);
});

test("ไฟล์ถูกเริ่มใหม่ (สั้นลง) แล้วอ่านใหม่ทั้งไฟล์", () => {
  const file = tmpFile();
  fs.writeFileSync(file, '{"a":1}\n{"a":2}\n{"a":3}\n', "utf8");
  const first = readNew(file, {});
  assert.equal(first.rows.length, 3);

  fs.writeFileSync(file, '{"a":9}\n', "utf8"); // บอทเริ่มรอบใหม่ ไฟล์สั้นลง
  const second = readNew(file, first.cursor);
  assert.equal(second.reset, true);
  assert.equal(second.rows.length, 1);
  assert.equal(second.rows[0].a, 9);
});

test("บรรทัดที่ parse ไม่ได้ ถูกข้ามแต่ยังนับไว้", () => {
  const file = tmpFile();
  fs.writeFileSync(file, '{"a":1}\nไม่ใช่ json\n{"a":2}\n', "utf8");
  const result = readNew(file, {});
  assert.equal(result.rows.length, 2);
  assert.equal(result.bad, 1);
});

test("บรรทัดว่างไม่นับเป็นข้อมูล", () => {
  const file = tmpFile();
  fs.writeFileSync(file, '{"a":1}\n\n\n{"a":2}\n', "utf8");
  const result = readNew(file, {});
  assert.equal(result.rows.length, 2);
  assert.equal(result.bad, 0);
});

test("readJson คืน null เมื่อไฟล์ไม่มีหรือเสีย", () => {
  assert.equal(readJson(path.join(os.tmpdir(), "ไม่มี-98765.json")), null);
  const file = tmpFile("state.json");
  fs.writeFileSync(file, "{ ยังไม่ใช่ json", "utf8");
  assert.equal(readJson(file), null);
  fs.writeFileSync(file, '{"halted":true}', "utf8");
  assert.deepEqual(readJson(file), { halted: true });
});
