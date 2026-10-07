import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export interface PermissionPreset {
  id: string;
  label: string;
  detail: string;
  rule: string;
  enabled: boolean;
  /** 被 deny 挡着：放行无效，置灰并说明 */
  denied: boolean;
}

/**
 * Claude Code 权限开关。
 *
 * auto 模式下，任何它评估不了的工具调用都会被一律挡掉——不是它在拒绝用户，
 * 是它判不了时按最保守处理。用户口头上说"全放行"，配置文件里并没有这句话。
 *
 * 所以这里做成看得懂的开关，而不是让用户去手改 JSON：点一下就把规则写进去。
 * 写的是 `settings.local.json`——`settings.json` 常被 cc-switch 这类工具托管
 * （它 env 段里的 PROXY_MANAGED 就是证据），写那儿下次切供应商就丢了。
 */
export function Permissions() {
  const [items, setItems] = useState<PermissionPreset[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      setItems(await invoke<PermissionPreset[]>("list_permissions"));
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const toggle = useCallback(
    async (preset: PermissionPreset) => {
      setPending(preset.id);
      try {
        const next = await invoke<PermissionPreset[]>(
          preset.enabled ? "revoke_permission" : "allow_permission",
          { rule: preset.rule },
        );
        setItems(next);
        setError(null);
      } catch (e) {
        setError(String(e));
      } finally {
        setPending(null);
      }
    },
    [],
  );

  if (error) {
    return (
      <div className="card">
        <div className="head"><span className="title">Claude Code 权限</span></div>
        <p className="empty-text">读不到配置：{error}</p>
      </div>
    );
  }

  if (!items) {
    return (
      <div className="card">
        <div className="head"><span className="title">Claude Code 权限</span></div>
        <p className="empty-text">正在读取…</p>
      </div>
    );
  }

  return (
    <div className="card">
      <div className="head">
        <span className="title">Claude Code 权限</span>
        <span className="act">写入 settings.local.json</span>
      </div>
      <p className="perm-hint">
        auto 模式下，它评估不了的操作会被一律挡掉。打开下面的开关，对应的操作就不再询问。
      </p>

      <div className="perm-list">
        {items.map((preset) => (
          <label
            key={preset.id}
            className={`perm-row ${preset.denied ? "is-denied" : ""}`}
          >
            <input
              type="checkbox"
              checked={preset.enabled}
              disabled={preset.denied || pending === preset.id}
              onChange={() => void toggle(preset)}
            />
            <span className="perm-text">
              <span className="perm-label">{preset.label}</span>
              <span className="perm-detail">
                {preset.denied ? "已被 deny 禁用，放行无效" : preset.detail}
              </span>
            </span>
            <code className="perm-rule">{preset.rule}</code>
          </label>
        ))}
      </div>

      <p className="perm-foot">
        改动对新会话生效。deny 里的规则优先级最高，这里不会改动它。
      </p>
    </div>
  );
}
