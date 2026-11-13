# Writes a workerd config: the handler, with or without its kernel binding (the grant), with the
# default `internet` globalOutbound or a refusing one, and optionally --jitless.
import sys
grant, outbound, jit, port = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
binding = ""
if grant == "granted":
    binding = '''bindings = [ ( name = "ikigai", service = ( name = "kernel", entrypoint = "Kernel",
      props = ( json = "{\\"grant\\": [\\"source urn:host:name\\", \\"sink urn:host:log\\"]}" ) ) ) ],'''
glob = 'globalOutbound = "deny",' if outbound == "deny" else ""
flags = 'v8Flags = ["--jitless"],' if jit == "jitless" else ""
print(f'''using Workerd = import "/workerd/workerd.capnp";
const config :Workerd.Config = (
  services = [
    (name = "main", worker = .handler),
    (name = "kernel", worker = .kernel),
    (name = "deny", network = (allow = [])),
  ],
  sockets = [ ( name = "http", address = "127.0.0.1:{port}", http = (), service = "main" ) ],
  {flags}
);
const handler :Workerd.Worker = (
  modules = [ (name = "handler.js", esModule = embed "handler.js") ],
  compatibilityDate = "2026-11-01",
  {binding}
  {glob}
);
const kernel :Workerd.Worker = (
  modules = [ (name = "kernel.js", esModule = embed "kernel.js") ],
  compatibilityDate = "2026-11-01",
);''')
