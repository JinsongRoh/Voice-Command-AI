import { useLayoutEffect, type RefObject } from 'react';

/** Room the conversation keeps above the composer while the text box grows. */
export const MIN_CHAT_PX = 120;

/**
 * Keeps the composer's text box from crowding out the conversation: sets
 * `--editor-max` on the live view to the height the box may take while the
 * chat area (`[data-live-content]`) keeps `minChat` pixels, and
 * `--editor-room` to all the height it can take (the chat shrunk to nothing;
 * used while typing with the on-screen keyboard open). The box's CSS
 * `min-height` still wins, so on very small screens the chat gives way and
 * the composer's buttons never leave the screen.
 */
export function useEditorCap(ref: RefObject<HTMLElement | null>, minChat = MIN_CHAT_PX): void {
  useLayoutEffect(() => {
    const live = ref.current;
    if (!live || typeof ResizeObserver === 'undefined') return;
    let last = '';
    let lastRoom = '';
    const update = () => {
      const content = live.querySelector<HTMLElement>('[data-live-content]');
      const box = live.querySelector<HTMLElement>('textarea');
      if (!content || !box) return;
      const total = live.clientHeight;
      if (total === 0) return;
      // Everything except the chat and the text box (stable while the box resizes).
      const others =
        total - content.getBoundingClientRect().height - box.getBoundingClientRect().height;
      const cap = `${Math.max(0, Math.floor(total - others - minChat))}px`;
      if (cap !== last) {
        last = cap;
        live.style.setProperty('--editor-max', cap);
      }
      const room = `${Math.max(0, Math.floor(total - others))}px`;
      if (room !== lastRoom) {
        lastRoom = room;
        live.style.setProperty('--editor-room', room);
      }
    };
    const resize = new ResizeObserver(update);
    const observeChildren = () => {
      resize.disconnect();
      resize.observe(live);
      for (const child of Array.from(live.children)) resize.observe(child);
    };
    observeChildren();
    // A decision card or an error line appearing changes the room as well.
    const mutations = new MutationObserver(() => {
      observeChildren();
      update();
    });
    mutations.observe(live, { childList: true });
    update();
    return () => {
      resize.disconnect();
      mutations.disconnect();
    };
  }, [ref, minChat]);
}
