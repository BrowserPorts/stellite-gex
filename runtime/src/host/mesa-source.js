// Mesa's files for the GL device (mesa-gl.js `loadFromSource`) from the
// page's own source (MesaGl's `source`): the glue (`gex-wgpu-replay.js`),
// the module and its cold part. Without a source the device fetches them
// itself from the system's baseUrl.

/** Mesa's files, by the names the device asks for. */
export const MESA_FILES = ['gex-wgpu-replay.js', 'gex-wgpu-replay.wasm', 'gex-wgpu-replay.deferred.wasm'];

/**
 * The files a device is handed from `source` (compositor.js `hostGl`), by
 * name, or null to leave it all to the device's own fetches. The glue and
 * the module are enough: a build with no cold part has none to give.
 */
export async function mesaSources(source) {
  const got = await Promise.all(MESA_FILES.map((n) => Promise.resolve().then(() => source(n)).catch(() => null)));
  if (!got[0] || !got[1]) return null;
  return Object.fromEntries(MESA_FILES.map((n, i) => [n, got[i]]));
}
