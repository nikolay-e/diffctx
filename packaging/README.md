# Channel manifests

`npm/` is published to <https://www.npmjs.com/package/diffctx> by
`publish-extras.yml`. `scripts/render_packaging.py` regenerates
`npm/checksums.json` (and the Scoop manifest) from the release assets, and CD
patches `npm/package.json`'s version at publish time — do not hand-edit those;
`npm/install.js` and `npm/bin/diffctx.js` are ordinary hand-maintained source.

The Scoop manifest lives in [`../bucket/`](../bucket), not here: that directory
is what `scoop bucket add` clones, so it is a published surface rather than a
staging area.

Support tiers and freeze rules live in the release-channel policy in
[`docs/engineering/quality-strategy.md`](../docs/engineering/quality-strategy.md).
