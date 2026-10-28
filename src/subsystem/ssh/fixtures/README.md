# Public test fixture / 公开测试样本

`rsa_8196` is a publicly shipped, unencrypted RSA-8196 test private key.
It must only be used by isolated test servers, never as a real identity.
`rsa_8196` 是随源码公开分发的无口令 RSA-8196 测试私钥，
仅用于隔离的测试服务，绝不能用作真实身份。

Regenerate with / 重新生成命令：

```sh
ssh-keygen -t rsa -b 8196 -N '' -C 'PUBLIC TEST FIXTURE ONLY / 仅用于公开测试' -f rsa_8196
```
