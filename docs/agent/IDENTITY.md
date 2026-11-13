# 稳定身份派生契约

这是正式项目首版派生规则。应用升级、重新授权和更换存储加密主密钥不能改变结果；不将应用版本加入输入。PoC 的随机身份不自动迁移到此派生空间。

## 输入与编码

- 租户种子为 32 字节安全随机数，独立长期保存。当前核心只在内存中接收种子，持久化加密在授权集成阶段实现。
- 租户标识使用 UUID 的 16 字节网络顺序；账号依次包含稳定 workspace/account ID、user ID。
- 固定身份名区分大小写，使用原始 UTF-8，不做隐式 Unicode 归一化。不同编码视为不同身份；显示名不参与派生。
- 文本字段要求非空、至多 4096 UTF-8 字节、没有首尾空白或控制字符；不自动修剪输入。
- `field(x) = uint32_be(byte_length(x)) || x`。
- `context = field(tenant_uuid_bytes) || field(workspace_id_utf8) || field(user_id_utf8) || field(identity_name_utf8)`。

## HKDF 与输出

使用 HKDF-SHA256，IKM 为租户种子，固定 salt 为 UTF-8 `codex-rc-bridge/identity`。

- IN ID：info 为 `field("in/id") || context`，输出 16 字节。
- OUT ID：info 为 `field("out/id") || context`，输出 16 字节。
- 两种 ID 均将 UUID version 位设为 8、variant 位设为 RFC 9562 variant，按小写带连字符格式输出。此 ID 不替代官方分配的注册 ID。
- OUT 密钥：info 为 `field("out/signing") || context || uint32_be(counter)`，counter 从 0 开始，输出 32 字节大端标量候选。
- 仅接受 `0 < d < n`，其中 n 为 P-256 群阶；不满足则递增 counter 重新派生，不做取模。counter 是确定性拒绝采样的一部分，不是运行实例标记。
- 标量按 RFC 5915 ECPrivateKey、prime256v1 曲线导入 AWS-LC，由库计算公钥；签名使用 ECDSA P-256/SHA-256，输出 ASN.1 DER。密钥确定性不要求每次签名的随机数或签名字节相同。

`TenantSeed` 和 `OutSigningKey` 不实现 Debug/Serialize，派生出的标量和 DER 暂存使用 zeroize。调用方只能取得公钥与签名；此接口不能替代未来的租户鉴权和秘密存储。

## 兼容性验证

2026-11-08 对现有 A 账号做了一次真实重新授权比较：暂停对应链路并清除本地账号 OAuth 凭据，保留账号元数据、设备身份及配对，再完成新的授权。新 access/refresh token 和加密凭据记录均发生变化；返回令牌中的工作区及成员标识、保存的账号标识和现有 IN 设备 ID 均与基线一致。用户未确认使用的是“添加账号”还是“重新登录”；后者会校验预期身份，但不会改写上游返回值。本次证明这一账号/工作区的重新授权保持身份，不代表所有工作区变更场景的永久稳定保证。比较基线和结果仅在忽略目录中保存摘要，不记录真实令牌或账号标识。

固定向量位于 `tests/fixtures/identity.json`，使用公开的合成种子。Rust 测试对比设备 ID/公钥；`bun tests/identity-vectors.ts` 使用独立的 WebCrypto HKDF 与 OpenSSL 密钥导入验证同一向量。正式身份投入使用后不能通过更新向量接受不兼容派生变化。
