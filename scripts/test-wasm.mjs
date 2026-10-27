import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import init, { WasmSst, WasmWal } from "../packages/hoke/wasm/hoke_wasm.js";

// Run the actual generated Rust/WASM artifact. This adapter only replaces the
// byte transport; it does not emulate Engine, the WAL codec, or recovery.
await init({
  module_or_path: await readFile(
    new URL("../packages/hoke/wasm/hoke_wasm_bg.wasm", import.meta.url),
  ),
});

class ByteAdapter {
  constructor(bytes = new Uint8Array()) {
    this.bytes = bytes.slice();
    this.reads = 0;
    this.writes = 0;
    this.syncs = 0;
    this.truncations = 0;
  }

  getSize() {
    return this.bytes.length;
  }

  readAt(buffer, at) {
    const count = Math.min(buffer.length, Math.max(0, this.bytes.length - at), 3);
    buffer.set(this.bytes.subarray(at, at + count));
    this.reads++;
    return count;
  }

  writeAt(buffer, at) {
    const count = Math.min(buffer.length, 3);
    if (at + count > this.bytes.length) this.resize(at + count);
    this.bytes.set(buffer.subarray(0, count), at);
    this.writes++;
    return count;
  }

  resize(size) {
    const next = new Uint8Array(size);
    next.set(this.bytes.subarray(0, size));
    this.bytes = next;
  }

  truncate(size) {
    this.resize(size);
    this.truncations++;
  }

  flush() {
    this.syncs++;
  }

  close() {
    assert.fail("the WASM engine must not close the caller-owned handle");
  }
}

const key = Uint8Array.of(0, 255, 1);
const initial = Uint8Array.of(9, 0, 255);
const changed = Uint8Array.of(8, 7);

function close(engine) {
  try {
    engine.close();
  } finally {
    engine.free();
  }
}

function savedWal() {
  const adapter = new ByteAdapter();
  const engine = new WasmWal(adapter);
  engine.put(key, initial);
  close(engine);
  return adapter.bytes.slice();
}

