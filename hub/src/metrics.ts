// Live system history for the graphs. The Rust side (metrics_history.rs)
// samples every 2 s and keeps 30 minutes, even while the window is closed.
// This module loads that once, then polls only what is new while something
// is subscribed.

import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";

export interface Sample {
  /** Unix time, ms. */
  ts: number;
  /** Overall CPU, 0–100. */
  cpu: number;
  memUsed: number;
  memTotal: number;
  swapUsed: number;
  swapTotal: number;
  pressure: string | null;
  /** Bytes per second; null while unread. */
  netDown: number | null;
  netUp: number | null;
}

export interface Metrics {
  samples: Sample[];
  latest: Sample | null;
}

const POLL_MS = 2000;
const KEEP_MS = 30 * 60 * 1000;

let state: Metrics = { samples: [], latest: null };
const listeners = new Set<() => void>();
let timer: ReturnType<typeof setInterval> | null = null;
let inFlight = false;

function merge(fresh: Sample[]) {
  if (fresh.length === 0) return;
  const last = state.latest?.ts ?? 0;
  const added = fresh.filter((s) => s.ts > last);
  if (added.length === 0) return;
  const all = state.samples.concat(added);
  const cutoff = all[all.length - 1].ts - KEEP_MS;
  const samples = all.filter((s) => s.ts >= cutoff);
  state = { samples, latest: samples[samples.length - 1] };
  listeners.forEach((l) => l());
}

async function poll() {
  if (inFlight) return;
  inFlight = true;
  try {
    const since = state.latest?.ts;
    merge(await invoke<Sample[]>("metrics_history", { sinceMs: since ?? null }));
  } catch {
    // The next tick tries again; the graphs keep what they have.
  } finally {
    inFlight = false;
  }
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  if (listeners.size === 1) {
    poll();
    timer = setInterval(poll, POLL_MS);
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0 && timer != null) {
      clearInterval(timer);
      timer = null;
    }
  };
}

const snapshot = () => state;

/** The last 30 minutes of samples (oldest first) and the newest one. */
export function useMetrics(): Metrics {
  return useSyncExternalStore(subscribe, snapshot);
}

/** Samples from the last `minutes`, measured back from the newest sample. */
export function lastMinutes(samples: Sample[], minutes: number): Sample[] {
  if (samples.length === 0) return samples;
  const cutoff = samples[samples.length - 1].ts - minutes * 60_000;
  const first = samples.findIndex((s) => s.ts >= cutoff);
  return first <= 0 ? samples : samples.slice(first);
}

export const memPercent = (s: Sample) => (s.memTotal > 0 ? (s.memUsed / s.memTotal) * 100 : 0);
export const swapPercent = (s: Sample) => (s.swapTotal > 0 ? (s.swapUsed / s.swapTotal) * 100 : 0);
