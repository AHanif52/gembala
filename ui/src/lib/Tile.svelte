<script lang="ts">
  // Satu pane = satu xterm yang di-attach ke daemon (FR-1, FR-2), plus FR-34 sampai 36, 41.
  import { onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { Terminal, type IMarker, type ITheme } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import { SearchAddon } from "@xterm/addon-search";
  import { WebLinksAddon } from "@xterm/addon-web-links";
  import { Unicode11Addon } from "@xterm/addon-unicode11";
  import "@xterm/xterm/css/xterm.css";
  import { call, b64, unb64 } from "./daemon";
  import { encodeKey, onCsi } from "./kitty";
  import type { Ui } from "./config";

  let {
    pane,
    focused,
    ui,
    zoom,
    onexit,
    onlink,
    onpasted,
    online,
    onkey,
    onprompt,
    ontype,
    onfocusin,
    onused,
    onblock,
  }: {
    pane: number;
    focused: boolean;
    ui: Ui;
    zoom: number;
    onexit: (pane: number) => void;
    onlink: (url: string) => void;
    onpasted: (path: string) => void;
    online: (line: string) => void; // FR-57: teks yang diketik sejak Enter terakhir
    onkey: (key: string) => boolean; // FR-57: true = tombol dipakai daftar @, tidak diteruskan ke terminal
    onprompt: (at: boolean) => void; // FR-59: shell menunggu di prompt (penanda OSC 133 B sampai C)
    ontype: (text: string) => boolean; // FR-59: true = huruf diambil input editor, tidak diteruskan ke shell
    onfocusin: () => void; // FR-59: terminal dapat fokus keyboard (halaman memindahkannya ke editor bila tampil)
    onused: () => void; // FR-59: layar saat attach sudah berisi teks (pane bukan baru)
    onblock: (kind: "cmd" | "out" | "reuse" | "copy" | "run" | "output", text: string) => void; // FR-58: tombol aksi blok; FR-56: linimasa
  } = $props();

  // ponytail: dua palet tetap; palet 16 warna dari config menyusul bila diminta
  // palet tema Dark bawaan Warp; tanpa palet, xterm memakai warna ANSI bawaannya (biru gelap nyaris tak terbaca)
  const DARK: ITheme = {
    background: "#000000", foreground: "#ffffff", cursor: "#ffffff", selectionBackground: "#404040",
    black: "#616161", red: "#ff8272", green: "#b4fa72", yellow: "#fefdc2", blue: "#a5d5fe", magenta: "#ff8ffd", cyan: "#d0d1fe", white: "#f1f1f1",
    brightBlack: "#8e8e8e", brightRed: "#ffc4bd", brightGreen: "#d6fcb9", brightYellow: "#fefdd5", brightBlue: "#c1e3fe", brightMagenta: "#ffb1fe", brightCyan: "#e5e6fe", brightWhite: "#feffff",
  };
  // palet ANSI untuk latar terang (GitHub Light): warna xterm bawaan terlalu menyala dan tidak terbaca di putih
  const LIGHT: ITheme = {
    background: "#fafafa", foreground: "#27272a", cursor: "#27272a", selectionBackground: "#d4d4d8",
    black: "#24292f", red: "#cf222e", green: "#116329", yellow: "#7d4e00", blue: "#0969da", magenta: "#8250df", cyan: "#1b7c83", white: "#6e7781",
    brightBlack: "#57606a", brightRed: "#a40e26", brightGreen: "#1a7f37", brightYellow: "#633c01", brightBlue: "#218bff", brightMagenta: "#a475f9", brightCyan: "#3192aa", brightWhite: "#8c959f",
  };
  const MATCH = {
    matchBackground: "#854d0e",
    activeMatchBackground: "#ca8a04",
    matchOverviewRuler: "#854d0e",
    activeMatchColorOverviewRuler: "#ca8a04",
  };

  let el: HTMLDivElement;
  let term: Terminal;
  let fit: FitAddon;
  let search: SearchAddon;
  let finding = $state(false);
  let query = $state("");
  let found = $state("");
  let input = $state<HTMLInputElement>();
  const enc = new TextEncoder();

  $effect(() => {
    if (focused && ready && !finding) term.focus();
  });

  const light = matchMedia("(prefers-color-scheme: light)");
  let systemLight = $state(light.matches);
  const isLight = $derived(ui.theme === "light" || (ui.theme === "system" && systemLight));

  // FR-34: [ui] dari config berlaku langsung tanpa restart; zoom ⌘=/⌘- di atas ukuran config
  let ready = $state(false); // term bukan state (objek xterm besar); efek di bawah menunggu ini
  $effect(() => {
    if (!ready) return;
    const o = term.options;
    o.fontFamily = `'${ui.font_family.replace(/['"\\]/g, "")}', 'SF Mono', Menlo, 'Symbols Nerd Font Mono', monospace`;
    o.fontSize = Math.max(6, ui.font_size + zoom);
    o.lineHeight = ui.line_height;
    o.cursorStyle = ui.cursor;
    o.cursorBlink = ui.cursor_blink;
    o.cursorInactiveStyle = "none"; // ala Warp: terminal yang tidak fokus (atau saat input editor dipakai) tanpa kursor
    o.scrollback = ui.scrollback_lines;
    o.screenReaderMode = ui.screen_reader;
    o.macOptionIsMeta = ui.option_as_meta === "left" || ui.option_as_meta === "both";
    o.theme = isLight ? LIGHT : DARK;
    if (el.offsetParent) fit.fit();
  });

  /// FR-36: ⌘F
  export async function openFind() {
    finding = true;
    await tick();
    input?.focus();
    input?.select();
  }

  function find(back = false) {
    if (!query) return void search.clearDecorations();
    const opts = { decorations: MATCH, incremental: !back };
    if (back) search.findPrevious(query, opts);
    else search.findNext(query, opts);
  }

  function closeFind() {
    finding = false;
    search.clearDecorations();
    term.focus();
  }

  function onFindKey(e: KeyboardEvent) {
    if (e.key === "Enter") find(e.shiftKey);
    if (e.key === "Escape") closeFind();
  }

  /// FR-57: baris ketikan, hanya huruf yang diketik langsung; tombol khusus (panah, Enter, paste ber-ESC) mengosongkannya.
  /// ponytail: tidak tahu isi prompt TUI sebenarnya; cukup untuk mengenali `@` di awal baris baru
  let line = "";
  function track(d: string) {
    if (d === "\x7f") line = line.slice(0, -1);
    else if (/[\x00-\x1f]/.test(d)) line = "";
    else line += d;
    online(line);
  }
  export function resetLine() {
    line = "";
    online(line);
  }
  /// FR-57: isi prompt diubah halaman (mis. `@@q` diganti nama pane), baris yang dilacak ikut
  export function setLine(f: (line: string) => string) {
    line = f(line);
    online(line);
  }
  export const focus = () => term.focus();

  /// FR-58: blok per perintah dari penanda OSC 133 shell (A prompt, C perintah jalan + teks base64, D selesai + exit code).
  /// Hanya blok dari stream sejak attach; snapshot attach tidak memuat penanda.
  type Block = { a: IMarker; c?: IMarker; d?: IMarker; cmd: string; code?: number; t0?: number; ms?: number; live?: { dispose(): void } };
  let blocks: Block[] = [];
  let kitty: number[] = []; // tumpukan flag protokol keyboard kitty yang diminta app di pane ini
  function onMark(data: string) {
    const [k, arg = ""] = data.split(";", 2);
    const last = blocks.at(-1);
    if (k === "B") onprompt(true);
    if (k === "C") onprompt(false);
    if (k === "A") {
      kitty = []; // shell kembali ke prompt: flag keyboard app yang crash tidak ikut ke shell
      const a = term.registerMarker(0);
      if (a) blocks.push({ a, cmd: "" });
      if (blocks.length > 1000) blocks.shift()?.a.dispose(); // ponytail: 1000 blok terakhir, sisanya ikut scrollback
    } else if (k === "C" && last && !last.c) {
      last.c = term.registerMarker(0);
      last.t0 = Date.now();
      try {
        last.cmd = new TextDecoder().decode(unb64(arg));
      } catch {}
      if (last.cmd) last.live = decorate(last); // titik biru berdenyut selama jalan
    } else if (k === "D" && last?.c && !last.d) {
      last.d = term.registerMarker(0);
      last.code = Number(arg) || 0;
      if (last.t0) last.ms = Date.now() - last.t0;
      last.live?.dispose();
      decorate(last);
    }
    return true; // penanda tidak menggambar apa pun
  }
  const dur = (ms: number) => (ms < 10_000 ? `${(ms / 1000).toFixed(1).replace(".", ",")} dtk` : `${Math.round(ms / 1000)} dtk`);
  /// linimasa blok shell (Gembala.pen layar 01): garis + titik di gutter kiri luar grid, durasi/exit code di kanan baris perintah
  function decorate(b: Block) {
    const height = Math.max(1, (b.d?.line ?? b.a.line) - b.a.line);
    const d = term.registerDecoration({ marker: b.a, width: term.cols, height, layer: "top" });
    const meta = !b.d ? "" : (b.ms !== undefined ? dur(b.ms) : "") + (b.code ? ` <b>· keluar ${b.code}</b>` : "");
    d?.onRender((el) => {
      if (!el.dataset.tl) {
        el.dataset.tl = "1";
        el.className += " tl sh" + (b.code ? " fail" : "") + (b.d ? "" : " run");
        el.innerHTML = `<i class="tl-dot"></i><span class="tl-meta"></span>`;
        (el.querySelector(".tl-meta") as HTMLElement).innerHTML = meta; // hanya angka, aman
      }
      // tinggi baris bisa 0 saat render pertama: posisi dihitung ulang tiap render
      el.style.setProperty("--rh", `${rowHeight()}px`);
    });
    return d;
  }
  /// FR-56/62: blok ringkas sesi agent dan riwayat "Sesi sebelumnya" ditulis daemon sebagai teks (tanpa penanda,
  /// juga di snapshot attach): kepala blok = baris redup `<folder> (<durasi>)`, jadi garisnya dipasang dari isi buffer
  let summary: { dispose(): void }[] = [];
  function decorateSummary() {
    summary.splice(0).forEach((d) => d.dispose());
    const buf = term.buffer.active;
    if (buf.type === "alternate") return;
    // linimasa (mockup-blok-sesi E): "   [ikon ]<folder> · <durasi>[ · keluar n]"; format lama "<folder> (<durasi>)" = garis saja
    const head = /^ {0,3}(?:([✳π◎◇]) )?(\S.* · \d+ (?:dtk|mnt|jam)(?: \d+ (?:dtk|mnt))?)( · keluar (-?\d+))?$/;
    const old = /^\S.* \(\d+ (dtk|mnt|jam)[^)]*\)( · keluar (-?\d+))?$/;
    const text = (y: number) => {
      // baris yang terlipat (perintah panjang) disambung
      let t = buf.getLine(y)?.translateToString(true) ?? "";
      for (let k = y + 1; buf.getLine(k)?.isWrapped; k++) t += buf.getLine(k)!.translateToString(true);
      return t;
    };
    const heads: { y: number; fail: boolean; tl: boolean; icon?: string; info?: string; code?: string }[] = [];
    const stops: number[] = []; // "Sesi sebelumnya": blok berhenti di sini
    for (let y = 0; y < buf.length; y++) {
      const t = buf.getLine(y)?.translateToString(true) ?? "";
      if (/^\s*(── )?Sesi sebelumnya/.test(t)) stops.push(y);
      const n = head.exec(t);
      if (n) {
        heads.push({ y, tl: true, fail: !!n[4] && n[4] !== "0", icon: n[1], info: n[2], code: n[4] });
        continue;
      }
      const m = old.exec(t);
      if (m) heads.push({ y, tl: false, fail: !!m[3] && m[3] !== "0" });
    }
    const cursor = buf.baseY + buf.cursorY;
    // "Sesi sebelumnya · <tgl>" (pen layar 02): teks kecil sans + garis sampai kanan, menutupi baris terminalnya
    for (const y of stops) {
      const marker = term.registerMarker(y - cursor);
      if (!marker) continue;
      summary.push(marker);
      const label = (buf.getLine(y)?.translateToString(true) ?? "").replace(/[─\s]+$/, "").replace(/^[\s─]+/, "");
      term.registerDecoration({ marker, width: term.cols, height: 1, layer: "top" })?.onRender((el) => {
        if (!el.dataset.tl) {
          el.dataset.tl = "1";
          el.classList.add("tl-sep");
          el.append(Object.assign(document.createElement("span"), { textContent: label }), document.createElement("i"));
        }
        el.style.setProperty("--rh", `${rowHeight()}px`);
      });
    }
    heads.forEach((h, i) => {
      const end = Math.min(heads[i + 1]?.y ?? buf.length, stops.find((s) => s > h.y) ?? buf.length);
      const marker = term.registerMarker(h.y - cursor);
      if (!marker) return;
      summary.push(marker);
      if (!h.tl) {
        const d = term.registerDecoration({ marker, width: term.cols, height: Math.max(1, end - h.y), layer: "bottom" });
        d?.onRender((el) => (el.classList.add("blk"), el.classList.toggle("fail", h.fail)));
        return;
      }
      // perintah = baris tebal sesudah kepala; perintah lanjut = baris redup sesudahnya (format lama: "↳ …")
      let cmd = "";
      let resume = "";
      let ry = -1;
      for (let y = h.y + 1; y < end; y++) {
        if (buf.getLine(y)?.isWrapped) continue;
        const t = text(y).trim();
        if (!t) continue;
        if (!cmd) cmd = t;
        else if (!resume) (resume = t.replace(/^↳ /, "")), (ry = y);
      }
      // baris tombol (pen: di bawah perintah lanjut) = baris kosong pertama sesudahnya
      let by = ry + 1;
      while (ry >= 0 && buf.getLine(by)?.isWrapped) by++;
      const agent = !!h.icon;
      const [path = "", ...rest] = (h.info ?? "").split(" · ");
      // pen: "~/…/gembala/app" (dua folder terakhir)
      const parts = path.split("/");
      const short = parts.length > 3 ? `${parts[0]}/…/${parts.slice(-2).join("/")}` : path;
      if (agent && resume) {
        // kartu blok sesi (pen layar 02): latar sedikit terang dari kepala sampai baris tombol
        term.registerDecoration({ marker, width: term.cols, height: by - h.y + 2, layer: "bottom" })?.onRender((el) => el.classList.add("tl-card"));
      }
      const d = term.registerDecoration({ marker, width: term.cols, height: Math.max(1, end - h.y), layer: "top" });
      d?.onRender((el) => {
        // tinggi baris bisa 0 saat render pertama: posisi dihitung ulang tiap render (CSS memakai --rh)
        el.style.setProperty("--rh", `${rowHeight()}px`);
        if (el.dataset.tl) return;
        el.dataset.tl = "1";
        el.classList.add("tl");
        el.classList.toggle("fail", h.fail);
        el.classList.toggle("agent", agent);
        el.classList.toggle("card", agent && !!resume);
        // pen layar 02: blok sesi agent = ikon agent di kolom linimasa; blok perintah = titik
        el.innerHTML = `<i class="tl-dot"></i><div class="tl-hd"><span class="tl-info"></span><b class="tl-x"></b></div><span class="tl-acts"></span>`;
        const dot = el.querySelector(".tl-dot") as HTMLElement;
        if (agent) (dot.textContent = h.icon!), (dot.dataset.a = h.icon!);
        (el.querySelector(".tl-info") as HTMLElement).textContent = [short, ...rest].join(" · ");
        const x = el.querySelector(".tl-x") as HTMLElement;
        x.textContent = h.code && h.code !== "0" ? `· keluar ${h.code}` : agent ? "· keluar normal" : "";
        x.classList.toggle("ok", !(h.code && h.code !== "0"));
        const acts = el.querySelector(".tl-acts") as HTMLElement;
        const btn = (label: string, cls: string, run: () => void) => {
          const b = Object.assign(document.createElement("button"), { textContent: label, className: cls });
          b.addEventListener("mousedown", (e) => e.preventDefault());
          b.addEventListener("click", (e) => (e.stopPropagation(), run()));
          acts.append(b);
        };
        if (agent && resume) {
          // pen: Lanjutkan, Lihat output lengkap, Salin perintah di baris sendiri di bawah blok
          acts.classList.add("row");
          acts.style.setProperty("--k", String(by - h.y));
          btn("Lanjutkan", "pri", () => onblock("run", resume));
          btn("Lihat output lengkap", "", () => onblock("output", ""));
          btn("Salin perintah", "", () => onblock("copy", resume));
        } else if (h.fail && cmd) {
          btn("Salin", "ghost", () => onblock("copy", cmd));
          btn("Ulangi", "pri", () => onblock("run", cmd));
        }
      });
    });
  }
  const outputOf = (b: Block) => {
    if (!b.c || !b.d) return "";
    const buf = term.buffer.active;
    const rows: string[] = [];
    for (let y = b.c.line; y < Math.max(b.c.line + 1, b.d.line); y++) rows.push(buf.getLine(y)?.translateToString(true) ?? "");
    return rows.join("\n").replace(/\n+$/, "");
  };
  /// blok di baris layar `clientY` (klik kanan), atau null
  export function blockAt(clientY: number): { cmd: string; output: string; code?: number } | null {
    const rowsEl = el.querySelector(".xterm-rows");
    if (!rowsEl) return null;
    const r = rowsEl.getBoundingClientRect();
    const y = Math.floor(((clientY - r.top) / r.height) * term.rows) + term.buffer.active.viewportY;
    const b = blocks.findLast((b) => b.a.line <= y && b.d && y < Math.max(b.d.line, b.a.line + 1));
    return b ? { cmd: b.cmd, output: outputOf(b), code: b.code } : null;
  }
  // FR-58: tombol aksi blok saat kursor di atasnya, dan kepala blok menempel saat perintahnya tergulir keluar layar
  let hover = $state<{ top: number; b: Block } | null>(null);
  let sticky = $state<Block | null>(null);
  const rowHeight = () => (el.querySelector(".xterm-rows")?.getBoundingClientRect().height ?? 0) / term.rows;
  const blockOn = (y: number) => blocks.findLast((b) => b.cmd && b.a.line <= y && (!b.d || y < Math.max(b.d.line, b.a.line + 1)));
  function onHover(e: MouseEvent) {
    const rows = el.querySelector(".xterm-rows")?.getBoundingClientRect();
    if (!rows || term.buffer.active.type === "alternate") return void (hover = null);
    const vy = term.buffer.active.viewportY;
    const b = blockOn(Math.floor((e.clientY - rows.top) / rowHeight()) + vy);
    hover = b ? { top: (b.a.line - vy) * rowHeight(), b } : null;
  }
  function updateSticky() {
    const top = term.buffer.active.viewportY;
    sticky = term.buffer.active.type === "alternate" ? null : (blocks.findLast((b) => b.cmd && b.a.line < top && (!b.d || b.d.line > top)) ?? null);
  }

  /// FR-58: ⌘↑/⌘↓ lompat ke awal blok sebelum/sesudah baris teratas layar
  export function jump(dir: -1 | 1) {
    const top = term.buffer.active.viewportY;
    const b = dir < 0 ? blocks.findLast((b) => b.a.line < top) : blocks.find((b) => b.a.line > top);
    if (b) term.scrollToLine(b.a.line);
    else if (dir > 0) term.scrollToBottom();
  }

  /// FR-37: menu klik kanan
  export const selection = () => term.getSelection();
  export const paste = (text: string) => term.paste(text); // bracketed paste bila TUI memintanya

  /// FR-36: bersihkan layar dan scrollback. Shell diam juga diminta menggambar ulang prompt (Ctrl+L);
  /// bila ada proses jalan (agent), hanya scrollback yang dibuang: layar milik TUI digambar ulang per bagian,
  /// jadi layar yang dikosongkan dari luar membuat tampilan agent rusak.
  export async function clearScreen() {
    const info = await call("pane.info", { pane });
    if (info.busy) return void term.write("\x1b[3J");
    term.clear();
    call("pane.write", { pane, data: b64(new Uint8Array([0x0c])) });
  }

  // Renderer DOM bawaan xterm: terukur lebih hemat RAM dan datar per pane dibanding WebGL (TECH M1)
  onMount(() => {
    term = new Terminal({
      allowProposedApi: true, // decorations hasil cari dan Unicode 11
      // SEC-14: tautan OSC 8 tidak dibuka langsung; URL asli ditampilkan dulu
      linkHandler: { activate: (_e, uri) => onlink(uri) },
    });
    fit = new FitAddon();
    search = new SearchAddon();
    term.loadAddon(fit);
    term.loadAddon(search);
    term.loadAddon(new WebLinksAddon((_e, uri) => onlink(uri)));
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11"; // FR-41: lebar emoji dan CJK
    term.open(el);
    term.parser.registerOscHandler(133, onMark);
    // protokol keyboard kitty (flag 1): jawaban tanya dikirim ke pane seperti jawaban DA milik xterm
    for (const prefix of ["?", ">", "<", "="])
      term.parser.registerCsiHandler({ prefix, final: "u" }, (params) => {
        const reply = onCsi(kitty, prefix, params.map((p) => (Array.isArray(p) ? p[0] : p)));
        if (reply) call("pane.write", { pane, data: b64(enc.encode(reply)) });
        return true;
      });
    term.textarea?.addEventListener("focus", () => onfocusin());
    // pendengar di induk: tombol blok berada di luar elemen xterm, kursor boleh pindah ke tombol
    const host = el.parentElement!;
    const leave = () => (hover = null);
    host.addEventListener("mousemove", onHover);
    host.addEventListener("mouseleave", leave);
    term.onScroll(updateSticky);
    ready = true;
    fit.fit();

    const onTheme = () => (systemLight = light.matches);
    light.addEventListener("change", onTheme);
    search.onDidChangeResults(({ resultIndex, resultCount }) => {
      found = resultCount ? `${resultIndex + 1}/${resultCount}` : query ? "0" : "";
    });

    // FR-35: Option kiri dan kanan diatur terpisah (bawaan: kiri Meta, kanan karakter, mis. ⌥⇧2 = €).
    // xterm hanya punya satu saklar, jadi diganti tepat saat tombol Option ditekan.
    const onAlt = (e: KeyboardEvent) => {
      const m = ui.option_as_meta;
      if (e.code === "AltLeft") term.options.macOptionIsMeta = m === "left" || m === "both";
      if (e.code === "AltRight") term.options.macOptionIsMeta = m === "right" || m === "both";
    };
    el.addEventListener("keydown", onAlt, true);

    // FR-54: ⌘V berisi gambar = simpan ke folder sementara Gembala (SEC-34), tempel path-nya ber-quote.
    // Teks biasa tidak disentuh: handler paste xterm yang menanganinya (bracketed paste).
    const EXT: Record<string, string> = { "image/png": "png", "image/jpeg": "jpg", "image/gif": "gif", "image/webp": "webp" };
    const onPaste = async (e: ClipboardEvent) => {
      const item = [...(e.clipboardData?.items ?? [])].find((i) => i.kind === "file" && EXT[i.type]);
      const file = item?.getAsFile();
      if (!item || !file) return;
      e.preventDefault();
      e.stopPropagation();
      try {
        const path = await invoke<string>("save_paste", { data: b64(new Uint8Array(await file.arrayBuffer())), ext: EXT[item.type] });
        onpasted(path);
        const quoted = `'${path.replaceAll("'", "'\\''")}' `;
        call("pane.write", { pane, data: b64(enc.encode(`\x1b[200~${quoted}\x1b[201~`)) });
      } catch (err) {
        term.write(`\r\n\x1b[31mgagal menyimpan gambar: ${err}\x1b[0m\r\n`);
      }
    };
    el.addEventListener("paste", onPaste, true);

    let attached = false;
    const early: any[] = [];
    const onEvent = (e: any) => {
      if (e.event === "pane.output") term.write(unb64(e.params.data));
      // FR-56: agent keluar = layar diganti blok ringkas dari daemon
      if (e.event === "pane.replace") {
        // blok shell lama (termasuk perintah agent yang baru keluar) tidak berlaku lagi: tanpa ini titik dan durasinya
        // menumpuk di atas blok ringkas
        blocks.splice(0).forEach((b) => (b.live?.dispose(), b.a.dispose(), b.c?.dispose(), b.d?.dispose()));
        term.reset();
        term.write(unb64(e.params.data), decorateSummary);
      }
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
        const snap = unb64(a.snapshot);
        term.write(snap, decorateSummary);
        if (a.prompt) onprompt(true); // FR-59: penanda B sebelum attach tidak ikut snapshot
        if (/\S/.test(new TextDecoder().decode(snap).replace(/\x1b\[[0-9;?]*[ -\/]*[@-~]/g, ""))) onused();
        attached = true;
        early.splice(0).forEach(onEvent);
      } catch (e) {
        term.write(`\x1b[31mgagal attach pane ${pane}: ${e}\x1b[0m\r\n`);
      }
    })();

    // Shift+Enter = ESC CR (sama dengan /terminal-setup Claude Code di iTerm2): baris baru di TUI agent
    term.attachCustomKeyEventHandler((e) => {
      // FR-57: daftar @ terbuka = Tab/panah/Esc milik daftar
      if (e.type === "keydown" && ["Tab", "Enter", "ArrowUp", "ArrowDown", "Escape"].includes(e.key) && !e.metaKey && !e.shiftKey && onkey(e.key)) {
        e.preventDefault(); // Tab bawaan WebKit memindah fokus keluar dari terminal; agent menyembunyikan kursor saat fokus hilang
        return false;
      }
      const ku = (kitty.at(-1) ?? 0) & 1 ? encodeKey(e, !!term.options.macOptionIsMeta) : null;
      if (ku) {
        e.preventDefault(); // Ctrl+Enter: WebKit macOS membuka menu konteks walau kodenya sudah dikirim
        if (e.type === "keydown") (track(ku), call("pane.write", { pane, data: b64(enc.encode(ku)) }));
        return false;
      }
      // Ctrl+Enter tanpa protokol kitty = Enter biasa; tanpa ini WebKit macOS membuka menu konteks
      if (e.key === "Enter" && e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey) {
        e.preventDefault();
        if (e.type === "keydown") call("pane.write", { pane, data: b64(enc.encode("\r")) });
        return false;
      }
      if (e.key !== "Enter" || !e.shiftKey || e.metaKey || e.ctrlKey || e.altKey) return true;
      if (e.type === "keydown") call("pane.write", { pane, data: b64(enc.encode("\x1b\r")) });
      return false;
    });
    term.onData((d) => {
      // FR-59: editor tampil = huruf biasa ke editor (fokus bisa tertinggal di terminal, mis. setelah TUI keluar)
      if (!/[\x00-\x1f\x7f]/.test(d) && ontype(d)) return;
      track(d);
      call("pane.write", { pane, data: b64(enc.encode(d)) });
    });
    term.onBinary((d) => call("pane.write", { pane, data: btoa(d) }));
    term.onResize(({ rows, cols }) => call("pane.resize", { pane, rows, cols }));
    const ro = new ResizeObserver(() => el.offsetParent && fit.fit());
    ro.observe(el);
    if (focused) term.focus();

    return () => {
      host.removeEventListener("mousemove", onHover);
      host.removeEventListener("mouseleave", leave);
      ro.disconnect();
      light.removeEventListener("change", onTheme);
      el.removeEventListener("keydown", onAlt, true);
      el.removeEventListener("paste", onPaste, true);
      unlisten.then((f) => f());
      term.dispose();
    };
  });
