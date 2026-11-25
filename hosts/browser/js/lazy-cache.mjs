// The way to a page's own lazy source (fs.js setLazySource,
// runtime/src/lazy-source-worker.js). A lazy file's read happens in a
// Worker blocked in the kernel (lazy-fetch.mjs), which cannot wait on a
// promise; the source is the page's module, asynchronous. So the read is
// asked of the one Worker that runs it, through slots in a
// SharedArrayBuffer: the reader fills a slot, wakes the owner and waits; the
// owner asks the source and writes the bytes straight into the kernel's
// memory, where the reader wanted them.
//
// The slots: `ctl`, an Int32Array on a SharedArrayBuffer of SLOTS * SLOT
// bytes after a header of HEAD bytes. ctl[0] counts requests (the owner
// waits on it); ctl[1] counts slots freed (a reader that found none free
// waits on it). A slot, i32 at SLOT/4 * i + HEAD/4:
//   [0] state: FREE, ASKED (the reader waits), TAKEN (the owner asks the
//       source; the reader keeps waiting), WRITING (the owner copies), DONE
//   [1] result: bytes written, or -1
//   [2] dst, [3] len, [4] key length, [5..6] offset (f64 at byte 24)
//   bytes 32..: the key, at most KEY_MAX

export const CHUNK = 1 << 20;
export const SLOTS = 32;
export const HEAD = 64;
export const SLOT = 1056;
export const KEY_MAX = SLOT - 32;
export const FREE = 0, ASKED = 1, WRITING = 2, DONE = 3, TAKEN = 5;

/** The control buffer the page makes and hands to the owner and to every
 *  Worker. */
export function makeControl() {
  return new SharedArrayBuffer(HEAD + SLOTS * SLOT);
}

let ctl = null;
let raw = null;
/** Whether this Worker has a lazy source to ask (`setLazyCache`). */
export function lazyCacheOn() {
  return ctl !== null;
}

/** A Worker's side: `c` is `{ ctl: SharedArrayBuffer }`. */
export function setLazyCache(c) {
  if (!c?.ctl) return;
  ctl = new Int32Array(c.ctl);
  raw = new Uint8Array(c.ctl);
}

/** Read `len` bytes of `key` at `from` into the kernel memory at `dst`,
 *  through the owner. Returns the bytes written, or -1 (no owner, a key
 *  too long, the source's failure). */
export function cachedRead(key, from, dst, len) {
  if (!ctl) return -1;
  const k = new TextEncoder().encode(key);
  if (k.length > KEY_MAX) return -1;
  let s = -1;
  for (;;) {
    const freed = Atomics.load(ctl, 1);
    for (let i = 0; i < SLOTS; i++) {
      if (Atomics.compareExchange(ctl, (HEAD + i * SLOT) / 4, FREE, ASKED + 100) === FREE) { s = i; break; }
    }
    // The owner is the only way to the bytes: a full set of slots is waited out.
    if (s >= 0) break;
    Atomics.wait(ctl, 1, freed, 1000);
  }
  const base = (HEAD + s * SLOT) / 4;
  ctl[base + 1] = -1;
  ctl[base + 2] = dst;
  ctl[base + 3] = len;
  ctl[base + 4] = k.length;
  new Float64Array(ctl.buffer, HEAD + s * SLOT + 24, 1)[0] = from;
  raw.set(k, HEAD + s * SLOT + 32);
  // Claimed (ASKED + 100) while being written; asked now.
  Atomics.store(ctl, base, ASKED);
  Atomics.add(ctl, 0, 1);
  Atomics.notify(ctl, 0);
  const t0 = Date.now();
  let said = false;
  for (;;) {
    const st = Atomics.load(ctl, base);
    if (st === DONE) break;
    // a read that stalls its program, said once
    if (!said && Date.now() - t0 > 10000) {
      said = true;
      console.warn(`[gex] a read of ${key} @${from} has waited 10 s for the page's lazy source (${st === ASKED ? 'not taken yet' : 'taken'})`);
    }
    Atomics.wait(ctl, base, st, st === ASKED ? 5000 : 1000);
  }
  const n = ctl[base + 1];
  Atomics.store(ctl, base, FREE);
  Atomics.add(ctl, 1, 1);
  Atomics.notify(ctl, 1);
  return n < 0 ? -1 : n;
}

// A lazy file's ranged request that fails (lazy-fetch.mjs): tried again
// after each of RETRY_MS, then given up; the reader gets -1 (the kernel's EIO). A frame given up is not asked for
// again for FAILED_HOLD_MS: every read of it before then fails at once, with
// no request. Without this every read of a frame the server would not give
// asked again at once, and the guest read again, a few hundred requests a
// second while the program waited on the file.
export const RETRY_MS = Object.freeze([250, 1000, 4000]);
export const FAILED_HOLD_MS = 30000;

/** Frames (by key) given up, until when. */
export class FailedFrames {
  #until = new Map();
  /** Whether `key` was given up and is still held. */
  held(key, now = Date.now()) {
    const t = this.#until.get(key);
    if (t === undefined) return false;
    if (now < t) return true;
    this.#until.delete(key);
    return false;
  }
  failed(key, now = Date.now()) { this.#until.set(key, now + FAILED_HOLD_MS); }
  ok(key) { this.#until.delete(key); }
}

let napCell = null;
/** `get()` until it gives bytes, waiting RETRY_MS between tries (a Worker
 *  blocked in the kernel: `Atomics.wait`); null once given up. */
export function retrySync(get, delays = RETRY_MS) {
  for (let i = 0; ; i++) {
    let got = null;
    try { got = get(); } catch { got = null; }
    if (got) return got;
    if (i >= delays.length) return null;
    Atomics.wait(napCell ??= new Int32Array(new SharedArrayBuffer(4)), 0, 0, delays[i]);
  }
}
