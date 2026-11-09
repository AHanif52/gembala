/// FR-77 review (pen screen 21): line diff of the recipe now vs the agent's draft. Plain LCS: recipes are small (64 KB max).
export type Line = { t: " " | "+" | "-"; s: string };

export function lineDiff(before: string, after: string): Line[] {
  const a = before ? before.replace(/\n$/, "").split("\n") : [];
  const b = after ? after.replace(/\n$/, "").split("\n") : [];
  // lcs[i][j] = longest common run of a[i..] and b[j..]
  const lcs = Array.from({ length: a.length + 1 }, () => new Array<number>(b.length + 1).fill(0));
  for (let i = a.length - 1; i >= 0; i--)
    for (let j = b.length - 1; j >= 0; j--) lcs[i][j] = a[i] === b[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
  const out: Line[] = [];
  let [i, j] = [0, 0];
  while (i < a.length || j < b.length) {
    if (i < a.length && j < b.length && a[i] === b[j]) out.push({ t: " ", s: a[i++] }), j++;
    else if (j < b.length && (i === a.length || lcs[i][j + 1] >= lcs[i + 1][j])) out.push({ t: "+", s: b[j++] });
    else out.push({ t: "-", s: a[i++] });
  }
  return out;
}