</script>

{#snippet acts(b: Block)}
  <span class="bacts-in">
    <button class="ghost" title="Salin perintah" onclick={() => onblock("cmd", b.cmd)}>Salin</button>
    <button class="ghost" title="Salin output" onclick={() => onblock("out", outputOf(b))} disabled={!b.d}>Output</button>
    <button title="Masukkan ke editor untuk diubah lalu jalankan lagi" onclick={() => onblock("reuse", b.cmd)}>Ubah</button>
    {#if b.code}<button class="pri" title="Jalankan lagi" onclick={() => onblock("run", b.cmd)}>Ulangi</button>{/if}
  </span>
{/snippet}

<div class="term" bind:this={el}></div>
{#if sticky}
  <!-- FR-58: kepala blok menempel -->
  <div class="sticky" class:fail={!!sticky.code} class:run={!sticky.d}>
    <i class="sdot"></i>
    <span class="scmd">{sticky.cmd}</span>
    <span class="smeta"
      >{sticky.d ? (sticky.ms !== undefined ? dur(sticky.ms) : "") : "jalan"}{#if sticky.code}<span class="scode"> · keluar {sticky.code}</span>{/if}</span
    >
    {@render acts(sticky)}
  </div>
{/if}
{#if hover && hover.b !== sticky && hover.top >= 0}
  <div class="bacts" style:top="{hover.top + 10}px">{@render acts(hover.b)}</div>
{/if}
{#if finding}
  <div class="find">
    <input
      bind:this={input}
      bind:value={query}
      oninput={() => find()}
      onkeydown={onFindKey}
      placeholder="Cari"
      aria-label="Cari di pane"
      spellcheck="false"
    />
    <span class="count">{found}</span>
  </div>
{/if}

<style>
  /* jarak tepi ala Warp: teks tidak menempel ke pemisah */
  .term {
    position: absolute;
    inset: 10px 8px 6px 22px; /* kiri 22px = kolom linimasa (pen layar 02) */
    /* z-index dekorasi xterm (6-7) tetap di dalam pane: tidak menembus palet, panel Pesan, atau pemisah */
    isolation: isolate;
  }
  /* FR-58: dekorasi blok (dibuat xterm di luar komponen, jadi :global) */
  .term :global(.blk) {
    box-sizing: border-box;
    /* ala Warp: pemisah blok jelas terlihat; lebar xterm (px saat dibuat) ditimpa supaya ikut pane yang dibesarkan */
    width: 100% !important;
    border-top: 1px solid color-mix(in srgb, var(--fg) 16%, transparent);
    pointer-events: none;
  }
  .term :global(.blk.fail) {
    border-left: 3px solid #dc2626;
    background: rgba(220, 38, 38, 0.07);
  }
  /* FR-56 linimasa (mockup-blok-sesi E): garis kiri di kolom kosong 3 sel, titik per sesi, tombol di kanan */
  .term :global(.tl) {
    width: 100% !important;
    pointer-events: none;
  }
  .term :global(.tl::before) {
    content: "";
    position: absolute;
    left: -12px;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--line);
  }
  .term :global(.tl-dot) {
    position: absolute;
    top: calc(var(--rh, 20px) / 2 - 5px);
    left: -17px;
    width: 7px;
    height: 7px;
    border: 2px solid #16a34a;
    border-radius: 50%;
    background: var(--bg);
  }
  .term :global(.tl.fail .tl-dot) {
    border-color: #dc2626;
    background: #dc2626;
  }
  /* kepala blok sesi menutupi baris teks terminalnya (latar sama dengan pane) */
  .term :global(.tl-hd) {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: var(--rh, 20px);
    display: flex;
    align-items: center;
    gap: 6px;
    background: var(--bg);
    color: var(--muted);
    font: 11px system-ui, -apple-system, sans-serif;
  }
  .term :global(.tl.card .tl-hd) {
    background: color-mix(in srgb, var(--fg) 4%, var(--bg));
  }
  .term :global(.tl-x) {
    flex: 1;
    color: #f87171;
    font-weight: normal;
  }
  /* tombol blok perintah: kanan baris kepala; blok sesi agent: baris sendiri di bawah (pen layar 02) */
  .term :global(.tl-acts) {
    position: absolute;
    top: 0;
    right: 12px;
    height: var(--rh, 20px);
    display: flex;
    align-items: center;
    gap: 6px;
  }
  /* dua baris terminal untuk baris tombol: tombol setebal di pen (huruf 12px, tinggi ±1,5 baris) */
  .term :global(.tl-acts.row) {
    top: calc(var(--rh, 20px) * var(--k, 3));
    left: 0;
    right: auto;
    height: calc(var(--rh, 20px) * 2);
    gap: 8px;
  }
  .term :global(.tl-acts.row button) {
    height: max(22px, calc(var(--rh, 20px) * 1.5));
    padding: 0 12px;
    font-size: 12px;
  }
  .term :global(.tl.agent::before) {
    top: calc(var(--rh, 20px) / 2 + 9px);
  }
  .term :global(.tl.agent .tl-dot) {
    width: auto;
    height: auto;
    left: -19px;
    top: calc(var(--rh, 20px) / 2 - 8px);
    border: 0;
    border-radius: 0;
    background: none;
    font: 13px/16px system-ui, -apple-system, sans-serif;
  }
  .term :global(.tl-dot[data-a="✳"]) {
    color: #d97757;
  }
  .term :global(.tl-dot[data-a="π"]) {
    color: #a855f7;
  }
  .term :global(.tl-dot[data-a="◎"]) {
    color: #10a37f;
  }
  .term :global(.tl-dot[data-a="◇"]) {
    color: #60a5fa;
  }
  .term :global(.tl-x.ok) {
    color: var(--muted);
  }
  .term :global(.tl-card) {
    width: calc(100% + 18px) !important;
    margin-left: -18px;
    border-radius: 6px;
    /* tembus pandang: lapisan dekorasi tetap di atas teks terminal, latar solid menutupi perintahnya */
    background: color-mix(in srgb, var(--fg) 4%, transparent);
    pointer-events: none;
  }
  .term :global(.tl-sep) {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--rh, 20px) !important;
    width: 100% !important;
    background: var(--bg);
    color: color-mix(in srgb, var(--muted) 70%, transparent);
    font: 11px system-ui, -apple-system, sans-serif;
    pointer-events: none;
  }
  .term :global(.tl-sep i) {
    flex: 1;
    height: 1px;
    background: color-mix(in srgb, var(--fg) 12%, transparent);
  }
  /* tombol muat di dalam baris kepala (tidak meluber ke atas), tengah vertikal */
  .term :global(.tl-acts button) {
    display: flex;
    align-items: center;
    height: max(14px, calc(var(--rh, 20px) - 3px));
    padding: 0 9px;
    line-height: 1;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: var(--card);
    color: var(--fg);
    font: 11px system-ui, -apple-system, sans-serif;
    pointer-events: auto;
    cursor: pointer;
  }
  .term :global(.tl-acts button.pri) {
    border-color: #1d4ed8;
    background: #1e3a8a;
    color: #dbeafe;
  }
  .term :global(.tl-acts button.ghost) {
    border-color: transparent;
    background: none;
    color: var(--muted);
  }
  .term :global(.tl-acts button.ghost:hover) {
    color: var(--fg);
  }
  /* FR-58 blok shell: garis + titik di gutter kiri (di luar grid, kolom 0 dipakai prompt), durasi di kanan */
  .term :global(.tl.sh) {
    border-top: 1px solid color-mix(in srgb, var(--fg) 8%, transparent);
  }
  .term :global(.tl.sh.run .tl-dot) {
    border-color: #3b82f6;
    background: #3b82f6;
    animation: tl-pulse 1.4s ease-in-out infinite;
  }
  @keyframes -global-tl-pulse {
    50% {
      opacity: 0.35;
    }
  }
  .term :global(.tl.sh.fail) {
    background: rgba(220, 38, 38, 0.06);
  }
  .term :global(.tl-meta) {
    position: absolute;
    top: 0;
    right: 12px;
    line-height: var(--rh, 20px);
    color: var(--muted);
    font: 11px system-ui, -apple-system, sans-serif;
  }
  .term :global(.tl-meta b) {
    color: #f87171;
    font-weight: normal;
  }
  /* FR-58: tombol aksi blok (kanan baris perintah) dan kepala blok menempel (atas pane) */
  .bacts,
  .sticky {
    position: absolute;
    z-index: 2;
    font: 11px system-ui, -apple-system, sans-serif;
  }
  .bacts {
    right: 14px;
  }
  .sticky {
    top: 0;
    left: 0;
    right: 0;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 5px 12px 5px 14px;
    border-bottom: 1px solid var(--line);
    background: var(--bg);
    box-shadow: 0 4px 10px rgba(0, 0, 0, 0.25);
  }
  .sticky.fail {
    background: color-mix(in srgb, #dc2626 7%, var(--bg));
  }
  /* titik sama dengan linimasa blok: cincin hijau, merah gagal, biru berdenyut saat jalan */
  .sdot {
    flex: none;
    width: 7px;
    height: 7px;
    border: 2px solid #16a34a;
    border-radius: 50%;
  }
  .sticky.fail .sdot {
    border-color: #dc2626;
    background: #dc2626;
  }
  .sticky.run .sdot {
    border-color: #3b82f6;
    background: #3b82f6;
    animation: tl-pulse 1.4s ease-in-out infinite;
  }
  .smeta {
    color: var(--muted);
    white-space: nowrap;
  }
  .scmd {
    flex: 1;
    overflow: hidden;
    color: var(--fg);
    font: 600 12px "SF Mono", Menlo, monospace;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .scode {
    color: #f87171;
  }
  /* tombol blok ala Gembala.pen: Salin/Output polos, Ubah berbingkai, Ulangi biru */
  .bacts-in {
    display: inline-flex;
    gap: 6px;
    padding: 2px 4px;
    border-radius: 6px;
    background: var(--bg);
  }
  .bacts-in button {
    padding: 2px 8px;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: var(--card);
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }
  .bacts-in button.ghost {
    border-color: transparent;
    background: none;
    color: var(--muted);
  }
  .bacts-in button.pri {
    border-color: #1d4ed8;
    background: #1e3a8a;
    color: #dbeafe;
  }
  .bacts-in button:hover:not(:disabled) {
    color: var(--fg);
    filter: brightness(1.25);
  }
  .bacts-in button:disabled {
    opacity: 0.4;
  }
  .find {
    position: absolute;
    top: 8px;
    right: 14px;
    z-index: 3;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
    font: 12px system-ui, -apple-system, sans-serif;
  }
  .find input {
    width: 180px;
    border: 0;
    outline: 0;
    background: transparent;
    color: var(--fg);
    font: inherit;
  }
  .count {
    min-width: 32px;
    text-align: right;
    color: var(--muted);
  }
</style>
