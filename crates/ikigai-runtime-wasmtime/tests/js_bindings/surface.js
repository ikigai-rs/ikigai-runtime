// The `ikigai` surface. The plugin gives this script one primitive, `__ikigai_host_call`: it
// writes a `ModuleReply::HostCall` the codec encoded and returns the host's answer, a
// `ModuleCall` the codec decodes. Every call claims the capability of the session being served,
// never more; the host holds it to that session's grant whatever it claims.
const hostCall = globalThis.__ikigai_host_call;
delete globalThis.__ikigai_host_call;
let session = { kind: { tag: "Scoped", value: [] } };
let thrown = null;

// A named argument: a string or bytes go inline; `{ reference: iri }` and `{ content: id }`
// name a resource or a stored value.
function lowerArg(name, v) {
  if (typeof v === "string") return { tag: "Inline", value: utf8(v) };
  if (v instanceof Uint8Array) return { tag: "Inline", value: v };
  if (v && typeof v.reference === "string") return { tag: "Reference", value: v.reference };
  if (v && typeof v.content === "string") return { tag: "Content", value: v.content };
  throw new TypeError("argument `" + name + "` is not a string, bytes, { reference } or { content }");
}

function liftRepresentation(r) {
  return {
    mediaType: r.repr_type.media_type,
    params: r.repr_type.params,
    bytes: r.bytes,
    expiry: r.expiry.tag === "At" ? r.expiry.value : r.expiry.tag,
    get text() { return fromUtf8(this.bytes); },
  };
}

// A typed refusal: an exception whose `name` is the class and `message` the resource.
function refuse(e) {
  const error = new Error(e.tag === "InvalidArgument" ? e.name + ": " + e.detail : e.value);
  error.name = e.tag;
  if (e.tag === "InvalidArgument") { error.argument = e.name; error.detail = e.detail; }
  thrown = { error, refusal: e };
  throw error;
}

function call(verb, target, named) {
  if (typeof target !== "string") throw new TypeError("a target is an IRI string");
  const args = {};
  for (const name of Object.keys(named || {})) args[name] = lowerArg(name, named[name]);
  const request = { verb: { tag: verb }, target, args };
  const reply = codec.AbiModuleReply.encode({ tag: "HostCall", request, capability: session });
  const answer = codec.AbiModuleCall.decode(hostCall(reply));
  if (answer.tag === "HostResult") {
    if ("ok" in answer.value) return liftRepresentation(answer.value.ok);
    refuse({ tag: "Endpoint", value: answer.value.err });
  }
  if (answer.tag === "HostError") refuse(answer.value);
  refuse({ tag: "Endpoint", value: "host answered a host_call with " + answer.tag });
}

const ikigai = { codec, refusals: refusals.slice() };
for (const verb of verbs) {
  ikigai[verb.toLowerCase()] = (target, args) => call(verb, target, args);
}
Object.freeze(ikigai);
globalThis.ikigai = ikigai;

// For the plugin: begin serving an `Invoke` (its encoded bytes), and ask whether an escaping
// exception is the kernel refusal this surface threw, unchanged.
globalThis.__ikigai_begin = (bytes) => {
  const invoke = codec.AbiModuleCall.decode(bytes);
  session = invoke.capability;
  thrown = null;
  return invoke.request.target;
};
globalThis.__ikigai_escaped = (e) => {
  if (thrown === null || thrown.error !== e || e.name !== thrown.refusal.tag) return null;
  return codec.AbiModuleReply.encode({ tag: "ErrorTyped", value: thrown.refusal });
};
