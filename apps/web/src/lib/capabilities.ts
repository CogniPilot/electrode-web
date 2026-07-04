import { writable } from 'svelte/store';

/**
 * Ground Station vs Viewer capability detection.
 *
 * The exact same built app is deployed two ways:
 *  - **Viewer** — static hosting (e.g. GitHub Pages). No local backend, so the
 *    `gcs/health` probe fails and hardware panels stay hidden.
 *  - **Ground Station** — served by the local `electrode-ground-station` daemon,
 *    which answers `gcs/*` on the same origin, so hardware panels unlock.
 *
 * Detection is same-origin and *relative to where the app is mounted*, so one
 * build works both at a GitHub Pages subpath (`/<repo>/`) and at the daemon's
 * local root (`/`).
 */

export interface GroundStationInfo {
  service: string;
  version: string;
  host?: string;
}

/** True once a local Ground Station backend has answered the health probe. */
export const isGroundStation = writable(false);
/** Metadata returned by the backend health endpoint, when present. */
export const groundStation = writable<GroundStationInfo | null>(null);

/**
 * Resolve a `gcs/<path>` URL relative to the current document location, so it
 * points at the local backend whether the app is served from `/` or `/<repo>/`.
 */
export function gcsUrl(path: string): string {
  const clean = path.replace(/^\/+/, '');
  return new URL(`gcs/${clean}`, window.location.href).toString();
}

/**
 * A `?viewer` query flag forces display-only Viewer mode even when a Ground
 * Station backend is present. Lets you open a viewer-only window (e.g.
 * `http://localhost:8790/?viewer`) alongside the full Ground Station.
 */
export function forcedViewer(): boolean {
  try {
    return new URLSearchParams(window.location.search).has('viewer');
  } catch {
    return false;
  }
}

/**
 * Probe for a local Ground Station backend. Sets {@link isGroundStation} and,
 * on success, {@link groundStation}. Never throws — absence of a backend is the
 * normal Viewer case.
 */
export async function detectGroundStation(signal?: AbortSignal): Promise<boolean> {
  if (forcedViewer()) {
    isGroundStation.set(false);
    return false;
  }
  try {
    const response = await fetch(gcsUrl('health'), { method: 'GET', signal });
    if (!response.ok) {
      isGroundStation.set(false);
      return false;
    }
    const info = (await response.json().catch(() => null)) as GroundStationInfo | null;
    groundStation.set(info);
    isGroundStation.set(true);
    return true;
  } catch {
    // No backend on this origin (Viewer), or the probe was aborted.
    isGroundStation.set(false);
    return false;
  }
}
