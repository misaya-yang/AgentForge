# Reqwest 0.13.5 compatibility — 2026-10-15

Recommendation: upgrade the native workspace to exact Reqwest 0.13.5 with
`default-features=false` and `json,rustls,stream,http2,query,form`. Keep the client's
existing `multipart` feature. Root owns manifest/lock changes and serialized builds.
The standalone SDK engine and upstream runtime overlay pins are outside this change.

## Required changes and compatibility

The official 0.13 changelog renames `rustls-tls` to `rustls` and gates query/form
serialization behind explicit features. Both helpers are used in the native tree,
including OAuth form submission and provider query parameters. MSRV is Rust1.85;
the project pins1.99. Searches found no removed legacy TLS/root-builder methods in
workspace source. Existing asynchronous clients, JSON, raw bytes, streaming,
timeouts and multipart APIs remain represented by the current API.

The upstream rustls default now uses aws-lc and platform certificate verification.
This changes the HTTP trust source from former bundled WebPKI roots to OS/platform
trust; it does not disable certificate or hostname checking. No custom CA,
verification bypass or process-global crypto provider is installed by this patch.

## Actual crypto-provider compatibility fix

Official Reqwest0.13.5 client code explicitly selects aws-lc when no application
provider is installed. Inspected Tonic0.14.6 selects its configured ring provider
explicitly, and SQLx0.8.6 also constructs an explicit provider. Those paths can coexist.
Tokio-Tungstenite0.28's default WSS connector instead calls Rustls's implicit builder,
which cannot infer a provider when both ring and aws-lc features are enabled.

`platform-gateway-foundation/src/websocket_transport.rs` now constructs an explicit
ring `ClientConfig`, keeps safe protocol defaults and verified WebPKI roots, and
passes it to `connect_async_tls_with_config`. This is the concrete ComfyUI/shared
JSON WebSocket path. Plain WS behavior remains handled by Tungstenite. A native
source scan found no other production direct `connect_async` or implicit Rustls
builder callsites; remaining direct connects are plain loopback Studio tests.

## Verification

Added a pure test constructing the explicit TLS configuration with no global
provider setup. Existing `platform-gateway-foundation --test websocket_transport`
contains actual duplex JSON/control-frame/binary-frame tests. No Cargo, HTTP/TLS
network call or infrastructure run was performed in this lane; root will compile
and run targeted tests after updating the manifests. Compilation and real remote
TLS interoperability are not claimed at this checkpoint.

Sources:
- https://raw.githubusercontent.com/seanmonstar/reqwest/v0.13.5/CHANGELOG.md
- https://raw.githubusercontent.com/seanmonstar/reqwest/v0.13.5/Cargo.toml
- https://raw.githubusercontent.com/seanmonstar/reqwest/v0.13.5/src/async_impl/client.rs
- https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html
- Local resolved official crate sources: tonic0.14.6, sqlx-core0.8.6,
  tokio-tungstenite0.28.0.
