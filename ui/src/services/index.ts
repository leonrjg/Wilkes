import { TauriSearchApi, TauriSourceApi } from "./tauri";
import { HttpSearchApi, HttpSourceApi } from "./http";
import type { SearchApi, SourceApi } from "./api";

export const isTauri = "__TAURI_INTERNALS__" in window;

export const api: SearchApi = isTauri ? new TauriSearchApi() : new HttpSearchApi();
export const source: SourceApi = isTauri ? new TauriSourceApi() : new HttpSourceApi();
