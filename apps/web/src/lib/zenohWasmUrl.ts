// @cognipilot/zenoh-wasm@1.9.0-wasm.1 ships this file but does not export the
// subpath. Use a relative asset import until the next package publish includes
// "./zenoh_wasm_bg.wasm" in package.json exports.
import zenohWasmUrl from '../../../../node_modules/@cognipilot/zenoh-wasm/zenoh_wasm_bg.wasm?url';

export default zenohWasmUrl;
