import { writable } from 'svelte/store';
import type { MappingProfile } from '$lib/gcs';

/**
 * The current RC mapping profile, shared across components. The RC Mapping
 * editor writes it on load and on every edit; the Manual Link "Hardware" view
 * reads it so remapping an axis takes effect immediately (no stale copy).
 */
export const mappingProfile = writable<MappingProfile | null>(null);
