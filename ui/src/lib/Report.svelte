<script lang="ts">
  // FR-100, SEC-40: "Laporkan masalah". Gembala only builds the text; the user opens GitHub's New issue page in the browser
  // (already logged in there) and presses Submit. No token, no API call, nothing sent by the app or by an agent.
  import { invoke } from "@tauri-apps/api/core";
  import { call } from "./daemon";
  import { t } from "./i18n/state.svelte";

  let {
    draft,
    version,
    mode,
    home,
    onclose,
    onsent,
  }: {
    /// from an agent's `gembala report` (already masked by the daemon) or empty when the user starts one
    draft: { title: string; body: string; from?: string };
    version: string;
    mode: string;
    home: string;
    onclose: () => void;
    onsent: () => void;
  } = $props();

  // the dialog is opened fresh each time, so these start from the props once on purpose
  // svelte-ignore state_referenced_locally
  let title = $state(draft.title);
  // svelte-ignore state_referenced_locally
  let text = $state(draft.body);
  let withLog = $state(false);
  let env = $state<any>(null);
  let note = $state("");
  // an agent asked for a screenshot: the button is lit, the user still decides
  let suggestShot = $state(false);

  const REPO = "https://github.com/AHanif52/gembala/issues/new";
  // ponytail: GitHub refuses very long URLs; ~6,000 encoded characters of body fits, the rest goes through the clipboard
  const URL_BODY_MAX = 6000;

  async function loadEnv() {
    const r = await call("report.env", { log: withLog }).catch(() => null);
    if (r && !r.error) {
      if (r.screenshot) suggestShot = true;
      env = r;
    }
  }
  loadEnv();

  const strip = (s: string) => (home.length > 1 ? s.split(home).join("~") : s);
  const envLines = $derived(
    env
      ? [
          `- Gembala ${version} · daemon ${env.daemon} · protocol ${env.proto}`,
          `- macOS ${env.macos ?? "?"}`,
          `- Agents: ${(env.agents ?? []).map((a: any) => `${a.name} ${a.version ?? ""}`.trim()).join(", ") || "none"}`,
          `- Mode: ${mode}`,
        ].join("\n")
      : "",
  );
  // English headings on purpose: the issue lands on the public repo; the user's own words stay as typed
  const body = $derived(
    [`### What happened\n${strip(text.trim()) || "-"}`, `### Environment\n${envLines}`, withLog && env?.log ? `<details><summary>Gembala log (last 50 lines, secrets masked)</summary>\n\n\`\`\`\n${env.log}\n\`\`\`\n</details>` : ""]
      .filter(Boolean)
      .join("\n\n"),
  );
  const url = (b: string) => `${REPO}?title=${encodeURIComponent(strip(title.trim()))}&body=${encodeURIComponent(b)}`;

  async function copy() {
    note = await invoke("copy_text", { text: `${strip(title.trim())}\n\n${body}` }).then(() => t("common.copied"), () => t("chg.copyFailed"));
  }
  async function open() {
    let b = body;
    // too long for the URL: the log goes first, then the text is cut; the whole report is on the clipboard either way
    if (encodeURIComponent(b).length > URL_BODY_MAX) {
      await invoke("copy_text", { text: body }).catch(() => {});
      b = [`### What happened\n${strip(text.trim())}`, `### Environment\n${envLines}`, t("rep.cut")].join("\n\n");
      while (encodeURIComponent(b).length > URL_BODY_MAX) b = b.slice(0, Math.floor(b.length * 0.9));
    }
    await invoke("open_url", { url: url(b) }).catch(() => {});
    onsent();
  }
  async function shot() {
    note = await invoke("shot_window").then(() => t("rep.shotDone"), () => t("rep.shotFailed"));
    suggestShot = false;
  }
  function key(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      onclose();
    }
  }
</script>

<svelte:window onkeydowncapture={key} />

<div class="veil" role="presentation" onmousedown={onclose}></div>
<div class="card" role="dialog" aria-label={t("rep.title")} aria-modal="true">
  <div class="head"><b>{t("rep.title")}</b><span class="m">{draft.from ? t("rep.fromAgent", { name: draft.from }) : t("rep.sub")}</span></div>
  <label class="col">{t("rep.titleLabel")}<input bind:value={title} maxlength="120" /></label>
  <label class="col">{t("rep.what")}<textarea rows="5" bind:value={text} placeholder={t("rep.whatPh")}></textarea></label>
  <label class="chk"><input type="checkbox" bind:checked={withLog} onchange={loadEnv} /> {t("rep.log")}</label>
  <div class="safe">{t("rep.safe")}</div>
  <pre aria-label={t("rep.preview")}>{body}</pre>
  <div class="acts">
    <span class="m grow">{note}</span>
    <button class="mini" class:pri={suggestShot} onclick={shot} title={t("rep.shotTip")}>{t("rep.shot")}</button>
    <button class="mini" onclick={onclose}>{t("common.cancel")}</button>
    <button class="mini" onclick={copy}>{t("wel.copy")}</button>
    <button class="mini go" disabled={!title.trim()} onclick={open}>{t("rep.open")} ↗</button>
  </div>
</div>

<style>
  .veil {
    position: absolute;
    inset: 0;
    z-index: 7;
    background: rgba(0, 0, 0, 0.35);
  }
  .card {
    position: absolute;
    left: 50%;
    top: 48px;
    transform: translateX(-50%);
    z-index: 8;
    width: min(600px, 92%);
    max-height: calc(100% - 96px);
    overflow: auto;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px 16px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--card);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
  }
  .head {
    display: flex;
    gap: 8px;
    align-items: baseline;
    font-size: 14px;
  }
  .m {
    color: var(--muted);
    font-size: 12px;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 4px;
    color: var(--muted);
    font-size: 12px;
  }
  .chk {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  textarea,
  input:not([type="checkbox"]) {
    box-sizing: border-box;
    width: 100%;
    padding: 6px 8px;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: var(--bg);
    color: var(--fg);
    font: 13px system-ui, -apple-system, sans-serif;
  }
  textarea {
    resize: vertical;
  }
  .safe {
    padding: 6px 9px;
    border: 1px solid var(--line);
    border-radius: 6px;
    color: var(--muted);
    font-size: 12px;
  }
  pre {
    margin: 0;
    max-height: 200px;
    overflow: auto;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--bg);
    white-space: pre-wrap;
    font: 11.5px "SF Mono", Menlo, monospace;
  }
  .acts {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .grow {
    flex: 1;
  }
  .mini {
    padding: 4px 10px;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: transparent;
    color: var(--fg);
    font: 12px system-ui, -apple-system, sans-serif;
    white-space: nowrap;
    cursor: pointer;
  }
  .mini.pri,
  .mini.go {
    border-color: var(--blue, #3b82f6);
  }
  .mini:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
