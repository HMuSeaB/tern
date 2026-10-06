import { useState } from "react";
import type { CcSwitchPreview } from "./useTern";

/**
 * 首次运行向导。
 *
 * 主路径是「从 cc-switch 导入」：用户手上已经有配好的供应商，不该为了试 tern 重录一遍。
 * 但导入会把 API key 复制到 tern 的配置里，所以刻意做成两步：先探测、把"将复制 N 个
 * 凭据"摊开给用户看，用户点确认才落盘。没有默认勾选、没有一键跳过确认。
 */
export function Welcome({
  preview,
  onImport,
  onStartFresh,
  onOpenConfigDir,
  busy,
}: {
  preview: CcSwitchPreview | null;
  onImport: () => void;
  onStartFresh: () => void;
  onOpenConfigDir: () => void;
  busy: boolean;
}) {
  const [confirmed, setConfirmed] = useState(false);

  return (
    <div className="welcome">
      <div className="welcome-head">
        <span className="welcome-mark">t</span>
        <div>
          <h1>欢迎使用 tern</h1>
          <p className="welcome-sub">本地网关 + 用量面板。先选一个起点：</p>
        </div>
      </div>

      {preview?.found ? (
        <section className="card import">
          <div className="import-head">
            <h2>从 cc-switch 导入</h2>
            <span className="import-count">
              {preview.providers.length} 个供应商
            </span>
          </div>
          <p className="import-note">
            在 <code>{preview.db_path}</code> 里找到 {preview.providers.length} 个
            Claude 供应商。导入后它们的地址和 key 会复制到 tern 自己的配置文件。
          </p>

          <ul className="import-list">
            {preview.providers.map((p) => (
              <li key={p.id}>
                <span className="import-name">{p.name || p.id}</span>
                <span className="import-url">{p.base_url}</span>
                <span className="import-fmt">{p.api_format}</span>
              </li>
            ))}
          </ul>

          {preview.third_party_count > 0 && (
            <div className="notice warn" style={{ marginTop: 14 }}>
              <span className="notice-ic">!</span>
              <span className="notice-body">
                其中 {preview.third_party_count} 个是第三方网关，
                Claude Code 的 WebSearch / WebFetch 在它们下面会失效。
                这是客户端行为，tern 无法代为转发。
              </span>
            </div>
          )}

          {preview.skipped.length > 0 && (
            <details className="import-skipped">
              <summary>{preview.skipped.length} 个导不进来（缺地址或缺 key）</summary>
              <ul>
                {preview.skipped.map((line, i) => (
                  <li key={i}>{line}</li>
                ))}
              </ul>
            </details>
          )}

          {/* 凭据确认：这一句是导出功能的合规前提，不能被折叠或默认勾掉 */}
          <label className="consent">
            <input
              type="checkbox"
              checked={confirmed}
              onChange={(e) => setConfirmed(e.target.checked)}
            />
            <span>
              我已知晓：这会把 <b>{preview.providers.length} 个供应商的 API key</b>{" "}
              从 cc-switch 复制到 <code>tern.json</code>，
              之后每次启动网关都会用它们发请求。现有 tern.json 会先备份成
              <code>tern.json.bak</code>。
            </span>
          </label>

          <div className="welcome-actions">
            <button
              className="btn primary"
              disabled={!confirmed || busy}
              onClick={onImport}
            >
              {busy ? "导入中…" : `导入 ${preview.providers.length} 个供应商`}
            </button>
            <button className="btn ghost" onClick={onStartFresh} disabled={busy}>
              不用，从空配置开始
            </button>
          </div>
        </section>
      ) : (
        <section className="card import">
          <div className="import-head">
            <h2>没有找到 cc-switch</h2>
          </div>
          <p className="import-note">
            本机没有 <code>~/.cc-switch/cc-switch.db</code>。
            可以先从一份空配置开始，然后手动填供应商。
          </p>
          <div className="welcome-actions">
            <button className="btn primary" onClick={onStartFresh} disabled={busy}>
              创建空配置
            </button>
            <button className="btn ghost" onClick={onOpenConfigDir} disabled={busy}>
              打开配置目录
            </button>
          </div>
        </section>
      )}
    </div>
  );
}
