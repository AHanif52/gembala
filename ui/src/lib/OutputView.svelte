<script lang="ts">
  // FR-56: output lengkap sesi agent terakhir, baca saja, dari RAM daemon (SEC-13: tidak pernah ke disk).
  import { onMount } from "svelte";
  import { Terminal } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import "@xterm/xterm/css/xterm.css";
  import { call, unb64 } from "./daemon";

  let { pane, onclose }: { pane: number; onclose: () => void } = $props();
  let el: HTMLDivElement;

  onMount(() => {
    const term = new Terminal({
      disableStdin: true,
      scrollback: 50_000,
      fontFamily: "'SF Mono', Menlo, monospace",
      fontSize: 12,
      theme: { background: "#000000", foreground: "#e5e5e5" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    fit.fit();
    call("pane.agent_output", { pane }).then((r) => term.write(unb64(r.data)));
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onclose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      term.dispose();
    };
  });
</script>

<div class="out" role="dialog" aria-label="Output sesi agent">
  <header><span>Output sesi agent sebelumnya · baca saja</span><button onclick={onclose}>Esc</button></header>
  <div class="t" bind:this={el}></div>
</div>

<style>
  .out {
    position: absolute;
    inset: 0;
    z-index: 4;
    display: flex;
    flex-direction: column;
    background: #000000;
  }
  header {
    display: flex;
    justify-content: space-between;
    padding: 6px 10px;
    border-bottom: 1px solid var(--line);
    color: var(--muted);
    font: 12px system-ui, -apple-system, sans-serif;
  }
  button {
    border: 1px solid var(--line);
    border-radius: 4px;
    background: transparent;
    color: var(--fg);
    font: inherit;
  }
  .t {
    flex: 1;
    margin: 4px 4px 2px 8px;
  }
</style>
