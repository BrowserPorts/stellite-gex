// `Store`: an OPFSFilesystem's store edited while no system holds it, from
// a dedicated Worker (a sync access handle is only had there). The format
// and every change to it are gex-store's (crates/gex-store, as store.wasm,
// hosts/store); this hands it the store file's handle and holds the store's
// Web Lock, the one a System takes (blocks.mjs `claimStore`), for as long as
// the Store is open.
//
// Paths are the guest's, absolute. A listing is the store's records: what
// the page or a run wrote, removed or changed of the sources, not the
// sources themselves. What is written is the next boot's once `commit()`
// has written it as a generation.

import { claimStore, openHandle, handleOp, storeDir, STORE_FILE, NOSPACE } from '../../../hosts/browser/js/blocks.mjs';
import files from './runtime-files.js';

/** fs.js's: the kernel's blocks of an OPFSFilesystem, and how large a file
 *  stays in its record. */
const BLOCK = 65536;
const INLINE = 16384;
/** How much of a file goes to store.wasm at a time. */
const PIECE = 16 << 20;

const ASSETS = typeof __GEX_RUNTIME_ASSETS__ === 'undefined' ? '../../public/' : __GEX_RUNTIME_ASSETS__;
const DEFAULT_BASE = () => (ASSETS === null ? new URL('/', globalThis.location?.href).href : new URL(ASSETS, import.meta.url).href);

const ERRNO = { 2: 'ENOENT', 5: 'EIO', 6: 'ENXIO', 17: 'EEXIST', 20: 'ENOTDIR', 21: 'EISDIR', 22: 'EINVAL', 27: 'EFBIG', 28: 'ENOSPC', 16: 'EBUSY' };
const fail = (what, errno, path) => Object.assign(new Error(`${what}: ${ERRNO[errno] ?? `errno ${errno}`}${path ? `: ${path}` : ''}`), { code: ERRNO[errno] ?? 'EIO', errno });
const FOUND = ['fresh', 'restored', 'fell-back'];

let compiled = null;
function moduleFor(url) {
  compiled ??= new Map();
  if (!compiled.has(url)) {
    const m = WebAssembly.compileStreaming(fetch(url));
    m.catch(() => compiled.delete(url));
    compiled.set(url, m);
  }
  return compiled.get(url);
}

const enc = new TextEncoder();
const dec = new TextDecoder();

export class Store {
  #name;
  #x;
  #h;
  #claim;
  #found;
  #closed = false;

  constructor(name, x, h, claim, found) {
    this.#name = name;
    this.#x = x;
    this.#h = h;
    this.#claim = claim;
    this.#found = found;
  }

  /**
   * The store `name` (an OPFSFilesystem's), open for editing in this
   * Worker. `wait`: until another page or Worker lets it go (`signal` stops
   * the wait), else EBUSY at once. `baseUrl`: where gex/store.wasm is, as
   * System's; `wasm` a URL or a compiled module instead. EIO when the store
   * keeps something no generation of which reads: it is never written then.
   */
  static async open(name, { wait = false, signal, baseUrl, wasm } = {}) {
    name = String(name ?? '');
    if (!/^[A-Za-z0-9._-]{1,64}$/.test(name)) throw new TypeError('Store.open: name the store (letters, digits, . _ -)');
    if (typeof FileSystemFileHandle !== 'function' || !('createSyncAccessHandle' in FileSystemFileHandle.prototype)) {
      throw new Error('Store.open: a store is edited from a dedicated Worker (a sync access handle is had only there)');
    }
    const own = baseUrl == null ? files?.['gex/store.wasm'] : undefined;
    const url = wasm instanceof WebAssembly.Module ? null
      : String(wasm ?? own ?? new URL('gex/store.wasm', new URL(baseUrl ?? DEFAULT_BASE(), globalThis.location?.href)).href);
    const mod = url ? await moduleFor(url) : wasm;
    const claim = await claimStore(name, { wait, signal });
    if (!claim) throw fail('Store.open', 16, `the store ${name} is open in another tab`);
    let h = null;
    try {
      const root = await navigator.storage.getDirectory();
      const dir = await root.getDirectoryHandle(storeDir(name), { create: true });
      h = await openHandle(await dir.getFileHandle(STORE_FILE, { create: true }));
      let memory = null;
      const { exports: x } = await WebAssembly.instantiate(mod, {
        store: {
          // blocks.mjs answers the kernel's store the same way: every count
          // checked, anything short a failure.
          io: (op, offset, ptr, len) => {
            const r = handleOp(h, true, memory, op, Number(offset), ptr, len);
            if (r === NOSPACE) return -28;
            return r < 0 ? -5 : 0;
          },
        },
      });
      memory = x.memory;
      const found = x.st_open(BLOCK, INLINE);
      if (found < 0) throw fail('Store.open', -found, name);
      return new Store(name, x, h, claim, FOUND[found]);
    } catch (e) {
      h?.close();
      claim.release();
      throw e;
    }
  }

