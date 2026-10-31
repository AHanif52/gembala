// Tata letak pane per tab (FR-37): pohon split biner. Fungsi murni, tanpa DOM.
// Tile diposisikan absolut dari `rects`, jadi xterm tidak dibuat ulang saat pohon berubah.

export type Dir = "row" | "col"; // row = kiri|kanan, col = atas/bawah
export type Node = { pane: number } | { dir: Dir; ratio: number; a: Node; b: Node };
export type Rect = { x: number; y: number; w: number; h: number }; // pecahan 0..1
export type Arrow = "left" | "right" | "up" | "down";
export type Divider = { path: string; dir: Dir; rect: Rect; at: number };

const FULL: Rect = { x: 0, y: 0, w: 1, h: 1 };
const MIN = 0.1;

const isLeaf = (n: Node): n is { pane: number } => "pane" in n;

function halves(n: Exclude<Node, { pane: number }>, r: Rect): [Rect, Rect] {
  return n.dir === "row"
    ? [{ ...r, w: r.w * n.ratio }, { ...r, x: r.x + r.w * n.ratio, w: r.w * (1 - n.ratio) }]
    : [{ ...r, h: r.h * n.ratio }, { ...r, y: r.y + r.h * n.ratio, h: r.h * (1 - n.ratio) }];
}

export function rects(n: Node, r: Rect = FULL, out = new Map<number, Rect>()): Map<number, Rect> {
  if (isLeaf(n)) return out.set(n.pane, r);
  const [ra, rb] = halves(n, r);
  rects(n.a, ra, out);
  return rects(n.b, rb, out);
}

/// Garis pemisah untuk di-drag. `path` = urutan a/b dari akar ke node split; `rect` = kotak node split itu.
export function dividers(n: Node, r: Rect = FULL, path = "", out: Divider[] = []): Divider[] {
  if (isLeaf(n)) return out;
  const [ra, rb] = halves(n, r);
  out.push({ path, dir: n.dir, rect: r, at: n.dir === "row" ? rb.x : rb.y });
  dividers(n.a, ra, path + "a", out);
  return dividers(n.b, rb, path + "b", out);
}

export const panes = (n: Node): number[] => [...rects(n).keys()];

export function split(n: Node, pane: number, dir: Dir, added: number): Node {
  if (isLeaf(n)) return n.pane === pane ? { dir, ratio: 0.5, a: n, b: { pane: added } } : n;
  return { ...n, a: split(n.a, pane, dir, added), b: split(n.b, pane, dir, added) };
}

export function remove(n: Node, pane: number): Node | null {
  if (isLeaf(n)) return n.pane === pane ? null : n;
  const a = remove(n.a, pane);
  const b = remove(n.b, pane);
  if (!a) return b;
  if (!b) return a;
  return { ...n, a, b };
}

export function setRatio(n: Node, path: string, ratio: number): Node {
  if (isLeaf(n)) return n;
  if (!path) return { ...n, ratio: Math.min(1 - MIN, Math.max(MIN, ratio)) };
  const [h, rest] = [path[0], path.slice(1)];
  return h === "a" ? { ...n, a: setRatio(n.a, rest, ratio) } : { ...n, b: setRatio(n.b, rest, ratio) };
}

/// Geser pemisah terdekat dari `pane` searah panah sebesar `delta` (pecahan layar penuh).
export function nudge(n: Node, pane: number, arrow: Arrow, delta = 0.05): Node {
  const dir: Dir = arrow === "left" || arrow === "right" ? "row" : "col";
  const sign = arrow === "left" || arrow === "up" ? -1 : 1;
  const own = rects(n).get(pane);
  if (!own) return n;
  // pemisah yang menyentuh pane, searah sumbu, paling dalam (kotak terkecil)
  const touching = dividers(n).filter((d) => {
    if (d.dir !== dir) return false;
    const [lo, hi] = dir === "row" ? [own.x, own.x + own.w] : [own.y, own.y + own.h];
    const [plo, phi] = dir === "row" ? [own.y, own.y + own.h] : [own.x, own.x + own.w];
    const [slo, shi] = dir === "row" ? [d.rect.y, d.rect.y + d.rect.h] : [d.rect.x, d.rect.x + d.rect.w];
    const near = (u: number, v: number) => Math.abs(u - v) < 1e-9;
    return (near(d.at, lo) || near(d.at, hi)) && plo >= slo - 1e-9 && phi <= shi + 1e-9;
  });
  if (!touching.length) return n;
  const d = touching.reduce((m, x) => (x.rect.w * x.rect.h < m.rect.w * m.rect.h ? x : m));
  const size = d.dir === "row" ? d.rect.w : d.rect.h;
  const start = d.dir === "row" ? d.rect.x : d.rect.y;
  return setRatio(n, d.path, (d.at + sign * delta - start) / size);
}

/// Pane tetangga searah panah: kotak yang menempel di sisi itu dan paling banyak bertumpuk di sumbu lain.
export function neighbor(n: Node, pane: number, arrow: Arrow): number | null {
  const all = rects(n);
  const r = all.get(pane);
  if (!r) return null;
  const eps = 1e-9;
  let best: number | null = null;
  let bestOverlap = 0;
  for (const [id, o] of all) {
    if (id === pane) continue;
    const touches =
      arrow === "left" ? Math.abs(o.x + o.w - r.x) < eps :
      arrow === "right" ? Math.abs(r.x + r.w - o.x) < eps :
      arrow === "up" ? Math.abs(o.y + o.h - r.y) < eps :
      Math.abs(r.y + r.h - o.y) < eps;
    if (!touches) continue;
    const overlap = arrow === "left" || arrow === "right"
      ? Math.min(r.y + r.h, o.y + o.h) - Math.max(r.y, o.y)
      : Math.min(r.x + r.w, o.x + o.w) - Math.max(r.x, o.x);
    if (overlap > bestOverlap + eps) {
      best = id;
      bestOverlap = overlap;
    }
  }
  return best;
}
