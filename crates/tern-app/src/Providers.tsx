import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  FolderDialog,
  GroupCard,
  MoveBar,
} from "./ProviderGrouping";
import { ProviderEditor } from "./ProviderEditor";
import { buildFolderGroups, buildUrlGroups, folderNames } from "./grouping";
import type {
  ConfigSummary,
  DomainGroup,
  ProviderFolder,
  ProviderSummary,
} from "./types";

/**
 * 供应商列表：选一个当默认、看它的地址和状态、顺手拉一份模型列表，
 * 再加上**分组**。
 *
 * 功能分三层，越往后越省事：
 *
 * 1. **看有哪些**：38 个全列出，搜索框按名称/地址/id 都匹配——
 *    用户记得住 deepseek，记不住那串 uuid
 * 2. **换**：点一下切默认，即时生效（路由表换 default_provider，
 *    不用重启网关）。正在用的那个 key 坏了要直接说，否则用户
 *    只会看到"请求失败"，不知道是选错了供应商
 * 3. **拉模型列表**：中转站的模型名千奇百怪（step-3.5-flash-2603、
 *    kimi-k2.5），猜错只得到一句 404。上游有个 OpenAI 兼容的
 *    GET /v1/models，问它就行
 * 4. **分组**：38 个平铺就是个长名单，看不出"哪几个其实是同一个上游"。
 *    按请求地址归组是自动的（完整规范化 URL 相同即同组，
 *    见 Rust 侧 `folders::group_key`）；按文件夹归组是用户自己排的，
 *    存在 `%APPDATA%\tern\folders.json`，与网关配置分开
 */

/** 三种视图。做成单选而不是两个独立开关：两个开关会同时开，
 *  那时"到底听哪个"得靠代码里的优先级猜，用户看不出所以然。 */
type ViewMode = "flat" | "url" | "folder";

const VIEW_MODES: ReadonlyArray<{ id: ViewMode; label: string; hint: string }> = [
  { id: "flat", label: "平铺", hint: "所有供应商排成一个列表" },
  { id: "url", label: "按地址", hint: "按请求地址自动归组" },
  { id: "folder", label: "文件夹", hint: "按你自己排的文件夹分组" },
];

