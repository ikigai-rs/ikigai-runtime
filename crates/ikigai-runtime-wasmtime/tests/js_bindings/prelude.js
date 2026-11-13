// postcard's primitives. A writer is an array of byte values; a reader is { b, i }.
function utf8(s) {
  const out = [];
  for (let i = 0; i < s.length; i++) {
    let c = s.codePointAt(i);
    if (c > 0xffff) i++;
    if (c < 0x80) out.push(c);
    else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
    else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
    else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
  }
  return out;
}
function fromUtf8(b) {
  let s = "";
  for (let i = 0; i < b.length; ) {
    const c = b[i++];
    let p;
    if (c < 0x80) p = c;
    else if (c < 0xe0) p = ((c & 31) << 6) | (b[i++] & 63);
    else if (c < 0xf0) p = ((c & 15) << 12) | ((b[i++] & 63) << 6) | (b[i++] & 63);
    else p = ((c & 7) << 18) | ((b[i++] & 63) << 12) | ((b[i++] & 63) << 6) | (b[i++] & 63);
    s += String.fromCodePoint(p);
  }
  return s;
}
function byteOrder(a, b) {
  const x = utf8(a), y = utf8(b);
  for (let i = 0; i < x.length && i < y.length; i++) if (x[i] !== y[i]) return x[i] - y[i];
  return x.length - y.length;
}
const E = {
  // A varint of up to 2^53 - 1: Number arithmetic, never 32-bit bitwise.
  u64(w, n) {
    if (!Number.isSafeInteger(n) || n < 0) throw new TypeError("not a u64: " + n);
    while (n >= 128) { w.push((n % 128) + 128); n = Math.floor(n / 128); }
    w.push(n);
  },
  bytes(w, b) { E.u64(w, b.length); for (let i = 0; i < b.length; i++) w.push(b[i]); },
  str(w, s) { E.bytes(w, utf8(s)); },
  opt: (f) => (w, v) => { if (v === null || v === undefined) w.push(0); else { w.push(1); f(w, v); } },
  seq: (f) => (w, v) => { E.u64(w, v.length); for (const x of v) f(w, x); },
  set: (f) => (w, v) => { const s = [...new Set(v)].sort(byteOrder); E.u64(w, s.length); for (const x of s) f(w, x); },
  map: (fk, fv) => (w, v) => { const ks = Object.keys(v).sort(byteOrder); E.u64(w, ks.length); for (const k of ks) { fk(w, k); fv(w, v[k]); } },
  result: (fo, fe) => (w, v) => { if ("ok" in v) { w.push(0); fo(w, v.ok); } else { w.push(1); fe(w, v.err); } },
};
const D = {
  u64(r) {
    let n = 0, scale = 1;
    for (;;) {
      if (r.i >= r.b.length) throw new RangeError("the message ends inside a varint");
      const c = r.b[r.i++];
      n += (c & 127) * scale;
      if (c < 128) return n;
      scale *= 128;
      if (scale > 2 ** 63) throw new RangeError("a varint longer than a u64");
    }
  },
  bytes(r) {
    const n = D.u64(r);
    if (r.i + n > r.b.length) throw new RangeError("the message ends inside a byte string");
    const out = r.b.slice(r.i, r.i + n);
    r.i += n;
    return out;
  },
  str: (r) => fromUtf8(D.bytes(r)),
  opt: (f) => (r) => { const t = D.u64(r); if (t === 0) return null; if (t === 1) return f(r); throw new RangeError("an option tag of " + t); },
  seq: (f) => (r) => { const n = D.u64(r), out = []; for (let i = 0; i < n; i++) out.push(f(r)); return out; },
  set: (f) => (r) => D.seq(f)(r),
  map: (fk, fv) => (r) => {
    const n = D.u64(r), out = {};
    for (let i = 0; i < n; i++) {
      const k = fk(r);
      if (Object.prototype.hasOwnProperty.call(out, k)) throw new RangeError("invalid argument `" + k + "`: given twice; an argument takes one value, so name it once");
      out[k] = fv(r);
    }
    return out;
  },
  result: (fo, fe) => (r) => { const t = D.u64(r); if (t === 0) return { ok: fo(r) }; if (t === 1) return { err: fe(r) }; throw new RangeError("a result tag of " + t); },
};
function encodeWith(f, v) { const w = []; f(w, v); return new Uint8Array(w); }
function decodeWith(f, b) {
  const r = { b: b instanceof Uint8Array ? b : new Uint8Array(b), i: 0 };
  const v = f(r);
  if (r.i !== r.b.length) throw new RangeError((r.b.length - r.i) + " bytes left after the message");
  return v;
}
