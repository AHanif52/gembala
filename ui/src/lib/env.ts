// Env form rows for the profile editor. The daemon never sends values (only names), and `profile.save`
// merges per key, so an empty value means "keep the one in config.toml".

export type EnvRow = { name: string; value: string };

export const envRows = (names: string[]): EnvRow[] => names.map((name) => ({ name, value: "" }));

/// Rows with a name and a value only; `undefined` (nothing sent) means the old values stay.
export const envPatch = (rows: EnvRow[]): Record<string, string> =>
  Object.fromEntries(
    rows
      .map((r) => [r.name.trim(), r.value.trim()] as const)
      .filter(([name, value]) => name && value),
  );
