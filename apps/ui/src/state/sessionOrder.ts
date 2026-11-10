/**
 * Moving cards in the session order. The order covers every session; a
 * filtered list ("확인 필요", "로컬", ...) shows a part of it, so a move is
 * always relative to another card and the hidden cards keep their places.
 */
import type { SessionId } from '../protocol/types';

/** `order` with `id` moved right before (or after) `target`. */
export function moveNextTo(
  order: SessionId[],
  id: SessionId,
  target: SessionId,
  after: boolean,
): SessionId[] {
  if (id === target || !order.includes(id) || !order.includes(target)) return order;
  const rest = order.filter((x) => x !== id);
  const at = rest.indexOf(target) + (after ? 1 : 0);
  const next = [...rest.slice(0, at), id, ...rest.slice(at)];
  return next.every((x, i) => x === order[i]) ? order : next;
}

/**
 * `order` with `id` moved one card up (`-1`) or down (`1`) among `visible`
 * (the cards the person sees; all of them by default).
 */
export function moveBy(
  order: SessionId[],
  id: SessionId,
  offset: -1 | 1,
  visible: SessionId[] = order,
): SessionId[] {
  const at = visible.indexOf(id);
  const neighbour = visible[at + offset];
  if (at < 0 || neighbour === undefined) return order;
  return moveNextTo(order, id, neighbour, offset > 0);
}

/** `order` with `id` moved to `index` among `visible`. */
export function moveTo(
  order: SessionId[],
  id: SessionId,
  index: number,
  visible: SessionId[] = order,
): SessionId[] {
  const from = visible.indexOf(id);
  if (from < 0 || index === from) return order;
  const others = visible.filter((x) => x !== id);
  const clamped = Math.max(0, Math.min(index, others.length));
  if (others.length === 0) return order;
  return clamped < others.length
    ? moveNextTo(order, id, others[clamped]!, false)
    : moveNextTo(order, id, others[others.length - 1]!, true);
}

/**
 * `order` with the cards of `arranged` put in that order, on the places
 * those cards had; cards not in `arranged` (the conductor's session, which
 * has no card) keep theirs.
 */
export function placeVisible(order: SessionId[], arranged: SessionId[]): SessionId[] {
  const wanted = new Set(arranged);
  let k = 0;
  const next = order.map((id) => (wanted.has(id) ? arranged[k++]! : id));
  const rest = arranged.slice(k).filter((id) => !order.includes(id));
  const result = [...next, ...rest];
  return result.length === order.length && result.every((x, i) => x === order[i]) ? order : result;
}
