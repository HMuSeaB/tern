import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * 把 cc-switch 的历史用量导进来。
 *
 * # 为什么单独一张卡，不塞进欢迎页
 *
 * 欢迎页的导入是"搬供应商配置"，这里搬的是**历史账单**。两件事：
 * - 供应商配置只在第一次运行时需要
 * - 历史用量任何时候都值得补——用户装了 tern 一个月后才想起来"我这月的花费呢"
 *
 * 而且欢迎页的导入已经做完了（不显示欢迎页时这张卡才出现）。
 *
 * # 默认不自动导
 *
 * 一进面板就闷头导 17000 条，用户不知道发生了什么。所以默认只做**只读预览**
 * （"有 16963 条、覆盖 9/20 到今天"），要点一下才真写。覆盖用量库的操作
 * 必须有这一步确认。
 */

/** 与 Rust 侧 ccswitch_usage::ImportOutcome 对齐 */
export interface ImportOutcome {
  db_path: string;
  total: number;
  imported: number;
  skipped_session: number;
  skipped_duplicate: number;
  unpriced: number;
  from_ms: number | null;
  to_ms: number | null;
}

/** 毫秒 → `10-08`。只显示月日，跨年才管年份。 */
function shortDate(ms: number | null): string {
  if (ms === null) return "—";
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

export function CcSwitchUsageCard({ onDone }: { onDone: () => void }) {
  const [preview, setPreview] = useState<ImportOutcome | null>(null);
  const [result, setResult] = useState<ImportOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [hidden, setHidden] = useState(false);

  // 预览是只读的，失败就当"没有可导的"——不弹错误。
  // 多数人机器上没装 cc-switch，那是常态而不是异常
  useEffect(() => {
    let alive = true;
    invoke<ImportOutcome>("cc_switch_usage_preview")
      .then((value) => alive && setPreview(value))
      .catch(() => alive && setPreview(null));
    return () => {
      alive = false;
    };
  }, []);

  const runImport = useCallback(async () => {
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      const outcome = await invoke<ImportOutcome>("cc_switch_usage_import");
      setResult(outcome);
      onDone();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [onDone]);

  // 没有可导的，或者用户已导过并说"知道了"，就不占地方
  const nothingToImport = preview === null || preview.imported === 0;
  if (hidden || nothingToImport) return null;

  return (
    <section className="card">
      <div className="head">
        <span className="title">导入历史用量</span>
        <span className="act">来自 cc-switch</span>
      </div>

      <p className="perm-hint">
        tern 只记自己启动之后的请求。本机 cc-switch 的库里有{" "}
        <b className="tnum">{preview.imported.toLocaleString()}</b> 条历史记录
        {preview.from_ms !== null && (
          <>
            （{shortDate(preview.from_ms)} → {shortDate(preview.to_ms)}）
          </>
        )}
        ，导进来之后趋势和占比才是完整的一个月。
      </p>

      {/* 跳过的那些要说出来。"共 17153 条却只导 16963"不说的话，
          用户会以为漏了数据 */}
      {preview.skipped_session > 0 && (
        <p className="perm-foot">
          跳过 {preview.skipped_session.toLocaleString()} 条：它们是从 Claude Code 会话日志
          反推的，token 口径和代理请求不同，混在一起会让"新增输入"算错。
        </p>
      )}

      <div className="wire-row">
        <button className="btn primary" onClick={() => void runImport()} disabled={busy}>
          {busy ? "导入中…" : "导入"}
        </button>
        <button className="btn ghost" onClick={() => setHidden(true)} disabled={busy}>
          暂不
        </button>
      </div>

      {result && (
        <div className="notice ok" style={{ marginTop: 14 }}>
          <span className="notice-ic">✓</span>
          <span className="notice-body">
            <b>
              导入了 {result.imported.toLocaleString()} 条
              {result.skipped_duplicate > 0 && `，${result.skipped_duplicate.toLocaleString()} 条已在库里`}
              。
            </b>
            {result.unpriced > 0 && (
              <> 其中 {result.unpriced.toLocaleString()} 条没查到价格，成本会偏低——用 `tern price set` 补。</>
            )}
            {result.from_ms !== null && (
              <>覆盖 {shortDate(result.from_ms)} → {shortDate(result.to_ms)}。</>
            )}
          </span>
          <button className="btn ghost" onClick={() => setHidden(true)}>
            知道了
          </button>
        </div>
      )}

      {error && (
        <div className="notice err" style={{ marginTop: 14 }}>
          <span className="notice-ic">!</span>
          <span className="notice-body">{error}</span>
        </div>
      )}

      <p className="perm-foot">
        重复导入是安全的：每条按请求 ID 去重，不会把历史翻倍。
        导入使用 tern 自己的价格表重算成本，所以和你此前在 cc-switch 里看到的
        数字可能有出入——那边九成记录没有存成本值。
      </p>
    </section>
  );
}
