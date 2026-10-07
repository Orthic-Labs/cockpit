import { bytes, type ChromeSnapshots } from "../api";

const dateText = (secs: number) => new Date(secs * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" });

/** One line about Chrome's leftover signing snapshots: how many, the change
 *  since the last check, and what moving them would really give back. */
export function ChromeSnapshotsLine({ info }: { info: ChromeSnapshots | null | undefined }) {
  if (!info || info.count === 0) return null;
  const delta = info.since_at != null && info.since_count != null ? info.count - info.since_count : null;
  const change = delta != null && delta !== 0 ? ` (${delta > 0 ? "+" : ""}${delta} since ${dateText(info.since_at as number)})` : "";
  const size = info.reclaimable_known
    ? `about ${bytes(info.reclaimable_bytes)} to gain (apparent ${bytes(info.apparent_bytes)}, shared with Chrome)`
    : `apparent ${bytes(info.apparent_bytes)}, reclaimable much less (shared with Chrome)`;
  return (
    <div className="notice small" title="Temporary copies of the Chrome app left behind when Chrome is force-quit">
      <span>
        {info.count} Chrome snapshots{change} · {size}
        {info.running && <span className="strong"> · Close Chrome to clear {info.count} snapshots</span>}
      </span>
    </div>
  );
}