  /** Forget the store `name`: its file removed, once no one holds it. */
  static async erase(name, { wait = true, signal } = {}) {
    const claim = await claimStore(name, { wait, signal });
    if (!claim) throw fail('Store.erase', 16, `the store ${name} is open in another tab`);
    try {
      const root = await navigator.storage.getDirectory();
      // A system or Store just closed lets the file go a moment later.
      for (let i = 0; ; i++) {
        try {
          return await root.removeEntry(storeDir(name), { recursive: true });
        } catch (e) {
          if (e?.name === 'NotFoundError') return;
          if (e?.name !== 'NoModificationAllowedError' || i >= 100) throw e;
          await new Promise((r) => setTimeout(r, 100));
        }
      }
    } finally {
      claim.release();
    }
  }

  get name() { return this.#name; }

  /** What the open read: 'fresh' (nothing kept), 'restored' (the newest
   *  generation) or 'fell-back' (the newest was torn: the one before). */
  get found() { return this.#found; }

  /** The generation read, or last written. */
  get generation() { return this.#live().st_generation(); }

  #live() {
    if (this.#closed) throw new Error(`Store ${this.#name}: closed`);
    return this.#x;
  }

  /** `fn(ptr, len)` with `s` (a string or bytes) in store.wasm's memory. */
  #with(s, fn) {
    const x = this.#live();
    const b = typeof s === 'string' ? enc.encode(s) : s;
    const p = x.st_alloc(b.length);
    try {
      new Uint8Array(x.memory.buffer, p, b.length).set(b);
      return fn(p, b.length);
    } finally {
      x.st_dealloc(p, b.length);
    }
  }

  #text(n) {
    return dec.decode(new Uint8Array(this.#x.memory.buffer, this.#x.st_out(), n));
  }

  #check(what, r, path) {
    if (r < 0) throw fail(what, -r, path);
    return r;
  }

  static #entry([path, type, size, mode, mtime]) {
    return { path, type, size, mode, mtime: mtime * 1000 };
  }

  /** Every record at or under `prefix`, in path order: `{ path, type,
   *  size, mode, mtime (ms) }`, `type` one of 'file', 'directory',
   *  'symlink', 'gone' (a path of the sources removed), 'attrs' (a file of
   *  the sources, its mode changed), 'lazy' (fetched from a URL). */
  list(prefix = '/') {
    const n = this.#check('list', this.#with(prefix, (p, l) => this.#x.st_list(p, l)), prefix);
    return JSON.parse(this.#text(n)).map(Store.#entry);
  }

  /** The record at `path`, as `list` gives it; ENOENT for none. */
  stat(path) {
    const n = this.#check('stat', this.#with(path, (p, l) => this.#x.st_stat(p, l)), path);
    return Store.#entry(JSON.parse(this.#text(n)));
  }

  readlink(path) {
    const n = this.#check('readlink', this.#with(path, (p, l) => this.#x.st_readlink(p, l)), path);
    return this.#text(n);
  }

  /** The file's bytes, or `length` of them from `offset`. */
  read(path, { offset = 0, length } = {}) {
    const size = this.stat(path).size;
    const want = Math.max(0, Math.min(length ?? size - offset, size - offset));
    const out = new Uint8Array(want);
    const x = this.#live();
    for (let at = 0; at < want;) {
      const n = Math.min(PIECE, want - at);
      const dst = x.st_alloc(n);
      try {
        const got = this.#check('read', this.#with(path, (p, l) => x.st_read(p, l, BigInt(offset + at), dst, n)), path);
        out.set(new Uint8Array(x.memory.buffer, dst, got), at);
        if (got < n) return out.subarray(0, at + got);
        at += got;
      } finally {
        x.st_dealloc(dst, n);
      }
    }
    return out;
  }

  /**
   * Write `data` (bytes, a string, or a Blob, read a piece at a time) into
   * the file at `path`: replacing it (`offset` 0 and `truncate`, the
   * default), or at `offset`. `mode` for a new file (0o644), `mtime` ms.
   */
  async write(path, data, { offset = 0, truncate = offset === 0, mode = 0o644, mtime = Date.now() } = {}) {
    const x = this.#live();
    const secs = Math.floor(mtime / 1000);
    const size = data instanceof Blob ? data.size : (typeof data === 'string' ? (data = enc.encode(data)) : (data = data instanceof ArrayBuffer ? new Uint8Array(data) : data)).length;
    let first = true;
    for (let at = 0; first || at < size; first = false) {
      const n = Math.min(PIECE, size - at);
      const piece = data instanceof Blob ? new Uint8Array(await data.slice(at, at + n).arrayBuffer()) : data.subarray(at, at + n);
      const src = x.st_alloc(n);
      try {
        new Uint8Array(x.memory.buffer, src, n).set(piece);
        this.#check('write', this.#with(path, (p, l) => x.st_write(p, l, BigInt(offset + at), src, n, mode, secs, first && truncate ? 1 : 0)), path);
      } finally {
        x.st_dealloc(src, n);
      }
      at += n;
    }
  }

  /** The record at `path` and every one under it gone. `hide`: what the
   *  sources have there is removed too (a 'gone' record). */
  remove(path, { hide = false } = {}) {
    this.#check('remove', this.#with(path, (p, l) => this.#x.st_remove(p, l, hide ? 1 : 0)), path);
  }

  mkdir(path, { mode = 0o755, mtime = Date.now() } = {}) {
    this.#check('mkdir', this.#with(path, (p, l) => this.#x.st_mkdir(p, l, mode, Math.floor(mtime / 1000))), path);
  }

  symlink(target, path) {
    this.#check('symlink', this.#with(target, (t, tl) => this.#with(path, (p, l) => this.#x.st_symlink(t, tl, p, l))), path);
  }

  chmod(path, mode) {
    this.#check('chmod', this.#with(path, (p, l) => this.#x.st_chmod(p, l, mode)), path);
  }

  /** The records at and under `from` moved to `to`. */
  rename(from, to) {
    this.#check('rename', this.#with(from, (a, al) => this.#with(to, (b, bl) => this.#x.st_rename(a, al, b, bl))), from);
  }

  /** What changed written as the next generation (every record, when
   *  `checkpoint`): whether one was written. */
  commit({ checkpoint = false } = {}) {
    return this.#check('commit', this.#live().st_commit(checkpoint ? 1 : 0), this.#name) === 1;
  }

  /** Let the store go: what was not committed is dropped. */
  close() {
    if (this.#closed) return;
    this.#closed = true;
    this.#x.st_close();
    this.#h.close();
    this.#claim.release();
  }
}
