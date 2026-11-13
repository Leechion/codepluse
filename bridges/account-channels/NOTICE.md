# Account channel protocol adapter

Derived from the user's installed `dsh-our-free-model` 2.3.0, its standalone channel runtime and MIT-licensed `channel-pack` (upstream https://gitee.com/iJetLi/deepseek-harness-codearts, commit 345f0a07b22713c0ae189ca7d8b97ec4f64626c6).

`vendor/business.mjs` is built from the published standalone protocol bundle; original SHA256: `D7440AC8A45B0E0A408B753B8030314F477FD9D0AD28FE39F8BEA7B1D866F20D`. Codepluse selects the WorkBuddy/Qoder classes, adapters and message conversion helpers, updates the WASM path, and removes the unused service implementations through esbuild tree shaking. This also removes unrelated vendors' embedded OAuth identities from the distributed bundle. Only WorkBuddy domestic/international and Qoder international/CN are instantiated.

To reproduce with the frontend's locked development dependencies installed: `node bridges/account-channels/build-vendor.mjs <installed-dsh-our-free-model-package-directory>`.

`contracts.mjs`, `credentials.mjs` and `images.mjs` come from the same package. The credential description now explicitly reports whether the credential exists. The upstream MIT license is preserved at `vendor/LICENSE`; bundled dependency notices remain in `business.mjs`.

The Go server starts this local Node.js adapter on demand. Its random Bearer key is passed through the process environment, and it listens on loopback only. Credentials and account state belong to this Codepluse checkout's `.data/account-channels`; the adapter does not modify DSH, WorkBuddy or Qoder client credentials. Browser authorization is initiated explicitly in settings. No automatic credit claiming or subscription changes are performed.
