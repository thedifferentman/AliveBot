# AliveBot local compatibility patch

Source: crates.io `nagisa-onebot` 0.12.0 (MIT OR Apache-2.0),
https://github.com/djkcyl/nagisa.
Upstream revision: `96c17f5bd1cfe4527a0bbb76d6425a247a88ec79`.
The crate source and manifest were copied without changing the global Cargo cache.
Only this adapter crate is overridden through `[patch.crates-io]` in AliveBot.

Changes:
- Deserialize `message`, `msg`, and `wording` separately. Prefer the first nonempty
  error description, retaining `data` when NapCat sends multiple fields together.
- Return an explicit action error for malformed WebSocket responses instead of
  converting them to an empty successful response.
- Decode both wrapped forward nodes and NapCat message objects, including nested
  sender information. Reject missing arrays or malformed content.

Regression tests: `cargo test --locked -p nagisa-onebot --lib`.
The ignored live test only retrieves forward messages and does not send QQ messages.
Remove this override after an upstream release includes equivalent fixes.
