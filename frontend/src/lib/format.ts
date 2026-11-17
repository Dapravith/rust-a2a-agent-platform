// Formatting helpers shared by server rendering and the browser enhancement
// scripts. Server output is UTC; src/scripts/local-time.ts rewrites
// `<time data-local>` to the viewer's time zone after load.

const UTC_FORMAT = new Intl.DateTimeFormat("en-GB", {
  day: "numeric",
  month: "short",
  year: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  timeZone: "UTC",
  hour12: false,
});

export function formatUtc(iso: string): string {
  return `${UTC_FORMAT.format(new Date(iso))} UTC`;
}

export function formatLocal(iso: string): string {
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(new Date(iso));
}

/** Elapsed time between two instants, e.g. `850 ms`, `3.2 s`, `4 min 05 s`. */
export function formatDuration(start: string | null, end: string | null): string | null {
  if (!start || !end) return null;
  const ms = Math.max(0, Date.parse(end) - Date.parse(start));
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`;
  const min = Math.floor(ms / 60_000);
  const sec = Math.round((ms % 60_000) / 1000);
  return `${min} min ${String(sec).padStart(2, "0")} s`;
}

export function shortId(id: string): string {
  return id.slice(0, 8);
}

export function truncate(text: string, max: number): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1).trimEnd()}…` : flat;
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}
