export type ReleaseNotes = { version: string; bullets: string[] };
const compareVersion = (a: string, b: string) => {
  const left = a.split(".").map(Number);
  const right = b.split(".").map(Number);
  for (let i = 0; i < Math.max(left.length, right.length); i++) {
    if ((left[i] ?? 0) !== (right[i] ?? 0)) return (left[i] ?? 0) - (right[i] ?? 0);
  }
  return 0;
};

export function sections(md: string, from: string, to: string, lang: "en" | "id"): ReleaseNotes[] {
  const releases = [...md.matchAll(/^## (.+)$/gm)];
  return releases.flatMap((match, i) => {
    const version = match[1].trim();
    if (!(compareVersion(version, from) > 0 && compareVersion(version, to) <= 0)) return [];
    const body = md.slice(match.index! + match[0].length, releases[i + 1]?.index ?? md.length);
    const headings = [...body.matchAll(/^### (English|Indonesia)\s*$/gim)];
    let text = body;
    if (headings.length) {
      const wanted = lang === "en" ? "english" : "indonesia";
      const wantedHeading = headings.find((h) => h[1].toLowerCase() === wanted);
      const wantedBody = wantedHeading && body.slice(wantedHeading.index! + wantedHeading[0].length, headings.find((h) => h.index! > wantedHeading.index!)?.index ?? body.length);
      const selected = wantedBody?.split("\n").some((line) => /^\s*-\s+\S/.test(line))
        ? wantedHeading
        : lang === "id" ? headings.find((h) => h[1].toLowerCase() === "english") : wantedHeading;
      if (!selected) return [];
      text = body.slice(selected.index! + selected[0].length, headings.find((h) => h.index! > selected.index!)?.index ?? body.length);
    }
    const bullets = text.split("\n").map((line) => line.match(/^\s*-\s+(.+)$/)?.[1]).filter((line): line is string => !!line);
    return bullets.length ? [{ version, bullets }] : [];
  }).sort((a, b) => compareVersion(b.version, a.version));
}

/// FR-103: the notes to show when the app opens on `to`. Same version or a downgrade = nothing; no stored
/// version yet (first run with this feature) = `to` alone, and only when the bundled changelog has that very
/// section, so a version without notes shows no card instead of another release's list.
export function upgradeNotes(md: string, seen: string, to: string, lang: "en" | "id"): ReleaseNotes[] {
  const notes = sections(md, seen, to, lang);
  return seen ? notes : notes.filter((release) => release.version === to);
}

export function inline(text: string): { text: string; kind: "plain" | "bold" | "code" }[] {
  return text.split(/(\*\*[^*]+\*\*|`[^`]+`)/g).filter(Boolean).map((part) => ({
    text: part.replace(/^\*\*(.*)\*\*$|^`(.*)`$/, "$1$2"),
    kind: part.startsWith("**") ? "bold" : part.startsWith("`") ? "code" : "plain",
  }));
}
