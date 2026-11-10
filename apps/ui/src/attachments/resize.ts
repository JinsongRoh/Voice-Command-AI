/**
 * Client-side preparation of photos before upload, sized for modern phone
 * cameras (48 to 200 MP, 5 to 30 MB files).
 *
 * - The decoder is asked for a downscaled bitmap (`createImageBitmap` with
 *   `resizeWidth`/`resizeHeight`), so a 200 MP photo is never held at full
 *   size. The pixel size is read from the file header first.
 * - The long edge is limited to `MAX_IMAGE_EDGE`; the result is JPEG at
 *   `JPEG_QUALITY` (PNG stays PNG only when it has transparency). Re-encoding
 *   through a canvas also drops EXIF and GPS data.
 * - Small JPEGs are sent byte for byte. GIF, SVG and RAW (DNG) are never
 *   touched. An image the webview cannot decode (HEIC on Android WebView) is
 *   sent as is; the caller applies the file size cap.
 * - A small preview (`THUMB_EDGE`) is made from the same decode, so one
 *   bitmap is alive at a time and it is closed before the next photo starts.
 */

export const MAX_IMAGE_EDGE = 2560;
export const JPEG_QUALITY = 0.82;
/** Longest edge of a chip preview. */
export const THUMB_EDGE = 256;
const THUMB_QUALITY = 0.7;
/** A JPEG up to this size and edge is sent unchanged. */
export const KEEP_ORIGINAL_BYTES = 1_500_000;
/** A PNG over this size becomes JPEG unless it has transparency. */
export const PNG_TO_JPEG_BYTES = 1_500_000;
/** Without a readable header, files above this are decoded downscaled. */
const UNKNOWN_SIZE_SCALE_BYTES = 4 * 1024 * 1024;

/** Decoded image, independent of the decoding API. */
export interface DecodedImage {
  width: number;
  height: number;
  source: CanvasImageSource;
  /** The decoder was asked for a downscaled bitmap (`resizeWidth` used). */
  scaled?: boolean;
  close?(): void;
}

export interface DecodeOptions {
  /** Decode no larger than this long edge. */
  maxEdge?: number;
}

/** The browser APIs used for resizing; replaced in tests. */
export interface ImageCodec {
  decode(file: Blob, options?: DecodeOptions): Promise<DecodedImage>;
  encode(
    image: DecodedImage,
    width: number,
    height: number,
    type: string,
    quality: number,
  ): Promise<Blob>;
  /** Any non-opaque pixel? Absent: assumed yes (keeps PNG). */
  hasAlpha?(image: DecodedImage): boolean;
}

/** Raster formats a canvas can usually redraw (not GIF: animation; not SVG: vector; not RAW). */
const RESIZABLE = /^image\/(jpeg|jpg|pjpeg|png|webp|heic|heif|bmp|avif|tiff)$/i;
const HEIF = /^image\/(heic|heif)$/i;

export function isResizable(type: string): boolean {
  return RESIZABLE.test(type);
}

export function isHeif(type: string): boolean {
  return HEIF.test(type);
}

/** Infers the MIME type from the extension when the picker left `type` empty. */
export function guessImageType(name: string): string {
  const ext = /\.([a-z0-9]+)$/i.exec(name)?.[1]?.toLowerCase();
  switch (ext) {
    case 'jpg':
    case 'jpeg':
    case 'jpe':
      return 'image/jpeg';
    case 'png':
      return 'image/png';
    case 'heic':
      return 'image/heic';
    case 'heif':
      return 'image/heif';
    case 'webp':
      return 'image/webp';
    case 'gif':
      return 'image/gif';
    case 'avif':
      return 'image/avif';
    case 'bmp':
      return 'image/bmp';
    case 'tif':
    case 'tiff':
      return 'image/tiff';
    case 'dng':
      return 'image/x-adobe-dng';
    default:
      return '';
  }
}

/** Target size keeping the aspect ratio, or `null` when already small enough. */
export function fitWithin(
  width: number,
  height: number,
  max = MAX_IMAGE_EDGE,
): { width: number; height: number } | null {
  const long = Math.max(width, height);
  if (long <= max || long <= 0) return null;
  const scale = max / long;
  return {
    width: Math.max(1, Math.round(width * scale)),
    height: Math.max(1, Math.round(height * scale)),
  };
}

function renamed(name: string, type: string): string {
  const ext = type === 'image/png' ? 'png' : 'jpg';
  const dot = name.lastIndexOf('.');
  return `${dot > 0 ? name.slice(0, dot) : name}.${ext}`;
}

// ---- Header probe: pixel size without decoding ------------------------------

async function headBytes(file: Blob, n: number): Promise<Uint8Array> {
  const part = file.slice(0, n);
  if (typeof part.arrayBuffer === 'function') return new Uint8Array(await part.arrayBuffer());
  return new Uint8Array(await new Response(part).arrayBuffer());
}

