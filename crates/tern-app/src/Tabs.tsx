import { useCallback, type KeyboardEvent, type ReactNode } from "react";

/** tab 的定义。`alert` 是个小圆点，用来把"被这一页挡住的提醒"露到 tab 栏上 */
export interface TabDef {
  id: string;
  label: string;
  alert?: boolean;
}

/**
 * 顶部 tab 容器：一条 tab 栏 + 一个横滑视口。
 *
 * # 为什么四块全留着挂载，而不是只渲染当前页
 *
 * 切走的页用 `inert` 关掉焦点和点击，**不从 DOM 里摘掉**。两个理由：
 *
 * 1. `usePanel` 的轮询在 App 里，不随 tab 卸载——翻回「用量」时数据是刚拉的，
 *    不用先看一个空页面再等下一次 tick
 * 2. 分组视图的展开/收起、搜索框里的字、翻开的模型列表，切走再切回还在。
 *    摘掉的话用户每次回来都得把 38 个供应商的组重新展开一遍
 *
 * 代价是四页同时存在于布局里，所以离屏那几页必须 `inert`——不禁的话 Tab 键会
 * 一路跑进看不见的内容里。
 *
 * # 指示条为什么按等分算
 *
 * tab 一律 `flex: 1` 等宽。不等宽的话指示条得量 DOM 尺寸，而横滑本身只需要
 * "第几页"，等分让 `translateX(index * 100%)` 直接成立，不引入测量。
 */
export function TabShell({
  tabs,
  active,
  onSelect,
  children,
}: {
  tabs: TabDef[];
  active: string;
  onSelect: (id: string) => void;
  children: ReactNode;
}) {
  const index = Math.max(0, tabs.findIndex((t) => t.id === active));

  // role="tablist" 该有的方向键。没有它的话键盘用户得 Tab 到栏上再用回车，
  // 而视觉上明明是一排横向的东西
  const onKeyDown = useCallback(
    (event: KeyboardEvent) => {
      const step = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
      if (step === 0) return;
      event.preventDefault();
      onSelect(tabs[(index + step + tabs.length) % tabs.length].id);
    },
    [index, onSelect, tabs],
  );

  return (
    <div className="tabs">
      <div className="tabbar" role="tablist" onKeyDown={onKeyDown}>
        {tabs.map((tab) => {
          const on = tab.id === active;
          return (
            <button
              key={tab.id}
              type="button"
              role="tab"
              id={`tab-${tab.id}`}
              aria-selected={on}
              aria-controls={`panel-${tab.id}`}
              tabIndex={on ? 0 : -1}
              className={`tabbtn ${on ? "on" : ""}`}
              onClick={() => onSelect(tab.id)}
            >
              <span>{tab.label}</span>
              {tab.alert && <span className="tab-dot" aria-hidden />}
            </button>
          );
        })}
        <span
          className="tab-ink"
          aria-hidden
          style={{ width: `${100 / tabs.length}%`, transform: `translateX(${index * 100}%)` }}
        />
      </div>
      <div className="tab-viewport">
        <div className="tab-track" style={{ transform: `translateX(-${index * 100}%)` }}>
          {children}
        </div>
      </div>
    </div>
  );
}

/** tab 里的一页。宽高由 `.tab-track` / `.tab-viewport` 定，内容自己滚。 */
export function TabPage({
  id,
  active,
  children,
}: {
  id: string;
  active: boolean;
  children: ReactNode;
}) {
  return (
    <section
      className="tab-page"
      role="tabpanel"
      id={`panel-${id}`}
      aria-labelledby={`tab-${id}`}
      aria-hidden={active ? undefined : true}
      inert={!active}
    >
      {children}
    </section>
  );
}
