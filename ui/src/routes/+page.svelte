<script lang="ts">
  // Fase 0.4: satu tile xterm.js tersambung ke daemon. Grid pane, split, dll. di Fase 1 (M1).
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { Terminal } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import "@xterm/xterm/css/xterm.css";

  let el: HTMLDivElement;

  const call = (method: string, params: object = {}) => invoke<any>("daemon_call", { method, params });

  const enc = new TextEncoder();
  function b64(bytes: Uint8Array): string {
    let s = "";
    for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
    return btoa(s);
  }
  const unb64 = (s: string) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));

  onMount(() => {
    const term = new Terminal({
      fontFamily: "'SF Mono', Menlo, monospace",
      fontSize: 13,
      cursorStyle: "bar",
      macOptionIsMeta: true,
      allowProposedApi: false,
      theme: { background: "#101114", foreground: "#d4d4d8" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    fit.fit();

    let pane = 0;
    let attached = false;
    const early: any[] = [];
    const onEvent = (e: any) => {
      if (e.params?.pane !== pane) return;
      if (e.event === "pane.output") term.write(unb64(e.params.data));
      if (e.event === "pane.exit") term.write(`\r\n\x1b[2m[proses keluar, kode ${e.params.code ?? "-"}]\x1b[0m\r\n`);
    };
    const unlisten = listen<any>("daemon", ({ payload }) => {
      if (payload.event === "daemon.lost") term.write("\r\n\x1b[31m[daemon terputus]\x1b[0m\r\n");
      else if (attached) onEvent(payload);
      else early.push(payload);
    });

    (async () => {
      try {
        const ids: number[] = await call("pane.list");
        if (ids.length) {
          pane = ids[ids.length - 1];
          await call("pane.resize", { pane, rows: term.rows, cols: term.cols });
        } else {
          pane = (await call("pane.create", { rows: term.rows, cols: term.cols })).pane;
        }
        // balasan attach dan event lewat jalur berbeda di Tauri: event yang datang duluan ditahan
        const a = await call("pane.attach", { pane });
        term.write(unb64(a.snapshot));
        attached = true;
        early.splice(0).forEach(onEvent);
      } catch (e) {
        term.write(`\x1b[31mgagal sambung ke daemon: ${e}\x1b[0m\r\n`);
      }
    })();

    term.onData((d) => pane && call("pane.write", { pane, data: b64(enc.encode(d)) }));
    term.onBinary((d) => pane && call("pane.write", { pane, data: btoa(d) }));
    term.onResize(({ rows, cols }) => pane && call("pane.resize", { pane, rows, cols }));
    const ro = new ResizeObserver(() => fit.fit());
    ro.observe(el);
    term.focus();

    return () => {
      ro.disconnect();
      unlisten.then((f) => f());
      term.dispose();
    };
  });
</script>

<div class="tile" bind:this={el}></div>

<style>
  :global(html, body) {
    margin: 0;
    height: 100%;
    background: #101114;
    overflow: hidden;
  }
  .tile {
    position: fixed;
    inset: 8px 0 0 10px;
  }
</style>
