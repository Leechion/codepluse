Codeg frontend and adapted backend contracts originate from spacering-net/codeg (local snapshot 0.42.0), licensed under Apache-2.0. The original license is preserved in codeg-ui/LICENSE. Go backend implementations are maintained in server-go.

Conversation UI components are adapted from zai-org/ZCode, revision a9fe401eede6e6e5e01be9048679c206fe6f02d9, under Apache-2.0. The copied reasoning component is derived from Vercel ai-elements, copyright 2023 Vercel, Inc., also under Apache-2.0. The license is preserved in codeg-ui/LICENSE; the source repository's notices are available in zcode-reference/THIRD-PARTY-NOTICES.md.

The Windows desktop window uses github.com/jchv/go-webview2 (MIT), github.com/jchv/go-winloader (ISC), and golang.org/x/sys (BSD-3-Clause). Event transport uses github.com/gorilla/websocket (BSD-2-Clause). Versions and checksums are pinned in server-go/go.mod and go.sum. Original dependency license files are included in dist/codepluse/licenses by the desktop build script.

Windows interactive terminals use github.com/UserExistsError/conpty v0.1.4 (MIT), providing access to the operating system's real ConPTY implementation. Its license is included by the same desktop build script.

The WorkBuddy/Qoder account agents use protocol code from dsh-our-free-model 2.3.0 and its MIT-licensed channel-pack, copyright 2026 Jet (https://gitee.com/iJetLi/deepseek-harness-codearts). The upstream license, provenance and local modifications are preserved in bridges/account-channels/vendor/LICENSE and bridges/account-channels/NOTICE.md. The adapter includes the upstream Qoder authentication WASM and bundled jose/undici dependency notices. The standalone support files retain the package's MIT license in vendor/LICENSE-OFM.
