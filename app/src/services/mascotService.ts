import type { MascotDetail, MascotSummary } from '../features/human/Mascot/backend/types';

export async function fetchMascotList(): Promise<MascotSummary[]> {
  return [];
}

export async function fetchMascotDetail(id: string): Promise<MascotDetail> {
  if (!id.trim()) throw new Error('mascot id is empty');
  throw new Error('Mascot library is not bundled in this build.');
}

/**
 * Lightweight in-memory cache for manifest fetches. The hosted mascot
 * catalogue was removed with the product backend; this cache remains so a
 * future local bundled catalogue can use the same picker surface.
 */
const detailCache = new Map<string, MascotDetail>();

export async function getCachedMascotDetail(id: string): Promise<MascotDetail> {
  const existing = detailCache.get(id);
  if (existing) return existing;
  const detail = await fetchMascotDetail(id);
  detailCache.set(id, detail);
  return detail;
}

export function clearMascotDetailCache(): void {
  detailCache.clear();
}
