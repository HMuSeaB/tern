import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * 从 cc-switch 补档位映射。
 *
 * # 为什么单独一张卡
 *
 * `client_env`（模型档位映射）是后加的功能。此前导入过的人配置里 38 个
 * 供应商的这个字段全是空的——导入是一次性的，补了代码不会追溯已经导完的
 * 数据。手工补 38 家不现实，所以要有这条把映射补上。
 *
 * 和历史用量导入（`CcSwitchUsageCard`）是同一类事：都是"cc-switch 里有、
 * tern 这边还缺"的补数。所以放在相邻位置，也用同一套先预览再确认的套路。
 *
 * # 默认不自动补
 *
 * 一进面板就改 38 个供应商的配置，用户不知道发生了什么。默认只读探测，
 * 点一下才写。而且只补 `client_env` 为空的——用户在面板里改过的映射是
 * 当前的意图，拿 cc-switch 的旧值盖掉它没有道理。
 */

/** 与 Rust 侧 ModelEnvBackfillReport 对齐 */
export interface BackfillReport {
  db_path: string;
  found: boolean;
  patched: number;
  skipped_configured: number;
  skipped_empty: number;
  details: string[];
}

export function ModelEnvBackfillCard({ onDone }: { onDone: () => void }) {
  /** null = 还没探测过。点「检查」才探测，不在 mount 时自动跑 */
  const [preview, setPreview] = useState<BackfillReport | null>(null);
  const [result, setResult] = useState<BackfillReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [hidden, setHidden] = useState(false);

  /** 只读探测（apply = false）：不写盘、不备份、不改配置。
   *
   *  # 为什么不自动跑
   *
   *  「用量」页是 inert 离屏页——四页在启动时全部 mount。在这里自动探测
   *  等于每次开面板都要读一遍 cc-switch 那个 11 MB 的库，而用户此刻正看着
   *  「供应商」页，压根没打算补映射。读库期间同步命令还会冻结整个界面。
   *
   *  而且这和"自动嗅探不好用"是同一个道理：cc-switch 有or没有、有or没有
   *  可补的，都该是用户表达意图之后才回答的问题。 */
  const check = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const value = await invoke<BackfillReport>("cc_switch_backfill_model_env", {
        apply: false,
      });
      setPreview(value);
      if (!value.found) setError(null);
    } catch (e) {
      // 读库失败就说读库失败，别静默显示成"没有可补的"——
      // 那会让用户以为自己的映射已经齐了
      setPreview(null);
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const run = useCallback(async () => {
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      const outcome = await invoke<BackfillReport>("cc_switch_backfill_model_env", {
        apply: true,
      });
      setResult(outcome);
      setPreview(outcome);
      onDone();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [onDone]);

  // 用户说"知道了"就不占地方。preview === null 不是隐藏条件——
  // 那正是"还没检查过"的初始态，要显示检查入口
  if (hidden) return null;

  const alreadyDone = result !== null && result.patched === 0;

  // ---- 还没探测：给一个检查入口，不自动读库 ----
  if (preview === null) {
    return (
      <section className="card">
        <div className="head">
          <span className="title">补档位映射</span>
          <span className="act">来自 cc-switch</span>
        </div>
        <p className="perm-hint">
          cc-switch 里给每家逐个挑的模型档位（Opus 档 / Sonnet 档…），此前导入的
          供应商没有带上。装过 cc-switch 的话可以补过来。
        </p>
        <div className="wire-row">
          <button className="btn primary" onClick={() => void check()} disabled={busy}>
            {busy ? "检查中…" : "检查有没有可补的"}
          </button>
        </div>
        {error && (
          <div className="notice err" style={{ marginTop: 14 }}>
            <span className="notice-ic">!</span>
            <span className="notice-body">{error}</span>
          </div>
        )}
        <p className="perm-foot">
          检查是只读的：不写盘、不改配置。读的是本机 cc-switch 的库，
          没装就什么都找不到。
        </p>
      </section>
    );
  }

  // 探测过了但没东西可补：一句话说完，不给按钮
  if (preview.patched === 0 && preview.skipped_configured === 0) {
    return (
      <section className="card">
        <div className="head">
          <span className="title">补档位映射</span>
          <span className="act">来自 cc-switch</span>
        </div>
        <p className="perm-hint">
          没有可补的：本机 cc-switch 里没有配映射的供应商，或者 tern
          这边都已经有了。
        </p>
        {error && (
          <div className="notice err" style={{ marginTop: 14 }}>
            <span className="notice-ic">!</span>
            <span className="notice-body">{error}</span>
          </div>
        )}
      </section>
    );
  }

  return (
    <section className="card">
      <div className="head">
        <span className="title">补档位映射</span>
        <span className="act">来自 cc-switch</span>
      </div>

      {preview.patched > 0 ? (
        <p className="perm-hint">
          本机 cc-switch 里 <b className="tnum">{preview.patched}</b> 个供应商配了模型档位
          （Opus 档 / Sonnet 档…），而 tern 这边还没有。补上之后，切到哪家，
          它的档位组合就跟着过去。
        </p>
      ) : (
        <p className="perm-hint">
          没有可补的：要么 cc-switch 里没配映射，要么 tern 这边都已经有了。
        </p>
      )}

      {preview.skipped_configured > 0 && (
        <p className="perm-foot">
          跳过 {preview.skipped_configured} 个：它们在 tern 这边已经有映射了。
          你在面板里改过的是当前意图，不拿 cc-switch 的旧值盖掉。
        </p>
      )}

      {preview.details.length > 0 && (
        <details className="import-skipped">
          <summary>补了哪些（{preview.details.length}）</summary>
          <ul>
            {preview.details.map((line, i) => (
              <li key={i}>{line}</li>
            ))}
          </ul>
        </details>
      )}

      {!alreadyDone && (
        <div className="wire-row">
          <button className="btn primary" onClick={() => void run()} disabled={busy}>
            {busy ? "补写中…" : `补 ${preview.patched} 个`}
          </button>
          <button className="btn ghost" onClick={() => setHidden(true)} disabled={busy}>
            暂不
          </button>
        </div>
      )}

      {result && result.patched > 0 && (
        <div className="notice ok" style={{ marginTop: 14 }}>
          <span className="notice-ic">✓</span>
          <span className="notice-body">
            <b>补了 {result.patched} 个供应商的档位映射。</b>
            现在切到其中任意一家，编辑框里「模型档位映射」一栏就能看到
            Opus 档 / Sonnet 档分别是什么，也能改。
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
        只补 <code>client_env</code> 为空的供应商，地址、key、倍率一律不动。
        写前会备份成 <code>tern.json.bak</code>。
      </p>
    </section>
  );
}
