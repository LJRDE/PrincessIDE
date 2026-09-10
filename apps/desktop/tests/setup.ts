/**
 * Global test setup (runs for every test file, node and jsdom alike).
 *
 * Kept deliberately small: pin the timezone so any date formatting in snapshots
 * is deterministic, and give jsdom the layout APIs CodeMirror 6 probes for but
 * jsdom does not implement (they return empty boxes, which is exactly what a
 * zero-sized offscreen DOM should report).
 */

process.env['TZ'] = 'UTC';

interface RangeLike {
  getClientRects?: () => unknown;
  getBoundingClientRect?: () => unknown;
}

if (typeof window !== 'undefined' && typeof Range !== 'undefined') {
  const emptyRect = () => ({
    x: 0,
    y: 0,
    top: 0,
    left: 0,
    right: 0,
    bottom: 0,
    width: 0,
    height: 0,
    toJSON: () => ({}),
  });
  const emptyRectList = () => Object.assign([], { item: () => null });

  const proto = Range.prototype as unknown as RangeLike;
  if (typeof proto.getClientRects !== 'function') {
    proto.getClientRects = emptyRectList;
  }
  if (typeof proto.getBoundingClientRect !== 'function') {
    proto.getBoundingClientRect = emptyRect;
  }
  // CodeMirror asks the element under the pointer during measurement.
  const elementProto = Element.prototype as unknown as Record<string, unknown>;
  if (typeof elementProto['getClientRects'] !== 'function') {
    elementProto['getClientRects'] = emptyRectList;
  }
}
