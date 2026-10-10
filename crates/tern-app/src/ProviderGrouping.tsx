import { useState } from "react";
import { createPortal } from "react-dom";
import { UNGROUPED_LABEL, type Group } from "./types";

/**
 * 供应商分组用的几块公用界面。
 *
 * 单独拆出来是因为 `.card` 里要同时容纳三种视图（平铺 / 按地址 / 按文件夹），
 * 全塞在 Providers.tsx 里那个文件会变成一锅粥。这里只放"分组长什么样"：
 * 分组卡、文件夹的增改对话框、批量移动的操作条。
 */

/** 分组的折叠卡。标题栏整块都是"展开/收起"的热区。 */
export function GroupCard({
  group,
  expanded,
  onToggle,
  actions,
  children,
}: {
  group: Group<unknown>;
  expanded: boolean;
  onToggle: () => void;
  /** 标题栏右侧的自定义操作（重命名 / 解散）。地址分组不传 */
  actions?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <section className={`prov-group ${group.containsActive ? "is-active" : ""}`}>
      <header className="prov-group-head">
        <button
          type="button"
          className="prov-group-toggle"
          onClick={onToggle}
          aria-expanded={expanded}
        >
          <span className={`prov-caret ${expanded ? "open" : ""}`} aria-hidden />
          <span className="prov-group-name" title={group.title}>
            {group.title}
          </span>
          {group.containsActive && <span className="prov-group-live">使用中</span>}
          <span className="prov-group-count">{group.providers.length} 个</span>
        </button>
        {actions && <div className="prov-group-actions">{actions}</div>}
      </header>
      {expanded && (
        <div className="prov-group-body">
          {group.providers.length > 0 ? (
            children
          ) : (
            <p className="prov-group-empty">
              这个文件夹还是空的。切到「平铺」勾选供应商，再移进来。
            </p>
          )}
        </div>
      )}
    </section>
  );
}

/** 新建 / 重命名文件夹的对话框。 */
export function FolderDialog({
  mode,
  oldName = "",
  initial,
  busy,
  onClose,
  onSubmit,
}: {
  /** create = 新建；rename = 改 oldName */
  mode: "create" | "rename";
  /** rename 时的原名 */
  oldName?: string;
  initial: string;
  busy: boolean;
  onClose: () => void;
  onSubmit: (name: string) => void;
}) {
  const [name, setName] = useState(initial);
  const trimmed = name.trim();

  // 挂到 body：理由同 ProviderEditor 的 portal()——`.tab-track` 上的 transform
  // 会把 fixed 的包含块从视口改成那条四页宽的轨道
  return createPortal(
    <div className="prov-modal-back" onClick={onClose}>
      <div
        className="prov-modal"
        role="dialog"
        aria-label={mode === "create" ? "新建文件夹" : "重命名文件夹"}
        onClick={(e) => e.stopPropagation()}
      >
        <h3>{mode === "create" ? "新建文件夹" : `重命名「${oldName}」`}</h3>
        <div className="prov-form">
          <input
            className="prov-modal-input"
            value={name}
            autoFocus
            placeholder="文件夹名，比如「NVIDIA」「官方直连」"
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && trimmed && !busy) onSubmit(trimmed);
              if (e.key === "Escape") onClose();
            }}
          />
        </div>
        <div className="prov-modal-actions">
          <button className="btn ghost" onClick={onClose} disabled={busy}>
            取消
          </button>
          <button
            className="btn primary"
            disabled={!trimmed || busy}
            onClick={() => onSubmit(trimmed)}
          >
            {busy ? "处理中…" : mode === "create" ? "创建" : "改名"}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

/** 勾选了供应商之后出现的批量移动条。
 *
 *  为什么不做"每行一个下拉框"：38 个供应商一人一个 select，页面先撑爆，
 *  而且真正想干的事是"把这 12 个一起丢进 NVIDIA"——一次选完比逐行点快得多。
 */
export function MoveBar({
  count,
  total,
  allPicked,
  folders,
  busy,
  onMove,
  onToggleAll,
  onClear,
}: {
  count: number;
  /** 当前视图里的供应商总数，"全选"的边界 */
  total: number;
  allPicked: boolean;
  /** 可选目标。已按注册表顺序排好 */
  folders: string[];
  busy: boolean;
  /** target = null 表示移到未分组 */
  onMove: (target: string | null) => void;
  onToggleAll: () => void;
  onClear: () => void;
}) {
  const [target, setTarget] = useState("");

  return (
    <div className="prov-movebar">
      <span className="prov-movebar-count">已选 {count} / {total}</span>
      <select
        className="prov-movebar-select"
        value={target}
        onChange={(e) => setTarget(e.target.value)}
      >
        <option value="">选择文件夹…</option>
        {folders.map((name) => (
          <option key={name} value={name}>
            {name}
          </option>
        ))}
        <option value={UNGROUPED_LABEL}>{UNGROUPED_LABEL}</option>
      </select>
      <button
        className="btn primary"
        disabled={!target || busy}
        onClick={() => {
          onMove(target === UNGROUPED_LABEL ? null : target);
          setTarget("");
        }}
      >
        移动
      </button>
      <button className="btn ghost" onClick={onToggleAll} disabled={busy}>
        {allPicked ? "清空选择" : `全选 ${total} 个`}
      </button>
      <button className="btn ghost" onClick={onClear} disabled={busy}>
        收起
      </button>
    </div>
  );
}