export function Providers({
  running,
  onRefresh,
}: {
  /** 网关在不在跑。没跑时也允许切换（配置先改好，启动即生效），但要提示 */
  running: boolean;
  onRefresh?: () => void;
}) {
  const [config, setConfig] = useState<ConfigSummary | null>(null);
  const [folders, setFolders] = useState<ProviderFolder[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  /** 展开模型列表的那个供应商 id。一次只展开一个：38 个全展开会把页面撑爆 */
  const [expanded, setExpanded] = useState<string | null>(null);

  const [mode, setMode] = useState<ViewMode>("flat");
  /** 勾了准备批量移动的供应商 id。只在文件夹视图下出现勾选框 */
  const [picked, setPicked] = useState<Set<string>>(new Set());
  /** 新建 / 重命名对话框 */
  const [dialog, setDialog] = useState<
    { kind: "create" } | { kind: "rename"; name: string } | null
  >(null);
  /** 供应商的新增 / 编辑弹层。null = 关着 */
  const [editor, setEditor] = useState<
    { mode: "create" } | { mode: "edit"; id: string } | null
  >(null);
  const [editorBusy, setEditorBusy] = useState(false);
  const [folderBusy, setFolderBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  /** 收起来的分组名（含地址视图的分组键）。空集 = 全展开 */
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  const reload = useCallback(async () => {
    try {
      // 两边一起拉：供应商带着自己的 folder 字段，注册表决定有哪些组可显示。
      // 分两次请求的话，中间改一下就会看到"有归属但没文件夹"的半成品状态
      const [summary, list] = await Promise.all([
        invoke<ConfigSummary>("config_summary"),
        invoke<ProviderFolder[]>("folders_list"),
      ]);
      setConfig(summary);
      setFolders(list);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const select = useCallback(
    async (id: string) => {
      setPending(id);
      try {
        setConfig(await invoke<ConfigSummary>("select_provider", { id }));
        setError(null);
        // useTern 的 boot 里带着 config，切换后要重新拉一次，
        // 否则"已配置 N 个供应商"和警告列表都是旧状态
        onRefresh?.();
      } catch (e) {
        setError(String(e));
      } finally {
        setPending(null);
      }
    },
    [onRefresh],
  );

  // ---- 增删改 ----

  /** 存 / 删之后统一走这里。Rust 侧的 `provider_save` / `provider_remove`
   *  返回的就是新的配置摘要，直接用它——省一次往返，也避免两次请求之间
   *  列表闪一下旧数据。 */
  const onEditorWritten = useCallback(
    (summary: ConfigSummary) => {
      setConfig(summary);
      setEditor(null);
      setEditorBusy(false);
      setError(null);
      // 顶栏的"已配置 N 个"和警告列表也要跟着变
      onRefresh?.();
    },
    [onRefresh],
  );

  // ---- 分组操作 ----

  /** 改过分组之后统一走这里：重新拉供应商 + 注册表，再顺带刷一下顶栏数字 */
  const reloadAfterFolderChange = useCallback(async () => {
    await reload();
    onRefresh?.();
  }, [reload, onRefresh]);

  const createFolder = useCallback(
    async (name: string) => {
      setFolderBusy(true);
      try {
        setFolders(await invoke<ProviderFolder[]>("folders_create", { name }));
        setDialog(null);
        setNotice(`已建文件夹「${name}」。切到「文件夹」视图，勾选供应商移进来。`);
        setError(null);
      } catch (e) {
        setError(String(e));
      } finally {
        setFolderBusy(false);
      }
    },
    [],
  );

  const renameFolder = useCallback(
    async (oldName: string, name: string) => {
      setFolderBusy(true);
      try {
        // 返回值是被移动的供应商数。Rust 侧连归属表一起改，所以这里不用再逐个挪
        const moved = await invoke<number>("folders_rename", {
          oldName,
          newName: name,
        });
        setDialog(null);
        setNotice(
          moved > 0
            ? `已改名为「${name}」，${moved} 个供应商跟着搬过去了`
            : `已改名为「${name}」`,
        );
        setError(null);
        await reloadAfterFolderChange();
      } catch (e) {
        setError(String(e));
      } finally {
        setFolderBusy(false);
      }
    },
    [reloadAfterFolderChange],
  );

  const removeFolder = useCallback(
    async (name: string) => {
      setFolderBusy(true);
      try {
        const moved = await invoke<number>("folders_delete", { name });
        setNotice(
          moved > 0
            ? `已解散「${name}」，${moved} 个供应商回到未分组`
            : `已解散「${name}」`,
        );
        setError(null);
        await reloadAfterFolderChange();
      } catch (e) {
        setError(String(e));
      } finally {
        setFolderBusy(false);
      }
    },
    [reloadAfterFolderChange],
  );

  const movePicked = useCallback(
    async (target: string | null) => {
      const ids = [...picked];
      if (ids.length === 0) return;
      setFolderBusy(true);
      try {
        await invoke<number>("folders_assign", { ids, folder: target });
        setPicked(new Set());
        setNotice(
          target
            ? `已把 ${ids.length} 个供应商移入「${target}」`
            : `已把 ${ids.length} 个供应商移到未分组`,
        );
        setError(null);
        await reloadAfterFolderChange();
      } catch (e) {
        setError(String(e));
      } finally {
        setFolderBusy(false);
      }
    },
    [picked, reloadAfterFolderChange],
  );

  /** 一键按域名归组。
   *
   *  为什么值得单独一个按钮：38 个供应商手排一遍要几分钟，而它们天然就挤在
   *  三五个域名下（NVIDIA、OpenRouter、DeepSeek…）。按下之后立刻能看出
   *  "哪几个其实是同一个上游"，之后想合并/改名再手工调。
   *  只把 >= 2 个供应商共用的域名建成文件夹——单例域名建组没有分组收益。
   */
  const groupByDomain = useCallback(async () => {
    setFolderBusy(true);
    try {
      const groups = await invoke<DomainGroup[]>("folders_group_by_domain");
      if (groups.length === 0) {
        setNotice("没有找到两个以上共用的请求域名，什么都没改。");
      } else {
        const created = groups.filter((g) => g.isNew).length;
        const assigned = groups.reduce((sum, g) => sum + g.providerIds.length, 0);
        setNotice(
          `按域名归好 ${assigned} 个供应商：${groups.map((g) => `${g.name}(${g.providerIds.length})`).join("、")}` +
            (created > 0 ? `，其中新建 ${created} 个文件夹` : ""),
        );
      }
      setMode("folder");
      setError(null);
      await reloadAfterFolderChange();
    } catch (e) {
      setError(String(e));
    } finally {
      setFolderBusy(false);
    }
  }, [reloadAfterFolderChange]);

  const saveExpanded = useCallback(async (name: string, expanded: boolean) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (expanded) next.delete(name);
      else next.add(name);
      return next;
    });
    // 只持久化注册表里有的（自定义文件夹）。地址视图的分组是算出来的，
    // 没有名字可挂，收起状态这一轮会话里有效就够了
    if (!folders.some((f) => f.name === name)) return;
    try {
      setFolders(await invoke<ProviderFolder[]>("folders_set_expanded", { name, expanded }));
    } catch {
      // 展开状态存不住不影响使用，别为它弹错误
    }
  }, [folders]);

  const togglePick = useCallback((id: string) => {
    setPicked((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  /** 提示自动消退。常驻不放的话，五分钟前那句"已建文件夹"会一直挂在最显眼处，
   *  看起来像还没做完。 */
  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), 6000);
    return () => window.clearTimeout(timer);
  }, [notice]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q || !config) return config?.providers ?? [];
    return config.providers.filter(
      (p) =>
        p.name.toLowerCase().includes(q) ||
        p.id.toLowerCase().includes(q) ||
        p.base_url.toLowerCase().includes(q) ||
        (p.folder ?? "").toLowerCase().includes(q),
    );
  }, [config, query]);

  const urlGroups = useMemo(
    () => (mode === "url" ? buildUrlGroups(visible) : []),
    [mode, visible],
  );
  const folderGroups = useMemo(
    () => (mode === "folder" ? buildFolderGroups(visible, folders) : []),
    [mode, visible, folders],
  );
  /** 按地址视图里那些"只有一家在用"的地址。摊平成一张平表，不单独占一张卡 */
  const singles = useMemo(
    () => urlGroups.flatMap((g) => (g.providers.length >= 2 ? [] : g.providers)),
    [urlGroups],
  );

  /** 文件夹视图的「全部收起 / 全部展开」。
   *
   *  会话集合和注册表里的 isExpanded 都要改：只改会话的话，重开面板又回到原状；
   *  只改注册表的话，会话里残留的 collapsed 会盖住它（见下面 open 的计算）。
   *  所以两边一起改。 */
  const setAllFolders = useCallback(
    async (expanded: boolean) => {
      const titles = folderGroups.map((g) => g.title);
      setCollapsed(expanded ? new Set() : new Set(titles));
      // 串行写：每次返回的都是完整注册表快照，并发的话后返回的快照会盖掉先返回的
      let latest: ProviderFolder[] | null = null;
      try {
        for (const folder of folders) {
          latest = await invoke<ProviderFolder[]>("folders_set_expanded", {
            name: folder.name,
            expanded,
          });
        }
      } catch {
        // 展开状态存不住不影响使用，和 saveExpanded 一样不为它弹错误
      }
      if (latest) setFolders(latest);
    },
    [folderGroups, folders],
  );

  if (!config) {
    return (
      <div className="card">
        <div className="head"><span className="title">供应商</span></div>
        <p className="empty-text">正在读取…</p>
      </div>
    );
  }

  const active = config.providers.find((p) => p.active);
  // 正在用的那个 key 是坏的：这是最该说的一句话，比用量数字更要紧
  const activeKeyBroken =
    active && active.key_state !== "real" && active.key_state !== "subscription";

  /** 一行供应商。勾选框只在文件夹视图出现——那是唯一需要"批量选中"的地方 */
  const renderRow = (p: ProviderSummary) => (
    <li key={p.id}>
      <div
        className={[
          "prov-row",
          p.active ? "is-active" : "",
          picked.has(p.id) ? "is-picked" : "",
          mode === "folder" ? "has-check" : "",
        ]
          .filter(Boolean)
          .join(" ")}
      >
        {mode === "folder" && (
          <input
            className="prov-check"
            type="checkbox"
            checked={picked.has(p.id)}
            onChange={() => togglePick(p.id)}
            title={`勾选「${p.name}」后批量移动到文件夹`}
          />
        )}
        <button
          className="prov-pick"
          onClick={() => void select(p.id)}
          disabled={pending !== null}
          title={`切换到 ${p.name}`}
        >
          <span className={`prov-radio ${p.active ? "on" : ""}`} aria-hidden />
          <span className="prov-main">
            <span className="prov-name">
              {p.name}
              {p.active && <span className="prov-tag">使用中</span>}
            </span>
            <span className="prov-url" title={p.base_url}>{p.base_url}</span>
          </span>
        </button>
        <span className="prov-side">
          <code className="prov-fmt">{p.api_format}</code>
          {p.web_tools_at_risk && (
            <span className="prov-risk" title="第三方网关：Claude Code 的联网搜索会失效">
              联网受限
            </span>
          )}
          {p.key_state === "placeholder" && <span className="prov-bad">占位符</span>}
          {p.key_state === "empty" && <span className="prov-bad">key 为空</span>}
          {/* 编辑：换名字、修地址、改 key、测连通性都在这儿 */}
          <button
            className="prov-edit-link"
            onClick={() => setEditor({ mode: "edit", id: p.id })}
            title={`编辑「${p.name}」：地址、key、成本倍率、连通性测试`}
          >
            编辑
          </button>
          {/* 拉模型列表：key 是坏的就没必要问上游了，先让它醒目标出来 */}
          {(p.key_state === "real" || p.key_state === "subscription") && (
            <button
              className="prov-models-btn"
              onClick={() => setExpanded(expanded === p.id ? null : p.id)}
              title="获取这个供应商的模型列表"
            >
              {expanded === p.id ? "收起模型" : "获取模型列表"}
            </button>
          )}
        </span>
      </div>
      {expanded === p.id && <ModelList providerId={p.id} running={running} />}
    </li>
  );

  return (
    <div className="card">
      <div className="head">
        <span className="title">供应商</span>
        <span className="act">
          {config.providers.length} 个
          {active ? ` · 当前 ${active.name}` : " · 未设置默认"}
        </span>
      </div>

      {config.default_provider === null && (
        <p className="perm-hint">
          还没有默认供应商。点下面任意一个，之后不带前缀的模型名都会走它。
        </p>
      )}

      {activeKeyBroken && (
        <div className="notice err" style={{ marginBottom: 14 }}>
          <span className="notice-ic">!</span>
          <span className="notice-body">
            <b>正在用的「{active.name}」key 不能用了。</b>
            请求会全部失败。请在下面换一个——key 正常的旁边没有红字。
          </span>
        </div>
      )}

      <div className="prov-toolbar">
        <div className="prov-seg" role="group" aria-label="分组方式">
          {VIEW_MODES.map((view) => (
            <button
              key={view.id}
              className={`prov-seg-btn ${mode === view.id ? "on" : ""}`}
              onClick={() => {
                setMode(view.id);
                // 换视图时清掉勾选：上一视图勾的是"要移动的"，换过去语义就断了
                setPicked(new Set());
              }}
              title={view.hint}
              aria-pressed={mode === view.id}
            >
              {view.label}
            </button>
          ))}
        </div>
        <div className="prov-tools">
          <button
            className="prov-mini accent"
            onClick={() => setEditor({ mode: "create" })}
            title="从零加一个供应商，不用去改 tern.json"
          >
            新增供应商
          </button>
          <button
            className="prov-mini"
            onClick={() => setDialog({ kind: "create" })}
            title="建一个空文件夹，之后把供应商勾选移进来"
          >
            新建文件夹
          </button>
          <button
            className="prov-mini"
            onClick={() => void groupByDomain()}
            disabled={folderBusy}
            title="按请求地址的域名根自动归组：同一域名下两个以上供应商才会建文件夹"
          >
            按域名归组
          </button>
          {mode === "folder" && folderGroups.length > 0 && (
            <>
              <button
                className="prov-mini"
                onClick={() => void setAllFolders(false)}
                disabled={folderBusy}
                title="把当前可见的所有文件夹收起来,下次打开面板也保持收起"
              >
                全部收起
              </button>
              <button
                className="prov-mini"
                onClick={() => void setAllFolders(true)}
                disabled={folderBusy}
                title="展开所有文件夹"
              >
                全部展开
              </button>
            </>
          )}
        </div>
      </div>

      {config.providers.length > 8 && (
        <input
          className="prov-search"
          type="search"
          value={query}
          placeholder={`搜索 ${config.providers.length} 个供应商（名称、地址、id 或文件夹）`}
          onChange={(e) => setQuery(e.target.value)}
        />
      )}

      {notice && (
        <div className="notice ok" style={{ marginBottom: 12 }}>
          <span className="notice-ic">✓</span>
          <span className="notice-body">{notice}</span>
        </div>
      )}

      {!running && (
        <p className="perm-foot" style={{ marginTop: 0 }}>
          网关没在跑，现在切换会在下次启动时生效。
        </p>
      )}

      {mode === "flat" && (
        <ul className="prov-list">
          {visible.map(renderRow)}
          {visible.length === 0 && (
            <li className="prov-empty">没有匹配「{query}」的供应商</li>
          )}
        </ul>
      )}

      {mode === "url" && (
        <div className="prov-groups">
          {urlGroups
            /* 只有一家在用的地址不值得一张卡：摊平成普通行，
               "一个组的标题里只有一行内容"省不下任何空间 */
            .filter((group) => group.providers.length >= 2)
            .map((group) => (
              <GroupCard
                key={group.key}
                group={group}
                expanded={!collapsed.has(group.key)}
                onToggle={() => void saveExpanded(group.key, collapsed.has(group.key))}
              >
                <ul className="prov-list">{group.providers.map(renderRow)}</ul>
              </GroupCard>
            ))}

          {/* 单例地址合成一张平表放在最后：分组在前、长尾在后，
              阅读顺序才是"先看哪些真是同一个上游" */}
          {singles.length > 0 && (
            <ul className="prov-list">{singles.map(renderRow)}</ul>
          )}

          {urlGroups.length === 0 && (
            <p className="prov-empty">没有匹配「{query}」的供应商</p>
          )}
        </div>
      )}

      {mode === "folder" && (
        <div className="prov-groups">
          {folderGroups.map((group) => {
            const persisted = folders.find((f) => f.name === group.title);
            // 注册表里的文件夹用用户上次收起的记忆；算出来的组（未登记归属）默认展开
            const open = collapsed.has(group.title)
              ? false
              : (persisted?.isExpanded ?? true);
            return (
              <GroupCard
                key={group.key}
                group={group}
                expanded={open}
                onToggle={() => void saveExpanded(group.title, !open)}
                actions={
                  group.custom ? (
                    <>
                      <button
                        className="prov-mini"
                        onClick={() => setDialog({ kind: "rename", name: group.title })}
                        title="重命名文件夹。归属它的供应商会一起跟着改名"
                      >
                        改名
                      </button>
                      <button
                        className="prov-mini danger"
                        onClick={() => void removeFolder(group.title)}
                        title="解散文件夹，里面的供应商回到未分组"
                      >
                        解散
                      </button>
                    </>
                  ) : undefined
                }
              >
                <ul className="prov-list">{group.providers.map(renderRow)}</ul>
              </GroupCard>
            );
          })}
          {folderGroups.length === 0 && (
            <p className="prov-empty">没有匹配「{query}」的供应商</p>
          )}
        </div>
      )}

      {mode === "folder" && picked.size > 0 && (
        <MoveBar
          count={picked.size}
          total={visible.length}
          allPicked={picked.size === visible.length}
          folders={folderNames(folders)}
          busy={folderBusy}
          onMove={(target) => void movePicked(target)}
          onToggleAll={() =>
            setPicked(
              picked.size === visible.length
                ? new Set()
                : new Set(visible.map((p) => p.id)),
            )
          }
          onClear={() => setPicked(new Set())}
        />
      )}

      {error && (
        <div className="notice err" style={{ marginTop: 14 }}>
          <span className="notice-ic">!</span>
          <span className="notice-body">{error}</span>
          <button className="btn ghost" onClick={() => void reload()}>重试</button>
        </div>
      )}

      {editor && (
        <ProviderEditor
          // key 让 create → edit 的切换重新挂载：不挂的话 useState 的初始值
          // 只在第一次生效，从"新建"跳到"编辑另一个"时表单里还是上次那些字
          key={editor.mode === "create" ? "create" : `edit:${editor.id}`}
          mode={editor.mode}
          providerId={editor.mode === "edit" ? editor.id : undefined}
          busy={editorBusy}
          onClose={() => setEditor(null)}
          onSaved={(summary) => onEditorWritten(summary)}
          onDeleted={(summary) => onEditorWritten(summary)}
        />
      )}

      {dialog && (
        <FolderDialog
          // key 让 create → rename 的切换重新挂载：不挂的话 useState 的初始值
          // 只在第一次生效，重命名框里会留着上次新建时输入的文字
          key={dialog.kind === "create" ? "create" : `rename:${dialog.name}`}
          mode={dialog.kind}
          oldName={dialog.kind === "rename" ? dialog.name : undefined}
          initial={dialog.kind === "rename" ? dialog.name : ""}
          busy={folderBusy}
          onClose={() => setDialog(null)}
          onSubmit={(name) => {
            if (dialog.kind === "create") void createFolder(name);
            else void renameFolder(dialog.name, name);
          }}
        />
      )}

      <p className="perm-foot">
        切换立即生效，不用重启网关。带前缀的模型名（<code>deepseek/xxx</code>）不受影响，
        它们总是按前缀走。
        {mode === "folder" && "分组只影响这里的显示，不改路由。"}
      </p>
    </div>
  );
}

