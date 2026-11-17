/** One of a store's records, as `Store.list` and `Store.stat` give them. */
export interface StoreEntry {
  /** The guest path, absolute. */
  path: string;
  /** 'gone': a path of the sources removed; 'attrs': a file of the sources,
   *  its mode changed; 'lazy': a file fetched from a URL, not readable here. */
  type: 'file' | 'directory' | 'symlink' | 'gone' | 'attrs' | 'lazy';
  size: number;
  mode: number;
  /** ms since the epoch. */
  mtime: number;
}

export interface StoreOpenOptions {
  /** Wait for another page or Worker to let the store go; else EBUSY at once. */
  wait?: boolean;
  signal?: AbortSignal;
  /** Where gex/store.wasm is, as System's `baseUrl`. */
  baseUrl?: string | URL;
  /** store.wasm's URL, or the module compiled. */
  wasm?: string | URL | WebAssembly.Module;
}

/** An OPFSFilesystem's store edited while no system holds it, in a
 *  dedicated Worker. Errors carry a `code` ('ENOENT', 'EBUSY', 'EIO',
 *  'ENOSPC', ...). */
export class Store {
  static open(name: string, options?: StoreOpenOptions): Promise<Store>;
  static erase(name: string, options?: { wait?: boolean; signal?: AbortSignal }): Promise<void>;
  readonly name: string;
  readonly found: 'fresh' | 'restored' | 'fell-back';
  readonly generation: number;
  list(prefix?: string): StoreEntry[];
  stat(path: string): StoreEntry;
  readlink(path: string): string;
  read(path: string, options?: { offset?: number; length?: number }): Uint8Array;
  write(path: string, data: Uint8Array | ArrayBuffer | Blob | string, options?: { offset?: number; truncate?: boolean; mode?: number; mtime?: number }): Promise<void>;
  remove(path: string, options?: { hide?: boolean }): void;
  mkdir(path: string, options?: { mode?: number; mtime?: number }): void;
  symlink(target: string, path: string): void;
  chmod(path: string, mode: number): void;
  rename(from: string, to: string): void;
  commit(options?: { checkpoint?: boolean }): boolean;
  close(): void;
}
