/*! embhub 布局内核（H1a）——**纯函数、不碰 DOM、零依赖**。
 *
 * 为什么要有这个文件：旧布局是「一列 flex 块互相协商高度」，谁该让、让多少全凭
 * CSS 隐式规则（t17 的挤压/重叠就是这么来的）。现在改成
 * **区域对象 + 分割树 + 尺寸求解**：
 *
 *   · 区域（region）：有 id/标题/最小尺寸/权重/能否关闭/默认是否可见；
 *   · 分割树（tree）：行列嵌套，叶子就是区域；
 *   · 求解（solve）：**每层先扣掉所有子节点最小值，剩下的按权重分配**；
 *     任何区域**永不低于最小值**；空间不够时大家各停最小值，剩下的交给整页滚动兜底；
 *     可见集合变化导致某层只剩一个孩子时，该孩子拿满整层（= 塌缩，空间被兄弟吸收）。
 *
 * 这个文件同时供两种环境使用：
 *   · 浏览器：`<script src="layout.js">` → `window.embhubLayout`
 *   · Node  ：`require("./layout.js")` → 同一套 API（`node ui/layout.test.js` 就能独立验证）
 * 它**不引用 document/window/localStorage**，所以能在 Node 里跑（这是 captain 能独立复跑的那一半）。
 */
