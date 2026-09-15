export function reorderTaskIds(
  currentIds: readonly string[],
  movingIds: readonly string[],
  targetId: string,
  placeAfter: boolean,
): string[] {
  const movingSet = new Set(movingIds);
  const moving = currentIds.filter((id) => movingSet.has(id));
  if (!moving.length || movingSet.has(targetId)) return [...currentIds];
  const remaining = currentIds.filter((id) => !movingSet.has(id));
  const targetIndex = remaining.indexOf(targetId);
  if (targetIndex < 0) return [...currentIds];
  const insertAt = targetIndex + (placeAfter ? 1 : 0);
  return [...remaining.slice(0, insertAt), ...moving, ...remaining.slice(insertAt)];
}

export function mergeVisibleTaskOrder(
  fullOrder: readonly string[],
  visibleBefore: readonly string[],
  visibleAfter: readonly string[],
): string[] {
  const visibleSet = new Set(visibleBefore);
  const reordered = visibleAfter.filter((id) => visibleSet.has(id));
  let visibleIndex = 0;
  const merged = fullOrder.map((id) => visibleSet.has(id) ? reordered[visibleIndex++] : id);
  return [...merged, ...reordered.slice(visibleIndex)];
}

export function applyStoredTaskOrder<T extends { id: string }>(items: readonly T[], storedIds: readonly string[]): T[] {
  const byId = new Map(items.map((item) => [item.id, item]));
  const storedSet = new Set(storedIds);
  const newItems = items.filter((item) => !storedSet.has(item.id));
  const storedItems = storedIds.flatMap((id) => {
    const item = byId.get(id);
    return item ? [item] : [];
  });
  return [...newItems, ...storedItems];
}
