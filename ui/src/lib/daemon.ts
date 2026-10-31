import { invoke } from "@tauri-apps/api/core";

export const call = (method: string, params: object = {}) => invoke<any>("daemon_call", { method, params });

export function b64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

export const unb64 = (s: string) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
