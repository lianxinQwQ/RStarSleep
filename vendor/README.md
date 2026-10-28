# Local dependency patches / 本地依赖补丁

These are vendored crates.io releases; `.cargo_vcs_info.json` records each upstream
revision. Their original source, manifests, tests, and license notices are retained.
此处保存 crates.io 发布源码；`.cargo_vcs_info.json` 记录各自上游修订。
保留原始源码、清单、测试和许可声明。

- `rsa` 0.10.0-rc.18: raise `RsaPublicKey::MAX_SIZE` from 8192 to 65536 bits.
  Keep modulus and exponent validation intact.
  将 `RsaPublicKey::MAX_SIZE` 从 8192 位提高到 65536 位，保留模数和指数合法性检查。
  The upstream oversized-key regression now explicitly selects its 8192-bit limit.
  上游超限回归测试现在显式指定其原有的 8192 位上限。
- `russh` 0.61.2: add the optional `Handler::auth_publickey_rejection_message`
  callback. Send its message as `SSH_MSG_USERAUTH_BANNER` before rejecting a public-key
  probe or signed request. Handlers returning `None` retain upstream behavior.
  新增可选的 `Handler::auth_publickey_rejection_message` 回调；拒绝候选公钥或签名
  请求前，以 `SSH_MSG_USERAUTH_BANNER` 发送提示。返回 `None` 时保留上游行为。

When upgrading, reapply these changes to the new releases and run the SSH tests,
including the RSA boundary and client-visible rejection tests.
升级时，应在新版源码上重新应用这些修改，并运行 SSH 测试，包含 RSA 边界和客户端
可见的拒绝原因测试。