function failedOpenPreserves(adapter, expected) {
  const before = adapter.bytes.slice();
  assert.throws(() => new WasmWal(adapter), expected);
  assert.deepEqual(adapter.bytes, before);
  assert.equal(adapter.truncations, 0);
}

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) {
      crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

test("actual WASM handles binary keys and three-byte short reads/writes", () => {
  const adapter = new ByteAdapter();
  let engine = new WasmWal(adapter);
  assert.equal(adapter.bytes.length, 16);
  assert.equal(adapter.syncs, 1);
  engine.put(key, initial);
  assert.deepEqual(engine.get(key), initial);
  assert(adapter.writes > 10);
  close(engine);
  engine = new WasmWal(adapter);
  assert(adapter.reads > 10);
  assert.deepEqual(engine.get(key), initial);
  close(engine);
});

test("empty values differ from missing keys and tombstones after recovery", () => {
  const adapter = new ByteAdapter();
  const empty = new Uint8Array();
  let engine = new WasmWal(adapter);
  assert.equal(engine.get(empty), undefined);
  engine.put(empty, empty);
  assert.deepEqual(engine.get(empty), empty);
  close(engine);
  engine = new WasmWal(adapter);
  assert.deepEqual(engine.get(empty), empty);
  engine.delete(empty);
  assert.equal(engine.get(empty), undefined);
  close(engine);
  engine = new WasmWal(adapter);
  assert.equal(engine.get(empty), undefined);
  close(engine);
});

test("returned values are owned copies", () => {
  const adapter = new ByteAdapter();
  const engine = new WasmWal(adapter);
  engine.put(key, initial);
  const copy = engine.get(key);
  copy.fill(123);
  assert.deepEqual(engine.get(key), initial);
  close(engine);
});

test("close is idempotent, rejects further use, and leaves handle ownership in JS", () => {
  const adapter = new ByteAdapter();
  const engine = new WasmWal(adapter);
  close(engine);
  // free() releases the Rust object, so exercise idempotence on a fresh object.
  const second = new WasmWal(adapter);
  second.close();
  const syncs = adapter.syncs;
  second.close();
  assert.equal(adapter.syncs, syncs);
  assert.throws(() => second.get(key), /closed/);
  assert.throws(() => second.put(key, initial), /closed/);
  assert.throws(() => second.delete(key), /closed/);
  second.free();
});

test("failed sync has unknown outcome, poisons writes, and recovers complete frame", () => {
  const adapter = new ByteAdapter(savedWal());
  let engine = new WasmWal(adapter);
  adapter.flush = () => {
    throw new Error("injected flush failure");
  };
  assert.throws(() => engine.put(key, changed), /injected flush failure/);
  assert.deepEqual(engine.get(key), initial);
  const afterFailure = adapter.bytes.slice();
  assert.throws(() => engine.put(key, initial), /outcome is unknown/);
  assert.throws(() => engine.delete(key), /outcome is unknown/);
  assert.deepEqual(adapter.bytes, afterFailure);
  assert.throws(() => engine.close(), /outcome is unknown/);
  engine.close();
  engine.free();
  delete adapter.flush;
  engine = new WasmWal(adapter);
  assert.deepEqual(engine.get(key), changed);
  close(engine);
});

test("exception after a partial append preserves confirmed data on recovery", () => {
  const adapter = new ByteAdapter(savedWal());
  const confirmedLength = adapter.bytes.length;
  let engine = new WasmWal(adapter);
  adapter.writeAt = function (buffer, at) {
    ByteAdapter.prototype.writeAt.call(this, buffer, at);
    throw new Error("injected partial append");
  };
  assert.throws(() => engine.put(key, changed), /injected partial append/);
  assert.deepEqual(engine.get(key), initial);
  assert(adapter.bytes.length > confirmedLength);
  assert.throws(() => engine.close(), /outcome is unknown/);
  engine.free();
  delete adapter.writeAt;
  engine = new WasmWal(adapter);
  assert.equal(adapter.bytes.length, confirmedLength);
  assert.deepEqual(engine.get(key), initial);
  close(engine);
});

test("incomplete final frames are truncated at every byte boundary", () => {
  const prefix = savedWal();
  const adapter = new ByteAdapter(prefix);
  const engine = new WasmWal(adapter);
  engine.put(key, changed);
  close(engine);
  for (let length = prefix.length + 1; length < adapter.bytes.length; length++) {
    const partial = new ByteAdapter(adapter.bytes.subarray(0, length));
    const recovered = new WasmWal(partial);
    assert.equal(partial.truncations, 1, `cut at ${length}`);
    assert.deepEqual(partial.bytes, prefix, `cut at ${length}`);
    assert.deepEqual(recovered.get(key), initial, `cut at ${length}`);
    close(recovered);
  }
});

test("partial file headers are errors and remain untouched", () => {
  const bytes = savedWal();
  for (let length = 1; length < 16; length++) {
    failedOpenPreserves(new ByteAdapter(bytes.subarray(0, length)), /header/);
  }
});

test("complete corrupt header or payload is rejected without truncation", () => {
  for (const offset of [0, 12, 16 + 24, savedWal().length - 1]) {
    const bytes = savedWal();
    bytes[offset] ^= 0x80;
    failedOpenPreserves(new ByteAdapter(bytes), /corrupt|checksum|magic/);
  }
});

test("future file version with valid checksum is preserved", () => {
  assert.equal(crc32(new TextEncoder().encode("123456789")), 0xcbf43926);
  const bytes = savedWal();
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  view.setUint32(8, 2, true);
  view.setUint32(12, crc32(bytes.subarray(0, 12)), true);
  failedOpenPreserves(new ByteAdapter(bytes), /unsupported WAL version/);
});

test("invalid metadata sizes are rejected before mutation", () => {
  for (const size of [-1, 0.5, NaN, Infinity, -Infinity, 2 ** 53, Number.MAX_VALUE]) {
    const adapter = new ByteAdapter(savedWal());
    adapter.getSize = () => size;
    failedOpenPreserves(adapter, /safe integer/);
  }
});

test("invalid read counts and premature EOF are rejected", () => {
  for (const count of [-1, 0.5, NaN, Infinity, 2 ** 53, 100000, 0]) {
    const adapter = new ByteAdapter(savedWal());
    adapter.readAt = () => count;
    failedOpenPreserves(adapter, /safe integer|more bytes|shorter/);
  }
});

test("invalid write counts and zero progress poison mutations", () => {
  for (const count of [-1, 0.5, NaN, Infinity, 2 ** 53, 100000, 0]) {
    const adapter = new ByteAdapter(savedWal());
    const engine = new WasmWal(adapter);
    adapter.writeAt = () => count;
    assert.throws(() => engine.put(key, changed), /safe integer|more bytes|progress/);
    assert.deepEqual(engine.get(key), initial);
    assert.throws(() => engine.put(key, changed), /outcome is unknown/);
    assert.throws(() => engine.close(), /outcome is unknown/);
    engine.free();
    assert.deepEqual(adapter.bytes, savedWal());
  }
});

test("adapter exceptions from metadata, read, and truncate become engine errors", () => {
  for (const method of ["getSize", "readAt", "truncate"]) {
    const bytes = savedWal();
    const input = method === "truncate" ? Uint8Array.from([...bytes, 0]) : bytes;
    const adapter = new ByteAdapter(input);
    adapter[method] = () => {
      throw new Error(`injected ${method} failure`);
    };
    failedOpenPreserves(adapter, new RegExp(`injected ${method} failure`));
  }
});

test("oversized keys fail without poisoning or changing bytes", () => {
  const adapter = new ByteAdapter(savedWal());
  const engine = new WasmWal(adapter);
  const before = adapter.bytes.slice();
  assert.throws(() => engine.put(new Uint8Array(65537), initial), /WAL size limit/);
  assert.deepEqual(adapter.bytes, before);
  engine.put(key, changed);
  assert.deepEqual(engine.get(key), changed);
  close(engine);
});

test("final synchronization failure still consumes the engine", () => {
  const adapter = new ByteAdapter(savedWal());
  const engine = new WasmWal(adapter);
  adapter.flush = () => {
    throw new Error("injected close flush failure");
  };
  assert.throws(() => engine.close(), /injected close flush failure/);
  engine.close();
  assert.throws(() => engine.get(key), /closed/);
  engine.free();
});

// Engine appends and recovery truncates. Neither API exposes arbitrary gap
// writes/extensions; those private Storage paths are not claimed as covered.

function unpackScan(packet) {
  assert(packet instanceof Uint8Array);
  const view = new DataView(packet.buffer, packet.byteOffset, packet.byteLength);
  assert(packet.length >= 4);
  const count = view.getUint32(0, true);
  const rows = [];
  let offset = 4;
  for (let row = 0; row < count; row++) {
    assert(offset + 8 <= packet.length);
    const keyLength = view.getUint32(offset, true);
    const valueLength = view.getUint32(offset + 4, true);
    offset += 8;
    assert(offset + keyLength + valueLength <= packet.length);
    const key = packet.slice(offset, offset + keyLength);
    offset += keyLength;
    const value = packet.slice(offset, offset + valueLength);
    offset += valueLength;
    rows.push([key, value]);
  }
  assert.equal(offset, packet.length, "scan packet must have no trailing bytes");
  return rows;
}

const bytes = (...values) => Uint8Array.from(values);
const scan = (engine, start, end, prefix, flags = 0, limit = 100) =>
  unpackScan(engine.scan(start, end, prefix, flags, limit));

test("actual WASM packed scans preserve binary order, tombstones, bounds, and recovery", () => {
  const adapter = new ByteAdapter();
  let engine = new WasmWal(adapter);
  const rows = [
    [bytes(), bytes()],
    [bytes(0), bytes(1)],
    [bytes(0, 255), bytes(2)],
    [bytes(255), bytes(3)],
    [bytes(255, 255), bytes(4)],
  ];
  for (const [key, value] of rows) engine.put(key, value);
  engine.put(bytes(1), bytes(9));
  engine.delete(bytes(1));
  assert.deepEqual(scan(engine), rows);
  assert.deepEqual(scan(engine, undefined, undefined, undefined, 4), [...rows].reverse());
  assert.deepEqual(scan(engine, bytes(0), bytes(255)), rows.slice(1, 3));
  assert.deepEqual(scan(engine, bytes(0), bytes(255), undefined, 3), rows.slice(2, 4));
  assert.deepEqual(scan(engine, bytes(0), bytes(255), undefined, 7), rows.slice(2, 4).reverse());
  assert.deepEqual(scan(engine, undefined, undefined, bytes(255)), rows.slice(3));
  assert.deepEqual(scan(engine, undefined, undefined, bytes()), rows);
  assert.deepEqual(scan(engine, bytes(), bytes(), undefined, 2), rows.slice(0, 1));
  assert.deepEqual(scan(engine, bytes(), bytes()), []);
  assert.deepEqual(scan(engine, bytes(), bytes(), undefined, 3), []);
  assert.deepEqual(scan(engine, undefined, undefined, undefined, 0, 2), rows.slice(0, 2));
  close(engine);
  engine = new WasmWal(adapter);
  assert.deepEqual(scan(engine), rows);
  const copy = scan(engine);
  copy[1][0].fill(123);
  copy[1][1].fill(123);
  assert.deepEqual(scan(engine), rows);
  close(engine);
});

test("actual WASM validates scan options even with zero limit and performs no storage I/O", () => {
  const adapter = new ByteAdapter(savedWal());
  const engine = new WasmWal(adapter);
  const before = adapter.bytes.slice();
  for (const method of ["getSize", "readAt", "writeAt", "truncate", "flush"]) {
    adapter[method] = () => assert.fail(`scan performed ${method}`);
  }
  assert.deepEqual(scan(engine), [[key, initial]]);
  assert.deepEqual(scan(engine, undefined, undefined, undefined, 0, 0), []);
  assert.throws(() => scan(engine, bytes(2), bytes(1), undefined, 0, 0), /range/);
  assert.throws(() => scan(engine, undefined, undefined, undefined, 8, 0), /flags/);
  assert.throws(() => scan(engine, undefined, undefined, undefined, 0, 1001), /1000/);
  for (let position = 0; position < 3; position++) {
    const args = [undefined, undefined, undefined, 0, 0];
    args[position] = new Uint8Array(65537);
    assert.throws(() => scan(engine, ...args), /65536/);
  }
  assert.deepEqual(scan(engine, undefined, undefined, new Uint8Array(65536), 0, 0), []);
  assert.deepEqual(adapter.bytes, before);
  for (const method of ["getSize", "readAt", "writeAt", "truncate", "flush"])
    delete adapter[method];
  engine.put(bytes(5), bytes(6));
  close(engine);
});

class FullByteAdapter extends ByteAdapter {
  readAt(buffer, at) {
    const count = Math.min(buffer.length, this.bytes.length - at);
    buffer.set(this.bytes.subarray(at, at + count));
    return count;
  }

  writeAt(buffer, at) {
    if (at + buffer.length > this.bytes.length) this.resize(at + buffer.length);
    this.bytes.set(buffer, at);
    return buffer.length;
  }
}

test("actual WASM scan enforces 1000 rows and exact 32 MiB packet boundary", () => {
  let adapter = new FullByteAdapter();
  let engine = new WasmWal(adapter);
  for (let index = 0; index < 1001; index++) {
    engine.put(bytes(index >>> 8, index & 255), bytes());
  }
  assert.equal(scan(engine, undefined, undefined, undefined, 0, 1000).length, 1000);
  close(engine);
  adapter = new FullByteAdapter();
  engine = new WasmWal(adapter);
  const maximum = 16 * 1024 * 1024;
  const value = new Uint8Array(maximum);
  engine.put(bytes(0), value);
  // Two one-byte keys, two length pairs and row count require 22 bytes.
  engine.put(bytes(1), value.subarray(0, maximum - 22));
  let packet = engine.scan(undefined, undefined, undefined, 0, 100);
  assert.equal(packet.length, 32 * 1024 * 1024);
  assert.equal(new DataView(packet.buffer, packet.byteOffset).getUint32(0, true), 2);
  packet = undefined;
  engine.put(bytes(1), value.subarray(0, maximum - 21));
  assert.throws(() => engine.scan(undefined, undefined, undefined, 0, 100), /packet.*budget/);
  engine.put(bytes(1), value);
  assert.throws(() => engine.scan(undefined, undefined, undefined, 0, 100), /payload.*budget/);
  assert.equal(scan(engine, undefined, undefined, undefined, 0, 1).length, 1);
  engine.delete(bytes(1));
  assert.equal(scan(engine).length, 1);
  close(engine);
});

test("actual WASM rejects scan after close and retains confirmed scan after failed commit", () => {
  const adapter = new ByteAdapter(savedWal());
  const engine = new WasmWal(adapter);
  adapter.flush = () => {
    throw new Error("injected scan sync failure");
  };
  assert.throws(() => engine.put(key, changed), /injected scan sync failure/);
  assert.deepEqual(scan(engine), [[key, initial]]);
  assert.throws(() => engine.close(), /outcome is unknown/);
  assert.throws(() => scan(engine), /closed/);
  assert.throws(() => scan(engine, undefined, undefined, undefined, 0, 0), /closed/);
  engine.close();
  engine.free();
});

// SST packets are ephemeral transfer framing. Durable bytes remain native SST v1.
function packSst(rows) {
  const packet = new Uint8Array(
    4 + rows.reduce((total, [key, value]) => total + 8 + key.length + (value?.length ?? 0), 0),
  );
  const view = new DataView(packet.buffer);
  view.setUint32(0, rows.length, true);
  let cursor = 4;
  for (const [key, value] of rows) {
    view.setUint32(cursor, key.length, true);
    view.setUint32(cursor + 4, value === null ? 0xffffffff : value.length, true);
    cursor += 8;
    packet.set(key, cursor);
    cursor += key.length;
    if (value !== null) {
      packet.set(value, cursor);
      cursor += value.length;
    }
  }
  return packet;
}

function unpackSst(packet) {
  const view = new DataView(packet.buffer, packet.byteOffset, packet.byteLength);
  const count = view.getUint32(0, true);
  const rows = [];
  let cursor = 4;
  for (let index = 0; index < count; index++) {
    const keyLength = view.getUint32(cursor, true);
    const valueLength = view.getUint32(cursor + 4, true);
    cursor += 8;
    const key = packet.slice(cursor, cursor + keyLength);
    cursor += keyLength;
    const value = valueLength === 0xffffffff ? null : packet.slice(cursor, cursor + valueLength);
    cursor += value?.length ?? 0;
    rows.push([key, value]);
  }
  assert.equal(cursor, packet.length);
  return rows;
}

const sstScan = (table, start, end, prefix, flags = 0, limit = 100) =>
  unpackSst(table.scan(start, end, prefix, flags, limit));
const sstRows = [
  [bytes(), bytes()],
  [bytes(0), bytes(4)],
  [bytes(1), null],
  [bytes(1, 0), bytes(5, 0)],
  [bytes(255), null],
  [bytes(255, 0), bytes(255)],
];

function savedSst() {
  const adapter = new ByteAdapter();
  close(WasmSst.create(adapter, packSst(sstRows)));
  return adapter;
}

test("actual WASM SST creates native bytes, reopens binary values and distinguishes tombstones", () => {
  const adapter = savedSst();
  assert.equal(new TextDecoder().decode(adapter.bytes.subarray(0, 8)), "STOWSST\0");
  assert.equal(new DataView(adapter.bytes.buffer).getUint32(8, true), 1);
  assert.equal(adapter.syncs, 1);
  const before = adapter.bytes.slice();
  const table = new WasmSst(adapter);
  assert.deepEqual(table.lookup(bytes()), bytes(2));
  assert.deepEqual(table.lookup(bytes(0)), bytes(2, 4));
  assert.deepEqual(table.lookup(bytes(1)), bytes(1));
  assert.deepEqual(table.lookup(bytes(99)), bytes(0));
  assert.deepEqual(sstScan(table), sstRows);
  const copy = table.lookup(bytes(0));
  copy.fill(8);
  assert.deepEqual(table.lookup(bytes(0)), bytes(2, 4));
  close(table);
  assert.deepEqual(adapter.bytes, before);
  assert.equal(adapter.syncs, 1);
  assert.equal(adapter.truncations, 0);
});

test("actual WASM SST scans intersect indexed bounds, prefix and reverse including tombstones", () => {
  const table = new WasmSst(savedSst());
  assert.deepEqual(sstScan(table, bytes(0), bytes(255)), sstRows.slice(1, 4));
  assert.deepEqual(sstScan(table, bytes(0), bytes(255), undefined, 3), sstRows.slice(2, 5));
  assert.deepEqual(sstScan(table, undefined, undefined, bytes(1)), sstRows.slice(2, 4));
  assert.deepEqual(
    sstScan(table, undefined, undefined, bytes(255), 4),
    sstRows.slice(4).toReversed(),
  );
  assert.deepEqual(
    sstScan(table, undefined, undefined, bytes(), 4, 2),
    sstRows.slice(-2).toReversed(),
  );
  assert.deepEqual(sstScan(table, bytes(1), bytes(1), undefined, 2), [sstRows[2]]);
  assert.deepEqual(sstScan(table, bytes(1), bytes(1)), []);
  assert.deepEqual(sstScan(table, bytes(255), undefined, bytes(0)), []);
  close(table);
});

test("actual WASM SST preflights malformed packets without any adapter calls", () => {
  const adapter = {};
  for (const method of ["getSize", "readAt", "writeAt", "truncate", "flush"])
    adapter[method] = () => assert.fail(`invalid packet performed ${method}`);
  const valid = packSst([[bytes(1), bytes(2)]]);
  const packets = [
    bytes(),
    bytes(0),
    bytes(1, 0, 0, 0),
    valid.subarray(0, valid.length - 1),
    Uint8Array.from([...valid, 0]),
    packSst([
      [bytes(1), null],
      [bytes(1), bytes()],
    ]),
    packSst([
      [bytes(2), null],
      [bytes(1), null],
    ]),
  ];
  const badCount = bytes(65, 66, 15, 0); // 1,000,001 little endian.
  packets.push(badCount);
  const badKey = valid.slice();
  new DataView(badKey.buffer).setUint32(4, 65537, true);
  packets.push(badKey);
  const badValue = valid.slice();
  new DataView(badValue.buffer).setUint32(8, 16 * 1024 * 1024 + 1, true);
  packets.push(badValue);
  packets.push(new Uint8Array(32 * 1024 * 1024 + 1));
  for (const packet of packets) assert.throws(() => WasmSst.create(adapter, packet), /SST/);
});

test("actual WASM SST preserves nonempty destination and rejects corrupt immutable data", () => {
  const adapter = savedSst();
  const before = adapter.bytes.slice();
  assert.throws(() => WasmSst.create(adapter, packSst([])), /not empty/);
  assert.deepEqual(adapter.bytes, before);
  for (const offset of [0, 28, before.length - 1]) {
    const bad = new ByteAdapter(before);
    bad.bytes[offset] ^= 1;
    const changed = bad.bytes.slice();
    assert.throws(() => new WasmSst(bad), /SST/);
    assert.deepEqual(bad.bytes, changed);
    assert.equal(bad.writes, 0);
  }
  // Opening validates frame headers/index; payload CRC is validated on lookup.
  const bad = new ByteAdapter(before);
  bad.bytes[32 + 28] ^= 1;
  const table = new WasmSst(bad);
  assert.throws(() => table.lookup(bytes()), /checksum/);
  close(table);
});

test("actual WASM SST creation propagates partial-write and flush failure without publication", () => {
  const writeFailure = new ByteAdapter();
  const original = writeFailure.writeAt.bind(writeFailure);
  writeFailure.writeAt = (buffer, at) => {
    if (at >= 9) throw new Error("injected SST write failure");
    return original(buffer, at);
  };
  assert.throws(() => WasmSst.create(writeFailure, packSst(sstRows)), /injected SST write failure/);
  assert(writeFailure.bytes.length > 0);
  assert.equal(writeFailure.syncs, 0);
  assert.throws(() => new WasmSst(writeFailure), /truncated/);
  const flushFailure = new ByteAdapter();
  flushFailure.flush = () => {
    throw new Error("injected SST flush failure");
  };
  assert.throws(() => WasmSst.create(flushFailure, packSst(sstRows)), /injected SST flush failure/);
  // A candidate can be complete despite its failed durability acknowledgement.
  const reopened = new WasmSst(flushFailure);
  assert.deepEqual(sstScan(reopened), sstRows);
  close(reopened);
});

test("actual WASM SST validates scan and lookup before I/O, then rejects use after close", () => {
  const adapter = savedSst();
  const table = new WasmSst(adapter);
  for (const method of ["getSize", "readAt", "writeAt", "truncate", "flush"])
    adapter[method] = () => assert.fail(`empty/invalid scan performed ${method}`);
  assert.deepEqual(sstScan(table, undefined, undefined, undefined, 0, 0), []);
  assert.deepEqual(table.lookup(bytes(99)), bytes(0));
  assert.throws(() => table.lookup(new Uint8Array(65537)), /65536/);
  assert.throws(() => sstScan(table, bytes(2), bytes(1), undefined, 0, 0), /range/);
  assert.throws(() => sstScan(table, undefined, undefined, undefined, 8, 0), /flags/);
  assert.throws(() => sstScan(table, undefined, undefined, undefined, 0, 1001), /1000/);
  for (let position = 0; position < 3; position++) {
    const args = [undefined, undefined, undefined, 0, 0];
    args[position] = new Uint8Array(65537);
    assert.throws(() => sstScan(table, ...args), /65536/);
  }
  table.close();
  table.close();
  assert.throws(() => table.lookup(bytes()), /closed/);
  assert.throws(() => sstScan(table, undefined, undefined, undefined, 0, 0), /closed/);
  table.free();
});

test("actual WASM SST indexed scan reads selected records only", () => {
  const adapter = savedSst();
  const table = new WasmSst(adapter);
  const offsets = [];
  const original = adapter.readAt.bind(adapter);
  adapter.readAt = (buffer, at) => {
    offsets.push(at);
    return original(buffer, at);
  };
  assert.deepEqual(sstScan(table, undefined, undefined, bytes(1), 4, 1), [sstRows[3]]);
  assert(offsets.length > 0);
  const index = new DataView(adapter.bytes.buffer).getBigUint64(20, true);
  assert(offsets.every((at) => at >= 32 && at < Number(index)));
  // Corrupt an unrelated earlier payload; indexed prefix scan must not read it.
  adapter.bytes[32 + 28] ^= 1;
  assert.deepEqual(sstScan(table, undefined, undefined, bytes(255)), sstRows.slice(4));
  close(table);
});

test("actual WASM SST scan enforces exact packet byte and row boundaries", () => {
  const value = new Uint8Array(16 * 1024 * 1024);
  let adapter = new FullByteAdapter();
  let table = WasmSst.create(
    adapter,
    packSst([
      [bytes(0), value],
      [bytes(1), value.subarray(0, value.length - 22)],
    ]),
  );
  assert.equal(table.scan(undefined, undefined, undefined, 0, 1000).length, 32 * 1024 * 1024);
  close(table);
  // Build a larger native SST by extending the second record by one byte,
  // independently updating native record/header CRCs and its index offset.
  const native = adapter.bytes;
  const oldView = new DataView(native.buffer);
  const oldIndex = Number(oldView.getBigUint64(20, true));
  const second = 32 + 28 + 1 + value.length + 4;
  const expanded = new Uint8Array(native.length + 1);
  const inserted = oldIndex - 4;
  expanded.set(native.subarray(0, inserted));
  expanded.set(native.subarray(inserted), inserted + 1);
  const view = new DataView(expanded.buffer);
  view.setUint32(second, view.getUint32(second, true) + 1, true);
  view.setUint32(second + 20, view.getUint32(second + 20, true) + 1, true);
  view.setUint32(second + 24, crc32(expanded.subarray(second, second + 24)), true);
  view.setUint32(oldIndex - 3, crc32(expanded.subarray(second, oldIndex - 3)), true);
  view.setBigUint64(20, BigInt(oldIndex + 1), true);
  view.setUint32(28, crc32(expanded.subarray(0, 28)), true);
  table = new WasmSst(new FullByteAdapter(expanded));
  assert.equal(table.scan(undefined, undefined, undefined, 0, 1).length, 13 + value.length);
  assert.throws(() => table.scan(undefined, undefined, undefined, 0, 2), /packet.*budget/);
  close(table);
  const rows = Array.from({ length: 1001 }, (_, i) => [
    bytes(i >>> 8, i & 255),
    i % 2 ? null : bytes(),
  ]);
  adapter = new FullByteAdapter();
  table = WasmSst.create(adapter, packSst(rows));
  assert.equal(sstScan(table, undefined, undefined, undefined, 0, 1000).length, 1000);
  assert.deepEqual(sstScan(table, undefined, undefined, undefined, 4, 1)[0][1], rows.at(-1)[1]);
  close(table);
});
