# The first plugin

`protocol-1.wasm.gz` is the test plugin (`src/api/probe`) as it was built with the first version
of the protocol (`corvus_api::wire`). It stands for every published plugin: a plugin names the
oldest Corvus GCS it runs in, and every later one has to take it. `tests.rs` loads and drives it.

**It is never built again.** A failing test means a change broke the protocol, a permission or
the sandbox for plugins that are out there: fix the change, not the file. When the protocol
grows, add a second file for the new version beside it.

Until the first release that carries WebAssembly plugins is out, the protocol may still change,
and this file with it:

```bash
cargo build --locked --target wasm32-wasip1 --profile plugin -p corvus_plugin_probe
gzip -9 -n -c target/wasm32-wasip1/plugin/corvus_plugin_probe.wasm > src/plugins/wasm/fixtures/protocol-1.wasm.gz
```
