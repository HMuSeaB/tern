import type { Group, ProviderFolder, ProviderSummary } from "./types";
import { NO_URL_LABEL, UNGROUPED_LABEL } from "./types";

/**
 * 两种自动归组的算法。都是纯函数：输入供应商数组（已经过搜索过滤）+ 文件夹注册表，
 * 输出有顺序的分组数组。Providers.tsx 只管照着渲染。
 *
 * 两条规则跨两种视图通用：
 *
 * - **当前在用的供应商所在组要高亮**（`containsActive`）。38 个供应商摊开之后，
 *   "我刚切的是哪个"是最常问的问题，不该让用户再去扫一遍
 * - **顺序反映真实顺序**：地址视图按第一次出现的顺序，文件夹视图按注册表顺序。
 *   两边都刻意不用 `localeCompare`——字母序会把用户排好的顺序打乱，而中文名按
 *   字母排出来基本是乱的，没好作用
 *
 * # 关于"常用地址排前面"
 *
 * `usage`（provider_id → 近 30 天加权请求数）是**次级排序**，不是整体重排。
 * 整体按字母/用量重排会打乱用户自己排的顺序，那是这两条规则存在的理由。
 * 所以只分成三档：
 *
 * 1. 含当前使用中供应商的组（`containsActive`，最该看见）
 * 2. 有用量的组，按组内用量之和降序
 * 3. 没用量的组，**保持原顺序不动**
 *
 * 第 3 档是关键：那 36 个不怎么用的供应商是用户排的、也是从 cc-switch
 * 导入时的原始顺序，按用量排它们等于把导入成果洗掉。
 */

/** 组内用量之和。同一个地址下挂几个供应商就累加几份——那正是
 *  "这个上游被用了多少次"的含义，分组的意义也在这儿。 */
function usageOf(
  group: Group<ProviderSummary>,
  usage: Map<string, number>,
): number {
  let sum = 0;
  for (const p of group.providers) sum += usage.get(p.id) ?? 0;
  return sum;
}

/** 次级排序：使用中的组最前，其次按用量，最后保持原顺序。
 *
 *  用稳定的 `Array.prototype.sort`（ES2019 起规定稳定），同档内不重排。
 *  `usageOf` 每次比较都重算，38 个供应商的规模下无所谓；
 *  真嫌它慢可以先 map 成带权值的元组再排，但那是优化不是正确性问题。 */
function byUsage(
  groups: Group<ProviderSummary>[],
  usage: Map<string, number>,
): Group<ProviderSummary>[] {
  if (usage.size === 0) return groups;
  return [...groups].sort((a, b) => {
    if (a.containsActive !== b.containsActive) return a.containsActive ? -1 : 1;
    return usageOf(b, usage) - usageOf(a, usage);
  });
}

/** 按请求地址归组。
 *
 * 分组键是 Rust 侧算好的 `group_key`（完整规范化 URL），这里只做 Map 归拢。
 * 为什么是完整 URL 而不是 host：同一个 host 下 `/v1` 和 `/v2` 常是两个不同上游，
 * 按 host 合会把它们混成一组，用户点进去发现模型名对不上。
 *
 * 摊平规则：只有 1 个供应商的地址不单独成组，直接当普通行渲染。一个组的卡片
 * 比一行没多说什么，却多占一块视觉重量。
 *
 * `usage` 可省略：探针和没有用量库的场景传空 Map，顺序退化成"第一次出现"。
 */
export function buildUrlGroups(
  providers: ProviderSummary[],
  usage: Map<string, number> = new Map(),
): Group<ProviderSummary>[] {
  const groups: Group<ProviderSummary>[] = [];
  const index = new Map<string, Group<ProviderSummary>>();

  for (const provider of providers) {
    // group_key 为空 = 没配地址。仍然归组（它是个真实现状），但排到末尾
    const key = provider.group_key || NO_URL_LABEL;
    let group = index.get(key);
    if (!group) {
      group = {
        key,
        title: key,
        providers: [],
        containsActive: false,
        custom: false,
      };
      index.set(key, group);
      groups.push(group);
    }
    group.providers.push(provider);
    if (provider.active) group.containsActive = true;
  }

  // 未配置地址沉底：它不是正常分组，夹在中间只会打断阅读。
  // 沉底在排序之前做，否则"常用"会把一个没配地址的组顶到前面来
  const ordered = [
    ...groups.filter((g) => g.key !== NO_URL_LABEL),
    ...groups.filter((g) => g.key === NO_URL_LABEL),
  ];
  return byUsage(ordered, usage);
}

/** 按自定义文件夹归组。
 *
 * - **注册表铺骨架**：先把每个已建文件夹铺成空组，空的也显示。用户建了文件夹
 *   却看不见它，只会以为创建失败了
 * - **未登记的名字追加在后面**：归属可以指向注册表里没有的文件夹（从 cc-switch
 *   导入时带来的就是这样）。这些组照样显示，只是不能改名/解散，直到被补登记
 * - **未分组恒沉底**：它通常是最多的那一组，放最前面会把用户自己排的组全推到下面
 */
export function buildFolderGroups(
  providers: ProviderSummary[],
  folders: ProviderFolder[],
  usage: Map<string, number> = new Map(),
): Group<ProviderSummary>[] {
  const groups: Group<ProviderSummary>[] = [];
  const index = new Map<string, Group<ProviderSummary>>();

  for (const folder of folders) {
    const group: Group<ProviderSummary> = {
      key: folder.name,
      title: folder.name,
      providers: [],
      containsActive: false,
      custom: true,
    };
    index.set(folder.name, group);
    groups.push(group);
  }

  for (const provider of providers) {
    const name = provider.folder?.trim() || UNGROUPED_LABEL;
    let group = index.get(name);
    if (!group) {
      group = { key: name, title: name, providers: [], containsActive: false, custom: false };
      index.set(name, group);
      groups.push(group);
    }
    group.providers.push(provider);
    if (provider.active) group.containsActive = true;
  }

  // 未分组恒沉底：它通常是最多的那一组，放最前面会把用户自己排的组全推到下面。
  // 同样先沉底再按用量排，否则"未分组"会因为装得多而被顶上来
  const ordered = [
    ...groups.filter((g) => g.title !== UNGROUPED_LABEL),
    ...groups.filter((g) => g.title === UNGROUPED_LABEL),
  ];
  return byUsage(ordered, usage);
}

/** 已建文件夹的名字，保持注册表顺序（Rust 侧已按 sortIndex 排好）。 */
export function folderNames(folders: ProviderFolder[]): string[] {
  return folders.map((f) => f.name);
}