/** 一个供应商的模型列表。点「获取模型列表」才拉，不预取——38 个全拉一遍
 *  会把上游打烦，而且大部分 key 是坏的，拉回来一堆错误信息没意义。 */
function ModelList({ providerId, running }: { providerId: string; running: boolean }) {
  const [models, setModels] = useState<string[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const list = await invoke<string[]>("fetch_provider_models", { id: providerId });
      setModels(list);
      setError(null);
    } catch (e) {
      setModels(null);
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [providerId]);

  useEffect(() => {
    void load();
  }, [load]);

  if (busy && !models) {
    return <p className="prov-models">正在向上游询问…</p>;
  }

  if (error) {
    return (
      <div className="prov-models is-err">
        <span>{error}</span>
        <button className="btn ghost" onClick={() => void load()}>重试</button>
      </div>
    );
  }

  if (!models) return null;

  if (models.length === 0) {
    return <p className="prov-models">上游返回了空列表。</p>;
  }

  return (
    <div className="prov-models">
      <div className="prov-models-head">
        <span>{models.length} 个模型</span>
        <span className="dim">复制名字填进 Claude Code 的 model 里即可</span>
      </div>
      <ul className="prov-models-grid">
        {models.map((m) => (
          <li key={m}>
            <code>{m}</code>
          </li>
        ))}
      </ul>
      {!running && (
        <p className="dim" style={{ marginTop: 8, fontSize: 12 }}>
          网关没在跑，现在只能看模型名，要真发请求得先启动。
        </p>
      )}
    </div>
  );
}