(function (root, factory) {
  const api = factory();
  if (typeof module !== "undefined" && module.exports) {
    module.exports = api; // Node：零依赖 CommonJS
  } else {
    root.embhubLayout = api; // 浏览器全局
  }
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  /** 用户比例的取值范围（超出即夹到边界；坏数据回退 1）。 */
  const RATIO_MIN = 0.25;
  const RATIO_MAX = 4;

  /**
   * **唯一的区域表**：界面上的四个区域都在这里定义，别处不许再写第二份。
   *
   * `minW/minH` 是**硬下限**（低于它就会重叠/控件被压没），`weight` 决定剩余空间怎么分
   * （weight 为 0 = 尺寸固定，不参与增长），`closable` 决定用户能不能关掉它。
   */
  const REGIONS = {
    params: {
      id: "params",
      title: "参数区",
      minW: 300,
      minH: 200,
      weight: 0, // 左列固定宽，不参与横向增长
      closable: false,
      defaultVisible: true,
    },
    tx: {
      id: "tx",
      title: "发送区",
      minW: 320,
      minH: 120,
      weight: 1,
      closable: true,
      defaultVisible: true,
    },
    rx: {
      id: "rx",
      title: "接收区",
      minW: 320,
      minH: 140,
      weight: 3, // 接收区是主角：吃剩余高度
      closable: false, // 接收区不给关（关了串口助手就没意义了）
      defaultVisible: true,
    },
    events: {
      id: "events",
      title: "事件与错误",
      minW: 260,
      minH: 96,
      weight: 1,
      closable: true,
      defaultVisible: true,
    },
  };

  /** 四个区域的固定顺序（界面/测试都用它，避免各处各写一遍顺序）。 */
  const REGION_ORDER = ["params", "tx", "rx", "events"];

  /**
   * 分割树：左列 `params`，右列纵向 `tx / rx / events`。
   *
   * `kind: "cols"` = 横向切（算宽度），`rows` = 纵向切（算高度）。
   */
  const TREE = {
    id: "root",
    kind: "cols",
    gap: 8,
    children: [
      { id: "left", kind: "leaf", region: "params" },
      {
        id: "right",
        kind: "rows",
        gap: 8,
        children: [
          { id: "row-tx", kind: "leaf", region: "tx" },
          { id: "row-rx", kind: "leaf", region: "rx" },
          { id: "row-events", kind: "leaf", region: "events" },
        ],
      },
    ],
  };

  function isLeaf(node) {
    return node.kind === "leaf";
  }

  /** 该节点的子树里是否有可见区域。 */
  function subtreeVisible(node, visible, regions) {
    if (isLeaf(node)) {
      const r = regions[node.region];
      return visible[r.id] !== false;
    }
    return node.children.some((ch) => subtreeVisible(ch, visible, regions));
  }

  /** 子树的最小高度/宽度（容器 = 子项最小值之和 + 间隙；跨轴取最大）。 */
  function subtreeMin(node, axis, visible, regions) {
    if (isLeaf(node)) {
      const r = regions[node.region];
      return axis === "w" ? r.minW : r.minH;
    }
    const kids = node.children.filter((ch) => subtreeVisible(ch, visible, regions));
    if (kids.length === 0) return 0;
    const own = node.kind === "cols" ? "w" : "h";
    const mins = kids.map((k) => subtreeMin(k, axis, visible, regions));
    if (axis === own) {
      return mins.reduce((a, b) => a + b, 0) + (node.gap || 0) * (kids.length - 1);
    }
    return Math.max(...mins);
  }

  /** 子树的权重和（容器可增长的能力 = 孩子的权重和）。 */
  function subtreeWeight(node, visible, regions) {
    if (isLeaf(node)) {
      const r = regions[node.region];
      return visible[r.id] === false ? 0 : r.weight;
    }
    return node.children.reduce(
      (sum, ch) => sum + subtreeWeight(ch, visible, regions),
      0
    );
  }

  /**
   * 取某个孩子节点的用户比例。
   *
   * 叶子按**区域 id**（界面上用户拖的就是区域比例），容器按**节点 id**；
   * 两者都没有就回退 1。这样 `{ rx: 0.5 }` 与 `{ right: 2 }` 都能表达。
   */
  function ratioFor(node, ratios) {
    const keys = isLeaf(node) ? [node.region, node.id] : [node.id];
    for (const k of keys) {
      const v = ratios[k];
      if (v != null && Number.isFinite(Number(v))) return Number(v);
    }
    return 1;
  }

  /**
   * 取某个孩子节点的**拖拽钉住值**（t24：分隔条拖拽 = 把相邻两区钉在明确的像素尺寸上）。
   *
   * 与 [`ratioFor`] 同一套双查：叶子先按区域 id、再按节点 id；容器按节点 id。
   * 坏值（非数字/<=0/离谱大）直接当作没钉 —— localStorage 里的东西不可信。
   */
  function pinFor(node, sizes, regions) {
    if (!sizes) return null;
    const keys = isLeaf(node) ? [node.region, node.id] : [node.id];
    for (const k of keys) {
      const v = sizes[k];
      const n = Number(v);
      if (v != null && Number.isFinite(n) && n > 0 && n < 1e6) return n;
    }
    return null;
  }

  /** 找树上的节点（按节点 id）。 */
  function findNode(node, id) {
    if (!node) return null;
    if (node.id === id) return node;
    if (isLeaf(node)) return null;
    for (const ch of node.children) {
      const hit = findNode(ch, id);
      if (hit) return hit;
    }
    return null;
  }

  /** 某容器**当前可见**的孩子（顺序与网格轨道一致）。 */
  function visibleChildren(node, visible, regions) {
    return node.children.filter((ch) => subtreeVisible(ch, visible, regions));
  }

  /** 容器的切分轴：`cols` 切宽、`rows` 切高。 */
  function axisOf(node) {
    return node.kind === "cols" ? "w" : "h";
  }

  /**
   * 拖拽一对**相邻**区域之间的分隔条（t24）。
   *
   * 语义：**等量反向、空间守恒**——`a` 涨多少 `b` 就减多少，其它区域一动不动；
   * 两侧都 respect 自己的最小值，到极限就**停住**（返回 `applied` 为实际生效的位移，
   * `clamped: true` 表示请求被夹过）。返回的是**新的 `sizes`**（把这一对钉住），
   * 由调用方写进状态并持久化。
   *
   * @param {object} opts
   * @param {string} opts.splitNodeId 分隔条所在容器的节点 id
   * @param {number} opts.index 分隔条左侧/上侧那个孩子在**可见列表**里的下标
   * @param {number} opts.deltaPx 请求的位移（正 = a 变大 / b 变小）
   */
  function resizePair(opts) {
    const o = opts || {};
    const tree = o.tree || TREE;
    const regions = o.regions || REGIONS;
    const visible = normalizeVisible(o.visible, regions);
    const ratios = normalizeRatios(o.ratios, regions);
    const sizes = normalizeSizes(o.sizes);
    const node = findNode(tree, o.splitNodeId);
    if (!node || isLeaf(node)) {
      return { sizes: sizes, applied: 0, requested: o.deltaPx, clamped: false, reason: "找不到容器 " + o.splitNodeId };
    }
    const kids = visibleChildren(node, visible, regions);
    if (kids.length < 2 || o.index < 0 || o.index + 1 >= kids.length) {
      return { sizes: sizes, applied: 0, requested: o.deltaPx, clamped: false, reason: "该容器没有相邻的一对区域" };
    }
    const axis = axisOf(node);
    // 基准尺寸取自**当前解**，保证与屏幕上的实际尺寸严格一致
    const solved = solve({ tree: tree, regions: regions, size: o.size, visible: visible, ratios: ratios, sizes: sizes });
    const track = solved.tracks[node.id];
    if (!track) {
      return { sizes: sizes, applied: 0, requested: o.deltaPx, clamped: false, reason: "容器没有轨道" };
    }
    const a = track.sizes[o.index];
    const b = track.sizes[o.index + 1];
    const minA = subtreeMin(kids[o.index], axis, visible, regions);
    const minB = subtreeMin(kids[o.index + 1], axis, visible, regions);
    const requested = Number(o.deltaPx) || 0;
    // 双侧最小值 clamp：a 不能低于 minA、b 不能低于 minB
    const lower = minA - a;
    const upper = b - minB;
    const d = Math.max(lower, Math.min(upper, requested));
    const next = Object.assign({}, sizes);
    next[kids[o.index].id] = a + d;
    next[kids[o.index + 1].id] = b - d;
    return {
      sizes: next,
      applied: d,
      requested: requested,
      clamped: Math.abs(d - requested) > 1e-9,
      a: a + d,
      b: b - d,
      axis: axis,
    };
  }

  /**
   * 能否隐藏某区域（t24）。
   *
   * 两条拒绝理由：① 不可关的常驻区域；② **最后一个可见区域**——关掉它界面就空了，
   * 而且没有任何入口能再打开回来。拒绝时必须给出原因（界面原样显示）。
   */
  function canHide(visible, id, regions) {
    const regs = regions || REGIONS;
    const def = regs[id];
    if (!def) return { ok: false, reason: "未知区域: " + id };
    if (!def.closable) {
      return { ok: false, reason: def.title + " 是常驻区域，不允许关闭" };
    }
    const vis = normalizeVisible(visible, regs);
    const count = Object.keys(regs).filter((k) => vis[k]).length;
    if (count <= 1) {
      return {
        ok: false,
        reason: "不能关掉最后一个可见区域（界面会空掉且无法恢复）；请先打开另一个区域",
      };
    }
    return { ok: true, reason: "" };
  }

  /** 尺寸钉住的规整：坏值/越界直接丢弃（回退成「没钉」）。 */
  function normalizeSizes(raw) {
    const src = raw && typeof raw === "object" && !Array.isArray(raw) ? raw : {};
    const out = {};
    for (const key of Object.keys(src)) {
      const n = Number(src[key]);
      if (Number.isFinite(n) && n > 0 && n < 1e6) out[key] = n;
    }
    return out;
  }

  /**
   * 求解布局：**纯函数**，输入尺寸/可见性/比例/钉住尺寸，输出每个区域的矩形 + 每层容器的轨道。
   *
   * @param {object} opts
   * @param {object} [opts.tree]     分割树（默认 TREE）
   * @param {object} [opts.regions]  区域表（默认 REGIONS）
   * @param {{width:number,height:number}} opts.size 容器尺寸（像素）
   * @param {object} [opts.visible]  可见性集合（缺省用 defaultVisible）
   * @param {object} [opts.ratios]   用户比例（乘到权重上；缺省 1）
   * @param {object} [opts.sizes]    拖拽钉住的像素尺寸（按节点 id；缺省无）
   * @returns {{rects:object, tracks:object, collapsed:string[], minSize:{width:number,height:number}, overflow:{x:number,y:number}}}
   */
  function solve(opts) {
    const o = opts || {};
    const tree = o.tree || TREE;
    const regions = o.regions || REGIONS;
    const size = o.size || { width: 0, height: 0 };
    const visible = normalizeVisible(o.visible, regions);
    const ratios = normalizeRatios(o.ratios, regions);
    const sizes = normalizeSizes(o.sizes);

    const rects = {};
    const tracks = {};
    const collapsed = [];

    function place(node, box) {
      if (isLeaf(node)) {
        const r = regions[node.region];
        rects[r.id] = { id: r.id, x: box.x, y: box.y, w: box.w, h: box.h };
        return;
      }
      const kids = node.children.filter((ch) => subtreeVisible(ch, visible, regions));
      if (kids.length === 0) return; // 整棵子树都不可见：不产出任何矩形
      // 塌缩：只剩一个可见孩子 → 它拿满整层（空间自然被兄弟/父层吸收）
      if (kids.length === 1) collapsed.push(kids[0].id);

      const axis = node.kind === "cols" ? "w" : "h";
      const total = axis === "w" ? box.w : box.h;
      const gap = node.gap || 0;
      const avail = total - gap * (kids.length - 1);
      const mins = kids.map((k) => subtreeMin(k, axis, visible, regions));
      const minSum = mins.reduce((a, b) => a + b, 0);

      // 拖拽钉住的尺寸（t24）：被钉住的孩子按钉住值占位，且**不再参与增长**。
      const pins = kids.map((k) => pinFor(k, sizes, regions));

      let outSizes;
      if (avail <= minSum) {
        // 空间不够：每个孩子停在自己的最小值，**绝不压到最小值以下**。
        // （钉住值也一律让位于最小值：宁可布局回到最小值，也不能压穿。）
        outSizes = mins.slice();
      } else {
        // ① 先给每个孩子最小值，被钉住的抬到钉住值（但不越过「留够别人最小值」的余量）
        const out = mins.slice();
        let used = minSum;
        for (let i = 0; i < kids.length; i += 1) {
          if (pins[i] == null) continue;
          const othersMin = minSum - mins[i];
          const cap = Math.max(mins[i], avail - othersMin);
          // **下限是区域自己的最小值**：钉住值再小也不能把区域钉穿（那样就重叠/压没了）
          const want = Math.max(mins[i], Math.min(pins[i], cap));
          used += want - out[i];
          out[i] = want;
        }
        // ② 剩余空间按权重分给**没被钉住**的孩子（全被钉住时给第一个未钉住的，尽量铺满）
        const leftover = avail - used;
        if (leftover > 0) {
          const weights = kids.map((k, i) => {
            if (pins[i] != null) return 0;
            return Math.max(0, subtreeWeight(k, visible, regions)) * Math.max(0, ratioFor(k, ratios));
          });
          const wsum = weights.reduce((a, b) => a + b, 0);
          if (wsum > 0) {
            kids.forEach((k, i) => {
              out[i] += (leftover * weights[i]) / wsum;
            });
          } else {
            const idx = kids.findIndex((k, i) => pins[i] == null);
            if (idx >= 0) out[idx] += leftover;
          }
        }
        outSizes = out;
      }
      const sizes2 = outSizes;

      tracks[node.id] = { kind: node.kind, gap, sizes: sizes2.slice() };

      let pos = axis === "w" ? box.x : box.y;
      kids.forEach((k, i) => {
        const childBox =
          axis === "w"
            ? { x: pos, y: box.y, w: sizes2[i], h: box.h }
            : { x: box.x, y: pos, w: box.w, h: sizes2[i] };
        place(k, childBox);
        pos += sizes2[i] + gap;
      });
    }

    const width = Math.max(0, Number(size.width) || 0);
    const height = Math.max(0, Number(size.height) || 0);
    place(tree, { x: 0, y: 0, w: width, h: height });

    const minSize = {
      width: subtreeMin(tree, "w", visible, regions),
      height: subtreeMin(tree, "h", visible, regions),
    };
    return {
      rects,
      tracks,
      collapsed,
      minSize,
      // 容器比最小值还小时各超出多少（>0 = 需要整页滚动兜底）
      overflow: {
        x: Math.max(0, minSize.width - width),
        y: Math.max(0, minSize.height - height),
      },
    };
  }

  /** 可见性集合规整：坏值/缺值一律用 defaultVisible；不可关的区域强制可见。 */
  function normalizeVisible(raw, regions) {
    const src = raw && typeof raw === "object" && !Array.isArray(raw) ? raw : {};
    const out = {};
    for (const id of Object.keys(regions)) {
      const r = regions[id];
      const v = src[id];
      out[id] = typeof v === "boolean" ? v : r.defaultVisible;
      if (!r.closable) out[id] = true;
    }
    return out;
  }

  /** 比例规整：坏值回退 1，越界夹到 [RATIO_MIN, RATIO_MAX]。 */
  function normalizeRatios(raw, regions) {
    const src = raw && typeof raw === "object" && !Array.isArray(raw) ? raw : {};
    const out = {};
    for (const id of Object.keys(regions)) {
      const n = Number(src[id]);
      out[id] = Number.isFinite(n) ? Math.min(RATIO_MAX, Math.max(RATIO_MIN, n)) : 1;
    }
    return out;
  }

  function defaultState(regions) {
    const regs = regions || REGIONS;
    const visible = {};
    const ratios = {};
    for (const id of Object.keys(regs)) {
      visible[id] = regs[id].defaultVisible;
      ratios[id] = 1;
    }
    // `sizes` 是拖拽钉住的像素尺寸（空 = 走权重分配，也就是出厂布局）
    return { version: 1, visible, ratios, sizes: {} };
  }

  /**
   * 持久化状态规整：输入可以是 `null` / 坏 JSON 字符串 / 乱结构 / 越界值，
   * **一律回退到合法状态且不抛异常**（localStorage 里的东西不可信）。
   */
  function normalizeState(raw, regions) {
    const regs = regions || REGIONS;
    let obj = raw;
    if (typeof raw === "string") {
      try {
        obj = JSON.parse(raw);
      } catch (e) {
        obj = null; // 坏 JSON → 全默认
      }
    }
    if (!obj || typeof obj !== "object" || Array.isArray(obj)) obj = {};
    return {
      version: 1,
      visible: normalizeVisible(obj.visible, regs),
      ratios: normalizeRatios(obj.ratios, regs),
      sizes: normalizeSizes(obj.sizes),
    };
  }

  /** 持久化序列化（与 normalizeState 往返一致）。 */
  function serializeState(state, regions) {
    const s = normalizeState(state, regions);
    return JSON.stringify(s);
  }

  return {
    RATIO_MIN,
    RATIO_MAX,
    REGIONS,
    REGION_ORDER,
    TREE,
    solve,
    resizePair,
    canHide,
    findNode,
    visibleChildren,
    axisOf,
    subtreeMin,
    subtreeWeight,
    subtreeVisible,
    normalizeVisible,
    normalizeRatios,
    normalizeSizes,
    defaultState,
    normalizeState,
    serializeState,
  };
});
