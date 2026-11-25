# stellite-gex

stellite-gex is Stellite's fork of [gex](https://github.com/HeyPuter/gex),
Puter's engine that runs unmodified Linux and Windows programs in a web
browser. It carries Stellite's work on top of gex, and it is the base the
other gex forks under BrowserPorts start from.

- Parent: [HeyPuter/gex](https://github.com/HeyPuter/gex)
- Forks of this repository:
  [unixnu-gex](https://github.com/BrowserPorts/unixnu-gex) (a Darwin
  personality) and
  [android-gex](https://github.com/BrowserPorts/android-gex) (an Android
  personality)
- Built with it:
  [mesa-stellite](https://github.com/BrowserPorts/mesa-stellite), the
  `third_party/mesa` submodule, and
  [wine-stellite](https://github.com/BrowserPorts/wine-stellite), the 64-bit
  Wine that `wine/wine.pins` names

## What gex is

gex translates a program's machine code to WebAssembly and runs it against
its own implementation of the Linux kernel ABI. That kernel is written in
Rust and compiled to WebAssembly as well. A program needs no source changes
and no rebuild.

It runs Linux programs for AArch64, x86-64 and i386, wasm32 Linux programs
built with its own compiler (`toolchain/gexcc`), and Windows programs through
Wine. Wayland and X11 programs share one desktop, and OpenGL goes through
Mesa, compiled to WebAssembly, onto the browser's WebGPU. The same programs
run in a browser page (`runtime/`) and natively under `gexc`, on wasmtime.

Upstream's README describes the engine, its tiers and its layout in full.

## Branches

- `master` is upstream gex with general fixes on top: the emitter, the
  lifters, the function tier, the page, the sysroots and the build, `gexc`,
  the native host and the kernel.
- `stellite` is `master` with Stellite's own work, listed below. It is the
  branch the forks are made from.

## What this fork adds

- **The app library and its sites.** Upstream gex took out its page app,
  its pages and ejected programs' sites in favour of the runtime package
  alone. Stellite's players run on them, so this fork keeps them on
  upstream's layout: the app library, `start()`
  (`runtime/src/host/app.js`, `runtime/app/README.md`), gex's test pages
  (`runtime/app`), the eject site (`site/`, `gexc eject --web-src
  <gex>/runtime`), the page's owner of the lazy files, which keeps them in
  Cache Storage, and the background fill.
- **Wine.** 64-bit Wine with WoW64 (`make wine`) is built from wine-stellite
  at the commit in `wine/wine.pins`. Wine Mono goes into the install with
  `WINE64_MONO=1`. With `WINE64_EXTRA_DLLS`, dirk's D3D11 and DXGI are linked
  into the native PE core and run on the page's WebGPU D3D device. The older
  32-bit-only Wine (`make wine32`) is legacy.
- **Windows 7 glass.** The compositor draws Windows 7 DWM glass: the
  `gex_glass_v1` Wayland protocol, blur-behind and tint, Windows 7's
  colorization, and the frame drawn from `aero.msstyles`.
  `scripts/aero/extract-dwm-frame.py` is a public tool.
- **Graphics.** D3D presents are paced in the compositor
  (`GEXGL_SWAP_INTERVAL`). Mesa takes `force_gl_vendor`, `force_gl_renderer`
  and `transcode_etc` from the environment the page gives it. A process may
  have several GL contexts.
- **The function JIT.** In the page, only hot code goes to the function JIT,
  and it is translated off the guest thread. The function tier (`gex-cgen`)
  has performance work of its own.
- **The block store.** `crates/gex-store` holds the format and allocator of
  the store an `OPFSFilesystem` keeps. `hosts/store` builds it alone as
  `store.wasm`, and the runtime's `Store` edits a store with no system
  booted: writes, a raw or compacting copy, and a file as a Blob. An ejected
  site ships `store.mjs`. The handle `start()` returns can sync, exposes the
  run's filesystem, and syncs before `kill()` stops the run.
- **A macOS host.** `gexc` runs on macOS, and the build runs there too,
  including on a case-insensitive volume. `scripts/host-env.sh` names the
  tools and build folders such a host uses.

## Building

`make help` lists the main targets, and `make help-all` lists every one. Each
target builds from this tree and the pins in it, and prints what it made on
its last line.

```console
$ make gexc      # gexc for AArch64, x86-64 and i386 guests
$ make page      # gex's pages, multi-process build, in runtime/app/dist
$ make serve     # runtime/app/dist at http://localhost:8088/
$ make wine      # 64-bit Wine with WoW64, and a prefix
$ make test      # the runtime tests: node test/run.mjs (ARGS= passes options)
$ make check     # cargo test, the workspace
```

`make page` and `make mesa` check out the `third_party/mesa` submodule
themselves.

What the build needs:

- the Rust toolchain that `rust-toolchain.toml` names
- clang and `wasm-ld` (LLVM 22), and Node.js 22
- for the page: Emscripten (`emsdk`) at the version in `toolchain/pins`, and
  `wasm-tools`
- for Wine: MinGW-w64 GCC 12 (`x86_64-w64-mingw32-gcc`, and
  `i686-w64-mingw32-gcc` for WoW64)
- for the AArch64 tests: `qemu-aarch64`
- on macOS: the Homebrew packages the header of `scripts/host-env.sh` lists

The Makefile's header lists the settings (`OUT`, `CACHE`, `JOBS`, `WINE_DIR`
and the rest). Fetched sources are kept in bare mirrors under the cache, so a
rerun fetches nothing it already has.

## Running

A Linux program, natively:

```console
$ target/release/gexc run hello          # an AArch64 guest
$ target/x86/release/gexc run hello      # an x86-64 or i386 guest
```

A Windows program, natively:

```console
$ make wine
$ . out/wine.env
$ wine/run.sh --dist $WINE64_DIST --prefix $WINE64_PREFIX app.exe
```

`make wine` prints `WINE64_DIST=`, `WINE64_PREFIX=` and `WINE64_NATIVE=`, and
saves them in `out/wine.env`. `wine/run.sh -h` gives the runner's options.
In the page, a Windows program runs from gex's Wine test page,
`runtime/app/nt.html`, whose query names the bundle and the program
(`runtime/src/host/nt-page.js`).

To use gex from a page of your own, build the runtime package with
`make runtime` and follow `runtime/README.md`, or build the app library
(`runtime/app/build.sh --lib DIR`) and call its `start()`
(`runtime/app/README.md`).

## Documentation in the tree

| file | covers |
|------|--------|
| `runtime/files/README.md` | the runtime's files and their two builds |
| `runtime/README.md` | the runtime API, `@gexemu/runtime` |
| `runtime/app/README.md` | the app library, `start()`, and gex's pages |
| `wine/README.md` | building Wine, running a program on it, known gaps |
| `toolchain/README.md` | gexcc and the sysroots |
| `crates/gex-eject/README.md` | ejected projects, built ahead of time |
| `test/README.md` | the test runner and its options |

## Licence

GNU Affero General Public License, version 3 only (`LICENSE`). gex is
Copyright (C) 2026 Puter Technologies Inc. Third-party code keeps its own
licence, and `NOTICE` lists it.

## Contributing

This repository takes no pull requests and no issues. Both are turned off
for the BrowserPorts organisation.
