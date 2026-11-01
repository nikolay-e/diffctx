# diffctx-snapshot

An internal layer of [diffctx](https://crates.io/crates/diffctx): one
repository state per question, with snapshot readers that admit files inside the
reader, resolved ranges and the analysis context.

It is published because the `diffctx` crate depends on it, and for no other
reason. **There are no API stability guarantees**: any release may change or
remove anything here. Use the [`diffctx`](https://crates.io/crates/diffctx)
crate.

Apache-2.0.