const u16 = (b: Uint8Array, i: number) => ((b[i] ?? 0) << 8) | (b[i + 1] ?? 0);
const u32 = (b: Uint8Array, i: number) => u16(b, i) * 65536 + u16(b, i + 2);
const ascii = (b: Uint8Array, i: number, s: string) =>
  [...s].every((c, k) => b[i + k] === c.charCodeAt(0));

function exifOrientation(b: Uint8Array, start: number, end: number): number {
  // start: first byte of the TIFF header.
  const le = b[start] === 0x49;
  const r16 = (i: number) => (le ? (b[i] ?? 0) | ((b[i + 1] ?? 0) << 8) : u16(b, i));
  const r32 = (i: number) => (le ? r16(i) + r16(i + 2) * 65536 : u32(b, i));
  const ifd = start + r32(start + 4);
  if (ifd + 2 > end) return 1;
  const count = r16(ifd);
  for (let k = 0; k < count; k++) {
    const e = ifd + 2 + k * 12;
    if (e + 12 > end) break;
    if (r16(e) === 0x0112) return r16(e + 8);
  }
  return 1;
}

function jpegSize(b: Uint8Array): { width: number; height: number } | null {
  let i = 2;
  let orientation = 1;
  while (i + 4 < b.length) {
    if (b[i] !== 0xff) {
      i++;
      continue;
    }
    const marker = b[i + 1] ?? 0;
    if (marker === 0xff) {
      i++;
      continue;
    }
    if (marker === 0xd8 || (marker >= 0xd0 && marker <= 0xd7) || marker === 0x01) {
      i += 2;
      continue;
    }
    const len = u16(b, i + 2);
    if (marker === 0xe1 && ascii(b, i + 4, 'Exif\0\0')) {
      orientation = exifOrientation(b, i + 10, Math.min(b.length, i + 2 + len));
    }
    if (marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
      const height = u16(b, i + 5);
      const width = u16(b, i + 7);
      return orientation >= 5 ? { width: height, height: width } : { width, height };
    }
    i += 2 + len;
  }
  return null;
}

function pngSize(b: Uint8Array): { width: number; height: number } | null {
  return ascii(b, 12, 'IHDR') ? { width: u32(b, 16), height: u32(b, 20) } : null;
}

function webpSize(b: Uint8Array): { width: number; height: number } | null {
  const le24 = (i: number) => (b[i] ?? 0) | ((b[i + 1] ?? 0) << 8) | ((b[i + 2] ?? 0) << 16);
  if (ascii(b, 12, 'VP8X')) return { width: 1 + le24(24), height: 1 + le24(27) };
  if (ascii(b, 12, 'VP8L')) {
    const bits = (b[21] ?? 0) | ((b[22] ?? 0) << 8) | ((b[23] ?? 0) << 16) | ((b[24] ?? 0) << 24);
    return { width: 1 + (bits & 0x3fff), height: 1 + ((bits >>> 14) & 0x3fff) };
  }
  if (ascii(b, 12, 'VP8 ')) {
    return {
      width: ((b[26] ?? 0) | ((b[27] ?? 0) << 8)) & 0x3fff,
      height: ((b[28] ?? 0) | ((b[29] ?? 0) << 8)) & 0x3fff,
    };
  }
  return null;
}

/** HEIC/AVIF: the largest `ispe` box is the main image; `irot` may turn it. */
function heifSize(b: Uint8Array): { width: number; height: number } | null {
  let best: { width: number; height: number } | null = null;
  let turned = false;
  for (let i = 4; i + 16 <= b.length; i++) {
    if (ascii(b, i, 'ispe')) {
      const width = u32(b, i + 8);
      const height = u32(b, i + 12);
      if (!best || width * height > best.width * best.height) best = { width, height };
    } else if (ascii(b, i, 'irot')) {
      turned = ((b[i + 4] ?? 0) & 1) === 1;
    }
  }
  if (best && turned) best = { width: best.height, height: best.width };
  return best;
}

/** Pixel size (after EXIF rotation) from the file header, or `null`. */
export async function probeSize(file: Blob): Promise<{ width: number; height: number } | null> {
  try {
    const b = await headBytes(file, 256 * 1024);
    let size: { width: number; height: number } | null = null;
    if (b[0] === 0xff && b[1] === 0xd8) size = jpegSize(b);
    else if (b[0] === 0x89 && ascii(b, 1, 'PNG')) size = pngSize(b);
    else if (ascii(b, 0, 'RIFF') && ascii(b, 8, 'WEBP')) size = webpSize(b);
    else if (ascii(b, 4, 'ftyp')) size = heifSize(b);
    return size && size.width > 0 && size.height > 0 ? size : null;
  } catch {
    return null;
  }
}

// ---- Browser codec ---------------------------------------------------------

