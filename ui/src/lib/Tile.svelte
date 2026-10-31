<script lang="ts">
  // Satu pane = satu xterm yang di-attach ke daemon (FR-1, FR-2).
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { Terminal } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import { WebglAddon } from "@xterm/addon-webgl";
  import "@xterm/xterm/css/xterm.css";
  import { call, b64, unb64 } from "./daemon";

  let { pane, focused, onexit }: { pane: number; focused: boolean; onexit: (pane: number) => void } = $props();

  let el: HTMLDivElement;
  let term: Terminal;
  const enc = new TextEncoder();

  $effect(() => {
    if (focused && term) term.focus();
  });

  onMount(() => {
    term = new Terminal({
      fontFamily: "'SF Mono', Menlo, monospace",
      fontSize: 13,
      cursorStyle: "bar",
      macOptionIsMeta: true,
      theme: { background: "#101114", foreground: "#d4d4d8" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    // WebGL: satu canvas per pane, jauh lebih hemat RAM daripada renderer DOM untuk TUI berwarna.
    // Konteks hilang (GPU reset, terlalu banyak konteks) = kembali ke DOM.
    try {
      const gl = new WebglAddon();
      gl.onContextLoss(() => gl.dispose());
      term.loadAddon(gl);
    } catch {}
    fit.fit();

    let attached = false;
    const early: any[] = [];
    const onEvent = (e: any) => {
      if (e.event === "pane.output") term.write(unb64(e.params.data));
      if (e.event === "pane.exit") onexit(pane);
    };
    const unlisten = listen<any>("daemon", ({ payload }) => {
      if (payload.params?.pane !== pane) return;
      if (attached) onEvent(payload);
      else early.push(payload);
    });

    (async () => {
      try {
        await call("pane.resize", { pane, rows: term.rows, cols: term.cols });
        // balasan attach dan event lewat jalur berbeda di Tauri: event yang datang duluan ditahan
        const a = await call("pane.attach", { pane });
        term.write(unb64(a.snapshot));
        attached = true;
        early.splice(0).forEach(onEvent);
      } catch (e) {
        term.write(`\x1b[31mgagal attach pane ${pane}: ${e}\x1b[0m\r\n`);
      }
    })();

    term.onData((d) => call("pane.write", { pane, data: b64(enc.encode(d)) }));
    term.onBinary((d) => call("pane.write", { pane, data: btoa(d) }));
    term.onResize(({ rows, cols }) => call("pane.resize", { pane, rows, cols }));
    const ro = new ResizeObserver(() => el.offsetParent && fit.fit());
    ro.observe(el);
    if (focused) term.focus();

    return () => {
      ro.disconnect();
      unlisten.then((f) => f());
      term.dispose();
    };
  });
</script>

<div class="term" bind:this={el}></div>

<style>
  .term {
    position: absolute;
    inset: 6px 4px 2px 8px;
  }
</style>
