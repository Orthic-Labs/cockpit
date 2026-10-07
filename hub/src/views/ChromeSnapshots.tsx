import { bytes, type ChromeSnapshots } from "../api";

const dateText = (secs: number) => new Date(secs * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" });

/** Chrome's leftover signing snapshots as a normal finding row: how many, the
 *  change since the last check, and what moving them would really give back. */
export function ChromeSnapshotsLine({ info }: { info: ChromeSnapshots | null | undefined }) {
  if (!info || info.count === 0) return null;
  const n = info.count;
  const delta = info.since_at != null && info.since_count != null ? n - info.since_count : null;
  const since = info.since_at != null ? dateText(info.since_at) : "";
  const change = delta == null || delta === 0 ? null : delta < 0 ? `${-delta} fewer since ${since}` : `+${delta} since ${since}`;
  const detail = !info.reclaimable_known
    ? `Apparent ${bytes(info.apparent_bytes)}, much less to free; shares space with Chrome`
    : info.reclaimable_bytes === 0
      ? "Nothing to free; shares space with Chrome"
      : `About ${bytes(info.reclaimable_bytes)} to gain (apparent ${bytes(info.apparent_bytes)}, shares space with Chrome)`;
  return (
    <div className="finding" title="Temporary copies of the Chrome app left behind when Chrome is force-quit">
      <span />
      <div className="finding-main">
        <span className="name">
          Chrome {n === 1 ? "snapshot" : "snapshots"}
          <span className="muted"> · {n}{change ? ` (${change})` : ""}</span>
        </span>
        <span className="muted small finding-reason">
          {detail}
          {info.running && ` · Close Chrome to clear ${n === 1 ? "it" : "them"}`}
        </span>
      </div>
      <span className="size strong">{info.reclaimable_known ? bytes(info.reclaimable_bytes) : ""}</span>
      <span />
    </div>
  );
}
