/**
 * Dragging files from Explorer, Finder or a file manager onto a session.
 *
 * The desktop shell turns off Tauri's native drag and drop handler
 * (`dragDropEnabled: false`), so WebView2, WKWebView and WebKitGTK deliver
 * standard HTML5 drag events with `File` objects, exactly like a browser.
 *
 * A session pane (`LiveView`) is the drop zone; the composer inside it
 * registers what a drop does (the same upload as the paperclip). Without a
 * zone around it the composer takes drops itself.
 */
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type DragEvent,
} from 'react';

/** What a drop does with the dropped data (read synchronously: it expires after the event). */
export type DropHandler = (data: DataTransfer) => void;

/** True for drags that carry files or folders (not text, links or in-page items). */
export function dragHasFiles(data: Pick<DataTransfer, 'types'> | null | undefined): boolean {
  const types = data?.types;
  if (!types) return false;
  return Array.from(types as ArrayLike<string>).includes('Files');
}

export interface DropProps {
  onDragEnter(e: DragEvent): void;
  onDragOver(e: DragEvent): void;
  onDragLeave(e: DragEvent): void;
  onDrop(e: DragEvent): void;
}

export interface FileDrag {
  /** Files are being dragged over the zone and it would take them. */
  dragging: boolean;
  dropProps: DropProps;
}

/**
 * Drag state of one drop zone. `onDrop` null: the zone takes nothing now
 * (events pass through, so an outer zone or the page guard sees them).
 */
export function useFileDrag(onDrop: DropHandler | null): FileDrag {
  const [dragging, setDragging] = useState(false);
  const depth = useRef(0);
  const handler = useRef(onDrop);
  useEffect(() => {
    handler.current = onDrop;
  });
  const accepting = onDrop !== null;

  const reset = useCallback(() => {
    depth.current = 0;
    setDragging(false);
  }, []);

  // Attaching may become unavailable mid-drag (disconnect, session ended).
  const shown = dragging && accepting;

  // A drag that ends outside the zone (or is cancelled) may not leave it cleanly.
  useEffect(() => {
    if (!dragging) return;
    const end = () => reset();
    window.addEventListener('drop', end);
    window.addEventListener('dragend', end);
    return () => {
      window.removeEventListener('drop', end);
      window.removeEventListener('dragend', end);
    };
  }, [dragging, reset]);

  const dropProps: DropProps = {
    onDragEnter: (e) => {
      if (!handler.current || !dragHasFiles(e.dataTransfer)) return;
      e.preventDefault();
      // A drag cut short while drops were off may have left a stale count.
      if (!shown) depth.current = 0;
      depth.current += 1;
      setDragging(true);
    },
    onDragOver: (e) => {
      if (!handler.current || !dragHasFiles(e.dataTransfer)) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = 'copy';
    },
    onDragLeave: () => {
      if (depth.current === 0) return;
      depth.current -= 1;
      if (depth.current === 0) setDragging(false);
    },
    onDrop: (e) => {
      reset();
      const take = handler.current;
      if (!take || !e.dataTransfer || !dragHasFiles(e.dataTransfer)) return;
      e.preventDefault();
      // Keep the page guard and outer zones out of it.
      e.stopPropagation();
      take(e.dataTransfer);
    },
  };

  return { dragging: shown, dropProps };
}

type Register = (handler: DropHandler | null) => void;

const FileDropContext = createContext<Register | null>(null);

export const FileDropProvider = FileDropContext.Provider;

/** A pane that takes file drops for the composer inside it. */
export function useFileDropZone(): FileDrag & { register: Register } {
  const [handler, setHandler] = useState<DropHandler | null>(null);
  const register = useCallback<Register>((h) => setHandler(() => h), []);
  return { ...useFileDrag(handler), register };
}

/**
 * The composer's side: hands drops to the enclosing zone while `handler` is
 * set. Returns whether there is such a zone (without one, the caller handles
 * drops itself).
 */
export function useFileDropTarget(handler: DropHandler | null): boolean {
  const register = useContext(FileDropContext);
  const latest = useRef(handler);
  useEffect(() => {
    latest.current = handler;
  });
  const active = handler !== null;
  useEffect(() => {
    if (!register) return;
    register(active ? (data) => latest.current?.(data) : null);
    return () => register(null);
  }, [register, active]);
  return register !== null;
}

/**
 * Files dropped anywhere else in the window must not make the webview open
 * them (it would navigate away from the app): such drops show a "no drop"
 * cursor and do nothing. Text and link drags are left alone.
 */
export function installFileDropGuard(target: Window = window): () => void {
  const onDragOver = (e: globalThis.DragEvent) => {
    if (e.defaultPrevented || !dragHasFiles(e.dataTransfer)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'none';
  };
  const onDrop = (e: globalThis.DragEvent) => {
    if (dragHasFiles(e.dataTransfer)) e.preventDefault();
  };
  target.addEventListener('dragover', onDragOver);
  target.addEventListener('drop', onDrop);
  return () => {
    target.removeEventListener('dragover', onDragOver);
    target.removeEventListener('drop', onDrop);
  };
}

/** Files pasted into the composer (Ctrl+V / Cmd+V), or none for a text paste. */
export function pastedFiles(data: DataTransfer | null): File[] {
  if (!data) return [];
  const files = Array.from(data.files ?? []);
  if (files.length === 0) return [];
  // Spreadsheet cells and rich text also carry a picture: text wins.
  if (Array.from(data.types ?? []).includes('text/plain')) return [];
  return files;
}
