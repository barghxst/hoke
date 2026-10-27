import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import init, { WasmWal } from "../packages/hoke/wasm/hoke_wasm.js";

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
