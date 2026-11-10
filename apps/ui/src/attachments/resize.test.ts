import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  browserCodec,
  fitWithin,
  guessImageType,
  isResizable,
  JPEG_QUALITY,
  MAX_IMAGE_EDGE,
  prepareImage,
  prepareImageWithThumb,
  probeSize,
  type ImageCodec,
} from './resize';

/** A canvas stand-in: reports a fixed decoded size and encodes to `outBytes`. */
function fakeCodec(width: number, height: number, outBytes = 1000, alpha?: boolean) {
  const close = vi.fn();
  const encode = vi.fn(async (_img: unknown, _w: number, _h: number, type: string) => {
    return new Blob([new Uint8Array(outBytes)], { type });
  });
  const codec: ImageCodec = {
    decode: vi.fn(async () => ({ width, height, source: {} as CanvasImageSource, close })),
    encode,
    ...(alpha === undefined ? {} : { hasAlpha: () => alpha }),
  };
  return { codec, encode, close };
}

const photo = (type = 'image/jpeg', bytes = 5_000_000, name = 'IMG_0001.HEIC') =>
  new File([new Uint8Array(bytes)], name, { type, lastModified: 42 });

describe('fitWithin', () => {
  it('keeps small images and scales the long edge to the limit', () => {
    expect(fitWithin(2500, 1500)).toBeNull();
    expect(fitWithin(2560, 100)).toBeNull();
    expect(fitWithin(8192, 6144)).toEqual({ width: 2560, height: 1920 });
    expect(fitWithin(3000, 12000)).toEqual({ width: 640, height: 2560 });
    expect(fitWithin(100_000, 1)).toEqual({ width: MAX_IMAGE_EDGE, height: 1 });
  });
});

describe('guessImageType', () => {
  it('reads the type from the extension', () => {
    expect(guessImageType('IMG_1.HEIC')).toBe('image/heic');
    expect(guessImageType('a.JPG')).toBe('image/jpeg');
    expect(guessImageType('a.dng')).toBe('image/x-adobe-dng');
    expect(guessImageType('notes.txt')).toBe('');
  });
});

describe('prepareImage', () => {
  it('returns a small JPEG unchanged', async () => {
    const { codec, encode, close } = fakeCodec(2000, 1500);
    const file = photo('image/jpeg', 800_000, 'a.jpg');
    expect(await prepareImage(file, codec)).toBe(file);
    // Only the 256 px preview is encoded; the file itself is untouched.
    expect(encode.mock.calls.every(([, w, h]) => Math.max(w, h) <= 256)).toBe(true);
    expect(close).toHaveBeenCalled();
  });

  it('asks the decoder for a limited size and re-encodes big photos at 0.82', async () => {
    const { codec, encode } = fakeCodec(2560, 1920, 900_000);
    const out = await prepareImage(photo('image/jpeg', 12_000_000, 'IMG_0001.jpg'), codec);
    expect(codec.decode).toHaveBeenCalledWith(expect.anything(), { maxEdge: MAX_IMAGE_EDGE });
    expect(encode).toHaveBeenCalledWith(expect.anything(), 2560, 1920, 'image/jpeg', JPEG_QUALITY);
    expect(JPEG_QUALITY).toBe(0.82);
    expect(out.size).toBe(900_000);
    expect(out.lastModified).toBe(42);
  });

  it('converts HEIC to JPEG even when the result is not smaller', async () => {
    const { codec } = fakeCodec(2000, 1500, 9_000_000);
    const out = await prepareImage(photo('image/heic', 3_000_000, 'IMG_0001.HEIC'), codec);
    expect(out.type).toBe('image/jpeg');
    expect(out.name).toBe('IMG_0001.jpg');
  });

  it('turns a big opaque PNG screenshot into JPEG, keeps transparent ones as PNG', async () => {
    const opaque = fakeCodec(1200, 2600, 100_000, false);
    const out = await prepareImage(photo('image/png', 3_000_000, 'shot.png'), opaque.codec);
    expect(opaque.encode.mock.calls[0]?.[3]).toBe('image/jpeg');
    expect(out.name).toBe('shot.jpg');

    const clear = fakeCodec(1200, 2600, 100_000, true);
    const kept = await prepareImage(photo('image/png', 3_000_000, 'logo.png'), clear.codec);
    expect(clear.encode.mock.calls[0]?.[3]).toBe('image/png');
    expect(kept.name).toBe('logo.png');

    const small = fakeCodec(500, 500, 10, false);
    const pngFile = photo('image/png', 200_000, 'icon.png');
    expect(await prepareImage(pngFile, small.codec)).toBe(pngFile);
  });

  it('sends the original when decoding fails and flags it', async () => {
    const failing: ImageCodec = {
      decode: async () => {
        throw new Error('no HEIC support');
      },
      encode: async () => new Blob(),
    };
    const heic = photo('image/heic');
    const out = await prepareImageWithThumb(heic, failing);
    expect(out.file).toBe(heic);
    expect(out.undecodable).toBe(true);
    expect(out.thumb).toBeNull();
  });

  it('never touches GIF, SVG, RAW or documents', async () => {
    const { codec, encode } = fakeCodec(9000, 9000);
    for (const type of ['image/gif', 'image/svg+xml', 'image/x-adobe-dng', 'application/pdf']) {
      const f = photo(type, 10, 'x');
      expect(await prepareImage(f, codec)).toBe(f);
      expect(isResizable(type)).toBe(false);
    }
    expect(encode).not.toHaveBeenCalled();
  });

  it('makes a 256 px preview from the same single decode', async () => {
    const { codec, encode } = fakeCodec(2560, 1920, 500);
    const out = await prepareImageWithThumb(photo('image/jpeg', 6_000_000, 'a.jpg'), codec);
    expect(codec.decode).toHaveBeenCalledTimes(1);
    expect(encode).toHaveBeenLastCalledWith(expect.anything(), 256, 192, 'image/jpeg', 0.7);
    expect(out.thumb).toBeInstanceOf(Blob);
  });
});

