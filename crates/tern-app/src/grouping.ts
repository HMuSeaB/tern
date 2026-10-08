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
 */

/** 按请求地址归组。
 *
 * 分组键是 Rust 侧算好的 `group_key`（完整规范化 URL），这里只做 Map 归拢。
 * 为什么是完整 URL 而不是 host：同一个 host 下 `/v1` 和 `/v2` 常是两个不同上游，
 * 按 host 合会把它们混成一组，用户点进去发现模型名对不上。
 *
 * 摊平规则：只有 1 个供应商的地址不单独成组，直接当普通行渲染。一个组的卡片
 * 比一行没多说什么，却多占一块视觉重量。
 */
export function buildUrlGroups(
  providers: ProviderSummary[],
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

  // 未配置地址沉底：它不是正常分组，夹在中间只会打断阅读
  return [
    ...groups.filter((g) => g.key !== NO_URL_LABEL),
    ...groups.filter((g) => g.key === NO_URL_LABEL),
  ];
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

  return [
    ...groups.filter((g) => g.title !== UNGROUPED_LABEL),
    ...groups.filter((g) => g.title === UNGROUPED_LABEL),
  ];
}

/** 已建文件夹的名字，保持注册表顺序（Rust 侧已按 sortIndex 排好）。 */
export function folderNames(folders: ProviderFolder[]): string[] {
  return folders.map((f) => f.name);
}
