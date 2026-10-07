// Kinds and the treemap layout for the Storage view.
//
// Adapted from Petal (https://github.com/henrydennis/petal, MIT; vendor/petal):
//  - the nine kinds and their colours are Petal's `src/classify/mod.rs` and
//    `classify/model.rs`. Petal judges kind with a small trained model; this
//    uses plain path rules instead, so a wrong guess only costs a wrong
//    colour. It never decides what is safe to delete.
//  - `squarify` is the squarified treemap (Bruls, Huizing and van Wijk) that
//    Petal's `src/treemap.rs` lays tiles out with, rewritten for SVG.

export type KindId =
  | "apps"
  | "downloads"
  | "developer"
  | "caches"
  | "media"
  | "documents"
  | "appdata"
  | "system"
  | "mixed";

export const KINDS: Record<KindId, { label: string; color: string }> = {
  apps: { label: "Apps", color: "#d95926" },
  downloads: { label: "Downloads & Trash", color: "#3987e5" },
  developer: { label: "Developer files", color: "#008300" },
  caches: { label: "Caches & logs", color: "#199e70" },
  media: { label: "Photos, video & music", color: "#d55181" },
  documents: { label: "Documents", color: "#c98500" },
  appdata: { label: "App data & backups", color: "#9085e9" },
  system: { label: "System", color: "#5b5e66" },
  mixed: { label: "Other", color: "#6b6e78" },
};

const MEDIA_EXT = /\.(mov|mp4|m4v|mkv|avi|mp3|m4a|wav|flac|jpg|jpeg|png|heic|raw|cr2|dng|photoslibrary|musiclibrary|fcpbundle)$/i;
const DEV_NAMES = new Set([
  "node_modules", "target", ".build", "DerivedData", ".git", ".cargo", ".rustup", ".npm", ".pnpm-store",
  ".gradle", ".nvm", "Developer", "Pods", "venv", ".venv", "dist",
]);

export function kindOf(path: string, isDir: boolean): KindId {
  const name = path.split("/").pop() ?? "";
  const rel = path.replace(/^\/Users\/[^/]+/, "~");
  if (rel.includes("/Library/Caches") || rel.includes("/Library/Logs") || name === ".cache" || name === "Caches") return "caches";
  if (rel.startsWith("/Applications") || /\.app$/.test(name)) return "apps";
  if (DEV_NAMES.has(name) || rel.includes("/Library/Developer") || rel.includes("/node_modules/")) return "developer";
  if (rel.startsWith("~/Downloads") || rel.startsWith("~/.Trash")) return "downloads";
  if (MEDIA_EXT.test(name) || /^~\/(Movies|Music|Pictures)(\/|$)/.test(rel)) return "media";
  if (/^~\/(Documents|Desktop)(\/|$)/.test(rel)) return "documents";
  if (rel.startsWith("~/Library")) return "appdata";
  if (/^\/(System|private|usr|bin|sbin|var|opt|cores|Library)(\/|$)/.test(rel)) return "system";
  return isDir ? "mixed" : "documents";
}

export interface Tile {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Squarified treemap. `values` must be positive and sorted largest first. */
export function squarify(values: number[], W: number, H: number): Tile[] {
  const total = values.reduce((a, b) => a + b, 0);
  const out: Tile[] = values.map(() => ({ x: 0, y: 0, w: 0, h: 0 }));
  if (total <= 0) return out;
  const areas = values.map((v) => (v / total) * W * H);
  const sum = (row: number[]) => row.reduce((a, b) => a + b, 0);
  const worst = (row: number[], side: number) => {
    const s = sum(row);
    return Math.max((side * side * Math.max(...row)) / (s * s), (s * s) / (side * side * Math.min(...row)));
  };
  let x = 0, y = 0, w = W, h = H, i = 0;
  while (i < areas.length) {
    const side = Math.min(w, h);
    const row = [areas[i]];
    let j = i + 1;
    while (j < areas.length && worst([...row, areas[j]], side) <= worst(row, side)) {
      row.push(areas[j]);
      j++;
    }
    const s = sum(row);
    if (w >= h) {
      const cw = s / h;
      let cy = y;
      row.forEach((a, k) => {
        const ch = a / cw;
        out[i + k] = { x, y: cy, w: cw, h: ch };
        cy += ch;
      });
      x += cw;
      w -= cw;
    } else {
      const rh = s / w;
      let cx = x;
      row.forEach((a, k) => {
        const cw = a / rh;
        out[i + k] = { x: cx, y, w: cw, h: rh };
        cx += cw;
      });
      y += rh;
      h -= rh;
    }
    i = j;
  }
  return out;
}