/** A JPEG header with the given pixel size and EXIF orientation (no pixel data). */
function jpegHeader(width: number, height: number, orientation = 1): Uint8Array<ArrayBuffer> {
  const exif = [
    0xff,
    0xe1,
    0x00,
    0x22,
    0x45,
    0x78,
    0x69,
    0x66,
    0,
    0, // APP1 "Exif\0\0"
    0x4d,
    0x4d,
    0x00,
    0x2a,
    0,
    0,
    0,
    8, // TIFF big-endian, IFD at 8
    0x00,
    0x01, // one entry
    0x01,
    0x12,
    0x00,
    0x03,
    0,
    0,
    0,
    1,
    0,
    orientation,
    0,
    0, // Orientation
    0,
    0,
    0,
    0,
  ];
  const sof = [0xff, 0xc0, 0x00, 0x11, 0x08, height >> 8, height & 255, width >> 8, width & 255, 3];
  return new Uint8Array([0xff, 0xd8, ...exif, ...sof, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
}

describe('probeSize and the browser decoder', () => {
  const original = globalThis.createImageBitmap;
  afterEach(() => {
    globalThis.createImageBitmap = original;
  });

  it('reads the pixel size from a JPEG header, turned by EXIF orientation', async () => {
    expect(await probeSize(new Blob([jpegHeader(16320, 12240)]))).toEqual({
      width: 16320,
      height: 12240,
    });
    expect(await probeSize(new Blob([jpegHeader(4000, 3000, 6)]))).toEqual({
      width: 3000,
      height: 4000,
    });
  });

  it('decodes a 200 MP photo through resize options, never at full size', async () => {
    const bitmap = { width: 2560, height: 1920, close: vi.fn() };
    const spy = vi.fn(async () => bitmap as unknown as ImageBitmap);
    globalThis.createImageBitmap = spy as unknown as typeof createImageBitmap;
    const file = new File([jpegHeader(16320, 12240)], 'huge.jpg', { type: 'image/jpeg' });
    const decoded = await browserCodec.decode(file, { maxEdge: 2560 });
    expect(spy).toHaveBeenCalledWith(file, {
      imageOrientation: 'from-image',
      resizeWidth: 2560,
      resizeHeight: 1920,
      resizeQuality: 'high',
    });
    expect(decoded.scaled).toBe(true);
    expect(decoded.width).toBe(2560);
  });

  it('passes the orientation flag and skips resizing for a small photo', async () => {
    const spy = vi.fn(async () => ({ width: 2000, height: 1500, close() {} }) as ImageBitmap);
    globalThis.createImageBitmap = spy as unknown as typeof createImageBitmap;
    const file = new File([jpegHeader(2000, 1500)], 'a.jpg', { type: 'image/jpeg' });
    const decoded = await browserCodec.decode(file, { maxEdge: 2560 });
    expect(spy).toHaveBeenCalledWith(file, { imageOrientation: 'from-image' });
    expect(decoded.scaled).toBe(false);
  });

  it('a HEIC with an unreadable header and a big file is still decoded downscaled', async () => {
    const spy = vi.fn(async () => ({ width: 2560, height: 1920, close() {} }) as ImageBitmap);
    globalThis.createImageBitmap = spy as unknown as typeof createImageBitmap;
    const file = new File([new Uint8Array(8)], 'a.heic', { type: 'image/heic' });
    Object.defineProperty(file, 'size', { value: 9_000_000 });
    await browserCodec.decode(file, { maxEdge: 2560 });
    expect(spy).toHaveBeenCalledWith(
      file,
      expect.objectContaining({ resizeWidth: 2560, imageOrientation: 'from-image' }),
    );
  });
});
