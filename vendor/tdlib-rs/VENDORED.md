tdlib-rs 1.4.0 from crates.io, vendored for TDLib 1.8.67 (member tags):

- `tl/api.tl` is TDLib's `td/generate/scheme/td_api.tl` at commit
  42e6a5259551178d1dab54a22ad96d14bd906e20, with one change: in
  `richTextButton` the field is `button:InlineButton` instead of the bare
  `inlineButton`, so the generator sees the RichText ⇄ InlineButton cycle
  and boxes it. The JSON is the same.
- `TDLIB_VERSION` in `build.rs` and `src/build.rs` is 1.8.67.
- The generator reruns only when `build.rs` changes: touch it after
  editing the schema.

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
