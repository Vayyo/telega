tdlib-rs 1.4.0 from crates.io, vendored for TDLib 1.8.67 (member tags):

- `tl/api.tl` is TDLib's `td/generate/scheme/td_api.tl` at commit
  42e6a5259551178d1dab54a22ad96d14bd906e20, with one change: in
  `richTextButton` the field is `button:InlineButton` instead of the bare
  `inlineButton`, so the generator sees the RichText ⇄ InlineButton cycle
  and boxes it. The JSON is the same.
- `TDLIB_VERSION` in `build.rs` and `src/build.rs` is 1.8.67.
- The generator reruns only when `build.rs` changes: touch it after
  editing the schema.
- Telega's security patches make `src/observer.rs` remove an exact pending
  subscription when its request future is dropped, including cancellation.
- `src/response.rs` validates native correlation IDs without panicking,
  routes correlated replies before update-type validation, and emits only
  structural diagnostics rather than raw response payloads.
- `src/lib.rs` and `src/tdjson.rs` wipe Rust-owned request JSON string fields,
  serialization buffers and native-wire CString buffers after sending.
  Copies retained by TDLib itself are outside this guarantee.

Upstream tdlib-rs v1.4.0 is dual-licensed MIT OR Apache-2.0; this
distribution uses its MIT option and includes the upstream notice verbatim
in `LICENSE-MIT` (source:
https://github.com/FedericoBruzzone/tdlib-rs/blob/v1.4.0/LICENSE-MIT).
The modified TDLib schema in `tl/api.tl` is under the Boost Software
License 1.0; its pinned upstream license is reproduced in
`LICENSE-TDLIB-BOOST` (source:
https://github.com/tdlib/td/blob/42e6a5259551178d1dab54a22ad96d14bd906e20/LICENSE_1_0.txt).
These notices concern upstream vendored materials, not the Telega
application's own license.
