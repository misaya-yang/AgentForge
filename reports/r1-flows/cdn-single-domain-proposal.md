# DashScope CDN 单域名 DNS 处置方案（只读调查，待用户确认）

## 已核实来源

运行的代理是 FlClash / FlClashCore（PID736/77689），另有ClashMac旧helper。FlClash当前选中profile ID361504683073736704，SQLite overwrite_type=standard，script_id为空；`overrideDns=false`。配置来源位于：

- `/Users/yang/Library/Application Support/com.follow.clash/profiles/361504683073736704.yaml`：当前选中的profile，已有dns和3条fake-ip-filter。
- `/Users/yang/Library/Preferences/com.follow.clash.plist` 的 `flutter.config` JSON：currentProfileId及patchClashConfig.dns；因overrideDns=false，不能把该patch当作直接覆盖源。
- `/Users/yang/Library/Application Support/com.follow.clash/config.yaml`：已生成文件包含enhanced-mode=fake-ip、198.18.0.1/16和2条filter；与当前profile和prefs并不逐项相同，不能直接编辑它冒充持久源。REST controller未可用，未读取正在运行Core的完整内存配置。
- ClashMac work/config.yaml更新时间为7月，不能当作当前FlClash选中profile。

实际Docker CDN DNS已解析到198.18/15；原safe_fetch拒绝正确。以上只读取选中ID、DNS字段和配置元数据，没有复制订阅URL、代理密码、controller secret或完整profile。

## 最小候选修正

用户确认后，在**当前选中profile的dns.fake-ip-filter**保留3条原条目，仅追加精确域名：

```yaml
- "dashscope-463f.oss-accelerate.aliyuncs.com"
```

当前fake-ip-filter-mode未显式设置，Mihomo默认blacklist；该匹配域名因此返回真实IP。规则依据[官方DNS说明](https://wiki.metacubex.one/config/dns/)；不要用`*.aliyuncs.com`、不要切换全局enhanced-mode/overrideDns、不要改变nameserver或路由/TLS/SSRF。

应用方式：通过FlClash当前配置的本地DNS覆写/编辑保留原profile内容，然后以现有“重新加载配置”流程生效。不要直接改生成config.yaml，也不要修改其他profile。当前profile来自订阅，若直接编辑本地文件，下次订阅更新可能覆盖此项；需要用户确认这是本轮临时验收修正还是要求订阅端/现有局部覆写持久保留。未经确认不新增覆写脚本、不改overwrite_type。

## 验证与回退

1. 修改前只记录该dns列表，避免复制整份含凭据profile。重载可能短暂影响使用此代理的连接，需要用户确认维护时机。
2. 先只读检查生成DNS规则和Docker `getaddrinfo`：该域名地址必须全部为公网，不能仍是198.18/15、localhost/link-local/private；普通chat/现有网络行为应可用。没有通过则不再消耗图片额度。
3. DNS前提通过后才启动**一个新的独立图片成功验收**，从平台生成→落盘→刷新回看。旧unknown任务和已发工具不重放。
4. 回退只删除新增的精确域名条目并按原方式重载；保持3条原filter及其余配置。不要清理整个DNS/Fake-IP缓存或重置系统代理。

## 待确认

本轮只交付可审阅方案，未修改共享网络。需要用户确认单profile精确域名修正及重载时机；若要求永久修复，还需确认如何保留订阅更新后的局部覆写。运行中Core配置未通过REST独立读取，因此修改后的DNS实测是必要验收，不能仅凭文件改动宣称解决。