export const browserCodec: ImageCodec = {
  async decode(file, options) {
    const maxEdge = options?.maxEdge;
    if (typeof createImageBitmap === 'function') {
      const opts: ImageBitmapOptions = { imageOrientation: 'from-image' };
      let scaled = false;
      if (maxEdge) {
        const size = await probeSize(file);
        if (size) {
          const fit = fitWithin(size.width, size.height, maxEdge);
          if (fit) {
            opts.resizeWidth = fit.width;
            opts.resizeHeight = fit.height;
            scaled = true;
          }
        } else if (file.size > UNKNOWN_SIZE_SCALE_BYTES) {
          // Unknown size and big: cap the width, the height follows the ratio.
          opts.resizeWidth = maxEdge;
          scaled = true;
        }
        if (scaled) opts.resizeQuality = 'high';
      }
      const bitmap = await createImageBitmap(file, opts);
      return {
        width: bitmap.width,
        height: bitmap.height,
        source: bitmap,
        scaled,
        close: () => bitmap.close(),
      };
    }
    // Fallback: one <img> at a time. It decodes at full size (no resize hook),
    // but the caller draws it small and drops it right away.
    const url = URL.createObjectURL(file);
    try {
      const img = new Image();
      img.src = url;
      await img.decode();
      return { width: img.naturalWidth, height: img.naturalHeight, source: img };
    } finally {
      URL.revokeObjectURL(url);
    }
  },
  async encode(image, width, height, type, quality) {
    const canvas = document.createElement('canvas');
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('canvas unavailable');
    ctx.imageSmoothingQuality = 'high';
    ctx.drawImage(image.source, 0, 0, width, height);
    return new Promise<Blob>((resolve, reject) => {
      canvas.toBlob(
        (blob) => {
          // Release the backing store now: WKWebView caps total canvas memory.
          canvas.width = 0;
          canvas.height = 0;
          if (blob) resolve(blob);
          else reject(new Error('encoding failed'));
        },
        type,
        quality,
      );
    });
  },
  hasAlpha(image) {
    const canvas = document.createElement('canvas');
    canvas.width = 64;
    canvas.height = 64;
    const ctx = canvas.getContext('2d', { willReadFrequently: true });
    if (!ctx) return true;
    ctx.drawImage(image.source, 0, 0, 64, 64);
    const data = ctx.getImageData(0, 0, 64, 64).data;
    canvas.width = 0;
    canvas.height = 0;
    for (let i = 3; i < data.length; i += 4) if ((data[i] ?? 255) < 255) return true;
    return false;
  },
};

// ---- Preparation -------------------------------------------------------------

export interface PreparedImage {
  /** What to upload: the original, or a smaller JPEG/PNG copy. */
  file: File;
  /** Small preview, or `null` when the image could not be decoded. */
  thumb: Blob | null;
  /** The webview could not decode it (HEIC on Android WebView): sent as is. */
  undecodable: boolean;
  /** `resizeWidth`/`resizeHeight` were used, so no full-size bitmap existed. */
  scaledDecode: boolean;
}

/**
 * Decodes the image once, alone, and returns the file to upload plus a small
 * preview. The decoded bitmap is closed before this returns.
 */
export async function prepareImageWithThumb(
  file: File,
  codec: ImageCodec = browserCodec,
): Promise<PreparedImage> {
  if (!isResizable(file.type))
    return { file, thumb: null, undecodable: false, scaledDecode: false };
  let image: DecodedImage;
  try {
    image = await codec.decode(file, { maxEdge: MAX_IMAGE_EDGE });
  } catch {
    return { file, thumb: null, undecodable: true, scaledDecode: false };
  }
  let out = file;
  let thumb: Blob | null;
  try {
    const target = fitWithin(image.width, image.height);
    const w = target?.width ?? image.width;
    const h = target?.height ?? image.height;
    const isPng = file.type === 'image/png';
    const big = file.size > (isPng ? PNG_TO_JPEG_BYTES : KEEP_ORIGINAL_BYTES);
    const needsConversion =
      isHeif(file.type) || file.type === 'image/bmp' || file.type === 'image/tiff';
    if (image.scaled || target || big || needsConversion) {
      let type = 'image/jpeg';
      if (isPng) {
        // A screenshot becomes JPEG; transparency (or not knowing) keeps PNG.
        type = codec.hasAlpha && !codec.hasAlpha(image) ? 'image/jpeg' : 'image/png';
      }
      try {
        const blob = await codec.encode(image, w, h, type, JPEG_QUALITY);
        // Keep the original if re-encoding somehow made it bigger (never for HEIC).
        if (blob.size < file.size || isHeif(file.type)) {
          out = new File([blob], renamed(file.name, type), {
            type,
            lastModified: file.lastModified,
          });
        }
      } catch {
        // Send the original.
      }
    }
    try {
      const small = fitWithin(w, h, THUMB_EDGE) ?? {
        width: Math.max(1, w),
        height: Math.max(1, h),
      };
      thumb = await codec.encode(image, small.width, small.height, 'image/jpeg', THUMB_QUALITY);
    } catch {
      thumb = null;
    }
  } finally {
    image.close?.();
  }
  return { file: out, thumb, undecodable: false, scaledDecode: Boolean(image.scaled) };
}

/**
 * Returns the file to upload: the original when it is not a resizable
 * image, small enough, or cannot be processed; otherwise a downscaled copy.
 */
export async function prepareImage(file: File, codec: ImageCodec = browserCodec): Promise<File> {
  return (await prepareImageWithThumb(file, codec)).file;
}
