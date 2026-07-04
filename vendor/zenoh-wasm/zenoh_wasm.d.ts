/* tslint:disable */
/* eslint-disable */

/**
 * Handle to a live subscription. Dropping it (or calling `undeclare`)
 * tears down the subscription and ends sample delivery.
 */
export class WasmSubscriber {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    keyExpr(): string | undefined;
    undeclare(): Promise<void>;
}

export class ZenohSession {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    close(): Promise<void>;
    /**
     * Declare a subscriber on `key_expr`. Every matching sample invokes
     * `callback(keyExpr: string, payload: Uint8Array, encoding: string)`.
     * The returned handle keeps the subscription alive until `undeclare()`
     * is called (or the handle is dropped).
     */
    declareSubscriber(key_expr: string, callback: Function): Promise<WasmSubscriber>;
    isClosed(): boolean;
    putBytes(key_expr: string, payload: Uint8Array): Promise<void>;
    putString(key_expr: string, payload: string): Promise<void>;
}

export function initPanicHook(): void;

export function open(endpoint: string): Promise<ZenohSession>;

export function openWithConfig(config_json5: string): Promise<ZenohSession>;

export function version(): string;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_wasmsubscriber_free: (a: number, b: number) => void;
    readonly __wbg_zenohsession_free: (a: number, b: number) => void;
    readonly open: (a: number, b: number) => any;
    readonly openWithConfig: (a: number, b: number) => any;
    readonly version: () => [number, number];
    readonly wasmsubscriber_keyExpr: (a: number) => [number, number];
    readonly wasmsubscriber_undeclare: (a: number) => any;
    readonly zenohsession_close: (a: number) => any;
    readonly zenohsession_declareSubscriber: (a: number, b: number, c: number, d: any) => any;
    readonly zenohsession_isClosed: (a: number) => number;
    readonly zenohsession_putBytes: (a: number, b: number, c: number, d: number, e: number) => any;
    readonly zenohsession_putString: (a: number, b: number, c: number, d: number, e: number) => any;
    readonly initPanicHook: () => void;
    readonly wasm_bindgen__convert__closures_____invoke__h8302226a96718918: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen__convert__closures_____invoke__h93bca70f85705cc5: (a: number, b: number, c: any, d: any) => void;
    readonly wasm_bindgen__convert__closures_____invoke__h3988072a0e8d3d30: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen__convert__closures_____invoke__h3988072a0e8d3d30_2: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen__convert__closures_____invoke__h3988072a0e8d3d30_3: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen__convert__closures_____invoke__h3988072a0e8d3d30_4: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen__convert__closures_____invoke__h6bad687c3924a02d: (a: number, b: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_destroy_closure: (a: number, b: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
