/**
 * What the file pickers of the current platform can do.
 *
 * - iOS/iPadOS (WKWebView): `<input type="file">` opens a sheet with
 *   Photo Library, Take Photo and Choose File (the Files browser: On My
 *   iPhone, iCloud Drive and installed providers such as Google Drive,
 *   Dropbox and OneDrive). Folders cannot be picked.
 * - Android (WebView): the system document picker (Downloads, internal
 *   storage, Drive and other providers); `capture` opens the camera.
 *   Folders cannot be picked.
 * - Desktop (WebView2, WKWebView on macOS, WebKitGTK): the native file
 *   dialog, including folders with `webkitdirectory`. `capture` is ignored.
 */
export interface PickerPlatform {
  /** A phone or tablet: offer the camera, hide folder upload. */
  mobile: boolean;
  /** Folder selection (`webkitdirectory`) works. */
  folders: boolean;
}

export function detectPlatform(
  nav: Pick<Navigator, 'userAgent' | 'maxTouchPoints'> | undefined = typeof navigator ===
  'undefined'
    ? undefined
    : navigator,
  doc: Document | undefined = typeof document === 'undefined' ? undefined : document,
): PickerPlatform {
  const ua = nav?.userAgent ?? '';
  // iPadOS reports itself as a Mac; touch support tells them apart.
  const mobile =
    /Android|iPhone|iPad|iPod|Mobile/i.test(ua) ||
    (/Macintosh/.test(ua) && (nav?.maxTouchPoints ?? 0) > 1);
  const input = doc?.createElement('input');
  const folders = !mobile && input !== undefined && 'webkitdirectory' in input;
  return { mobile, folders };
}
