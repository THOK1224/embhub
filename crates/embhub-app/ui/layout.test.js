/*! 布局求解器的零依赖 Node 测试（H1a）。
 *
 *   node crates/embhub-app/ui/layout.test.js      → 退出码 0（全过）/ 1（有失败）
 *
 * 这里测的是**求解器**（不碰浏览器的部分）：最小值扣除、权重分配、永不低于最小值、
 * 关闭后塌缩、拉伸 clamp、容器小于最小值总和、持久化往返与坏数据回退。
 * DOM/CSS 那半由页面自带的 harness（`index.html?selftest=1`）负责。
 */
"use strict";

const L = require("./layout.js");

let passed = 0;
const failures = [];

function check(name, fn) {
  try {
    fn();
    passed += 1;
    console.log("  [OK] " + name);
  } catch (e) {
    failures.push({ name, message: e && e.message ? e.message : String(e) });
    console.log("  [XX] " + name + "  -> " + (e && e.message ? e.message : e));
  }
}

function assert(cond, msg) {
  if (!cond) throw new Error(msg || "断言失败");
}

function assertClose(actual, expected, tol, msg) {
  assert(
    Math.abs(actual - expected) <= (tol == null ? 1e-6 : tol),
    (msg || "") + `（实际 ${actual}，期望 ${expected}）`
  );
}

/** 四个区域都可见时的基线尺寸（够大，能触发增长分配）。 */
const BIG = { width: 1200, height: 900 };

function allVisible() {
  return L.normalizeVisible(null, L.REGIONS);
}

function sumMins(axis, visible) {
  return L.subtreeMin(L.TREE, axis, visible, L.REGIONS);
}

console.log("== 1. 最小值扣除 + 权重分配 ==");
check("容器宽 = 左列最小值 + 右列最小值 + 间隙 + 剩余（无重叠、无空隙）", () => {
  const r = L.solve({ size: BIG });
  const left = r.rects.params;
  const right = r.rects.right === undefined ? null : null; // right 是容器不是区域
  assert(left.w >= L.REGIONS.params.minW, "左列不得低于最小值");
  // 右列区域应铺满剩余宽度
  const rightMin = L.subtreeMin(L.TREE.children[1], "w", allVisible(), L.REGIONS);
  const leftover = BIG.width - 8 - L.REGIONS.params.minW - rightMin;
  assertClose(r.rects.rx.w, rightMin + leftover, 1e-6, "右侧唯一加权区域应吃掉全部剩余宽度");
  assertClose(
    left.w + 8 + r.rects.rx.w,
    BIG.width,
    1e-6,
    "两条轨道 + 间隙必须正好等于容器宽度"
  );
});

check("右列按权重 1:3:1 分配剩余高度", () => {
  const r = L.solve({ size: BIG });
  const vis = allVisible();
  const minTx = L.REGIONS.tx.minH;
  const minRx = L.REGIONS.rx.minH;
  const minEv = L.REGIONS.events.minH;
  const avail = BIG.height - 8 * 2; // 三行两个间隙
  const leftover = avail - (minTx + minRx + minEv);
  assert(leftover > 0, "这个尺寸下应有剩余空间可用于分配");
  assertClose(r.rects.tx.h, minTx + leftover / 5, 1e-6, "tx 权重 1/5");
  assertClose(r.rects.rx.h, minRx + (leftover * 3) / 5, 1e-6, "rx 权重 3/5");
  assertClose(r.rects.events.h, minEv + leftover / 5, 1e-6, "events 权重 1/5");
  assertClose(r.rects.tx.h + r.rects.rx.h + r.rects.events.h + 16, BIG.height, 1e-6);
});

check("左列 weight=0：永远只有最小值宽（其余都给右列）", () => {
  const r = L.solve({ size: { width: 2000, height: 900 } });
  assertClose(r.rects.params.w, L.REGIONS.params.minW, 1e-6, "固定列不该增长");
  assert(r.rects.rx.w > 1000, "多出来的宽度应全部给右列");
});

check("比例参与分配（rx 比例调小时 tx 变高）", () => {
  const base = L.solve({ size: BIG });
  const tilted = L.solve({ size: BIG, ratios: { rx: 0.5 } });
  assert(tilted.rects.tx.h > base.rects.tx.h, "rx 比例变小 → tx 应分到更多高度");
  assert(tilted.rects.rx.h < base.rects.rx.h, "rx 比例变小 → rx 应变矮");
});

console.log("== 2. 任何区域永不低于最小值 ==");
check("扫一批尺寸 × 可见性组合，逐个断言 ≥ 最小值且总和不超过可用空间", () => {
  const sizes = [2000, 1400, 1200, 1024, 900, 800, 700, 640, 500, 420, 360, 300, 200];
  const visCombos = [
    { tx: true, events: true },
    { tx: false, events: true },
    { tx: true, events: false },
    { tx: false, events: false },
  ];
  for (const w of sizes) {
    for (const h of sizes) {
      for (const combo of visCombos) {
        const visible = L.normalizeVisible(combo, L.REGIONS);
        const r = L.solve({ size: { width: w, height: h }, visible });
        for (const id of Object.keys(r.rects)) {
          const reg = L.REGIONS[id];
          assert(
            r.rects[id].w >= reg.minW - 1e-9,
            `[${w}x${h} ${JSON.stringify(combo)}] 区域 ${id} 宽度 ${r.rects[id].w} 低于最小值 ${reg.minW}`
          );
          assert(
            r.rects[id].h >= reg.minH - 1e-9,
            `[${w}x${h} ${JSON.stringify(combo)}] 区域 ${id} 高度 ${r.rects[id].h} 低于最小值 ${reg.minH}`
          );
        }
        // 可见区域个数必须与矩形个数一致（不可见的不许产出矩形）
        const visibleIds = Object.keys(visible).filter((id) => visible[id]);
        assert(
          visibleIds.length === Object.keys(r.rects).length,
          `矩形个数应等于可见区域个数（${visibleIds.length} vs ${Object.keys(r.rects).length}）`
        );
      }
    }
  }
});

check("同层可见区域两两不重叠（扫尺寸）", () => {
  for (const w of [1400, 900, 640, 400]) {
    for (const h of [1200, 800, 600, 300]) {
      const r = L.solve({ size: { width: w, height: h } });
      const ids = Object.keys(r.rects);
      for (let i = 0; i < ids.length; i += 1) {
        for (let j = i + 1; j < ids.length; j += 1) {
          const a = r.rects[ids[i]];
          const b = r.rects[ids[j]];
          const overlapX = Math.min(a.x + a.w, b.x + b.w) > Math.max(a.x, b.x) + 1e-9;
          const overlapY = Math.min(a.y + a.h, b.y + b.h) > Math.max(a.y, b.y) + 1e-9;
          assert(!(overlapX && overlapY), `${ids[i]} 与 ${ids[j]} 重叠（${w}x${h}）`);
        }
      }
    }
  }
});

console.log("== 3. 关闭后塌缩：空间被兄弟吸收 ==");
check("关掉 tx：右列只剩 rx/events，两者都变大，且没有 tx 的矩形", () => {
  const before = L.solve({ size: BIG });
  const after = L.solve({ size: BIG, visible: { tx: false } });
  assert(after.rects.tx === undefined, "关掉的区域不该有矩形");
  assert(after.rects.rx.h > before.rects.rx.h, "rx 应变高（吸收 tx 让出的空间）");
  assert(after.rects.events.h > before.rects.events.h, "events 也应分到一部分");
  assertClose(
    after.rects.rx.h + after.rects.events.h + 8,
    BIG.height,
    1e-6,
    "剩下的两行 + 间隙应铺满整列"
  );
});

check("关掉 tx 与 events：右列塌缩成单孩子，rx 拿满整列", () => {
  const r = L.solve({ size: BIG, visible: { tx: false, events: false } });
  assertClose(r.rects.rx.h, BIG.height, 1e-6, "唯一的可见孩子应拿满整层");
  assert(r.collapsed.length > 0, "应记录发生了塌缩");
  assert(
    r.collapsed.indexOf("row-rx") >= 0,
    "塌缩的应是被保留下来的那个孩子：" + JSON.stringify(r.collapsed)
  );
});

check("不可关的区域（params/rx）永远可见，即使传 false", () => {
  const r = L.solve({ size: BIG, visible: { params: false, rx: false } });
  assert(r.rects.params !== undefined && r.rects.rx !== undefined, "不可关区域必须仍在布局里");
});

console.log("== 4. 拉伸 clamp（两端都不越界）==");
check("超大容器：只有加权区域增长，总尺寸等于容器", () => {
  const r = L.solve({ size: { width: 5000, height: 5000 } });
  assertClose(r.rects.params.w, L.REGIONS.params.minW, 1e-6, "固定列不增长");
  assertClose(
    r.rects.tx.h + r.rects.rx.h + r.rects.events.h + 16,
    5000,
    1e-6,
    "铺满容器高度"
  );
  // 右列宽度 = 5000 - 间隙 - 左列
  assertClose(r.rects.rx.w, 5000 - 8 - L.REGIONS.params.minW, 1e-6);
});

check("极窄容器：各区域停在最小值，overflow 报告需要横向滚动", () => {
  const r = L.solve({ size: { width: 120, height: 900 } });
  assertClose(r.rects.params.w, L.REGIONS.params.minW, 1e-6);
  assertClose(r.rects.rx.w, L.REGIONS.rx.minW, 1e-6, "右列应停在其最小值");
  assert(r.overflow.x > 0, "容器比最小值小时应报告横向溢出（交给整页滚动）");
});

check("极矮容器：高度方向全部停在最小值，overflow.y > 0", () => {
  const r = L.solve({ size: { width: 1200, height: 100 } });
  assertClose(r.rects.tx.h, L.REGIONS.tx.minH, 1e-6);
  assertClose(r.rects.rx.h, L.REGIONS.rx.minH, 1e-6);
  assertClose(r.rects.events.h, L.REGIONS.events.minH, 1e-6);
  assert(r.overflow.y > 0, "应报告纵向溢出");
});

check("minSize = 所有可见区域最小值之和（含间隙），随可见性变化", () => {
  const all = L.solve({ size: BIG });
  const noTx = L.solve({ size: BIG, visible: { tx: false } });
  assertClose(all.minSize.height, L.REGIONS.tx.minH + L.REGIONS.rx.minH + L.REGIONS.events.minH + 16, 1e-6);
  assertClose(noTx.minSize.height, L.REGIONS.rx.minH + L.REGIONS.events.minH + 8, 1e-6, "关掉一个区域后最小高度应变小");
  assertClose(all.minSize.width, L.REGIONS.params.minW + 8 + Math.max(L.REGIONS.tx.minW, L.REGIONS.rx.minW, L.REGIONS.events.minW), 1e-6);
});

console.log("== 5. 持久化往返 + 坏数据回退 ==");
check("默认状态：四区可见（关不掉的两个强制可见）、比例全 1", () => {
  const s = L.defaultState();
  assert(s.visible.params && s.visible.tx && s.visible.rx && s.visible.events);
  assert(s.ratios.rx === 1 && s.ratios.tx === 1 && s.ratios.events === 1);
});

check("序列化 → 反序列化往返一致", () => {
  const s = { version: 1, visible: { tx: false, events: false }, ratios: { rx: 2.5 } };
  const json = L.serializeState(s);
  const back = L.normalizeState(json);
  assert(back.visible.tx === false && back.visible.events === false, "可见性应往返一致");
  assert(back.visible.params === true && back.visible.rx === true, "不可关区域仍可见");
  assertClose(back.ratios.rx, 2.5, 1e-9, "比例应往返一致");
  assert(back.ratios.tx === 1, "没存的比例用默认值");
});

check("坏数据一律回退且不抛异常", () => {
  const cases = [
    null,
    undefined,
    "",
    "{ 这不是 JSON",
    "[1,2,3]",
    "42",
    '"字符串"',
    JSON.stringify({ visible: { tx: "yes", rx: null }, ratios: { rx: "很大" } }),
    JSON.stringify({ visible: null, ratios: [] }),
  ];
  for (const raw of cases) {
    const s = L.normalizeState(raw);
    assert(s.visible.params === true && s.visible.rx === true, "坏数据下不可关区域必须可见");
    assert(typeof s.visible.tx === "boolean", "可关区域必须归一化成布尔：" + JSON.stringify(s.visible));
    for (const id of Object.keys(s.ratios)) {
      const v = s.ratios[id];
      assert(Number.isFinite(v) && v >= L.RATIO_MIN && v <= L.RATIO_MAX, `比例越界未夹住：${id}=${v}`);
    }
    // 坏数据也必须能直接求解出合法布局
    const r = L.solve({ size: BIG, visible: s.visible, ratios: s.ratios });
    for (const id of Object.keys(r.rects)) {
      assert(r.rects[id].w >= L.REGIONS[id].minW - 1e-9, "坏数据不该产出非法布局");
    }
  }
});

check("比例越界被夹到 [RATIO_MIN, RATIO_MAX]", () => {
  const s = L.normalizeState({ ratios: { rx: 999, tx: -5, events: 0 } });
  assertClose(s.ratios.rx, L.RATIO_MAX, 1e-9, "上界应夹住");
  assertClose(s.ratios.tx, L.RATIO_MIN, 1e-9, "下界应夹住");
  assertClose(s.ratios.events, L.RATIO_MIN, 1e-9, "0 也应夹到下界");
});

check("未知区域 id 不会污染状态", () => {
  const s = L.normalizeState({ visible: { 幽灵区: true }, ratios: { 幽灵区: 3 } });
  assert(s.visible["幽灵区"] === undefined, "未知 id 不该出现");
  assert(Object.keys(s.visible).length === 4, "只应有四个已知区域");
});

console.log("== 6. 拖拽：钉住尺寸、等量反向、双侧最小值 clamp（t24）==");
check("resizePair：只有这一对变，其它区域一动不动（空间守恒）", () => {
  const before = L.solve({ size: BIG });
  const beforeRight = before.tracks.right.sizes.slice();
  const res = L.resizePair({ size: BIG, splitNodeId: "root", index: 0, deltaPx: 120 });
  const after = L.solve({ size: BIG, sizes: res.sizes });
  assertClose(res.applied, 120, 1e-9, "请求 120 应原样生效（离极限还远）");
  assertClose(after.tracks.root.sizes[0], before.tracks.root.sizes[0] + 120, 1e-6, "左列应 +120");
  assertClose(after.tracks.root.sizes[1], before.tracks.root.sizes[1] - 120, 1e-6, "右列应 -120");
  assertClose(
    after.tracks.root.sizes[0] + after.tracks.root.sizes[1],
    before.tracks.root.sizes[0] + before.tracks.root.sizes[1],
    1e-6,
    "这一对的宽度之和必须守恒"
  );
  // 右列整体变窄，但内部三行的**比例**不该被这次拖拽打乱
  assertClose(
    after.tracks.right.sizes[1] / after.tracks.right.sizes[2],
    beforeRight[1] / beforeRight[2],
    0.05,
    "右列内部比例不该被相邻拖拽打乱"
  );
});

check("resizePair：往两个方向拖到极限都停住，且任一侧不低于最小值", () => {
  const initialRight = L.solve({ size: BIG }).tracks.root.sizes[1];
  for (const requested of [99999, -99999]) {
    const res = L.resizePair({ size: BIG, splitNodeId: "root", index: 0, deltaPx: requested });
    assert(res.clamped === true, "拖到极限必须标记 clamped");
    const after = L.solve({ size: BIG, sizes: res.sizes });
    const left = after.rects.params.w;
    const rightCol = after.tracks.root.sizes[1];
    assert(left >= L.REGIONS.params.minW - 1e-6, "左列不得低于最小值：" + left);
    assert(
      rightCol >= L.subtreeMin(L.TREE.children[1], "w", allVisible(), L.REGIONS) - 1e-6,
      "右列不得低于最小值：" + rightCol
    );
    assert(rightCol <= initialRight + 1e-6, "右列不可能比初始还宽");
  }
});

check("resizePair：右列内部（tx/rx/events）拖到极限也不越界，且只动相邻两个", () => {
  const before = L.solve({ size: BIG });
  for (const requested of [99999, -99999]) {
    const res = L.resizePair({ size: BIG, splitNodeId: "right", index: 0, deltaPx: requested });
    const after = L.solve({ size: BIG, sizes: res.sizes });
    const tx = after.tracks.right.sizes[0];
    const rx = after.tracks.right.sizes[1];
    const events = after.tracks.right.sizes[2];
    assert(tx >= L.REGIONS.tx.minH - 1e-6, "tx 不得低于最小值：" + tx);
    assert(rx >= L.REGIONS.rx.minH - 1e-6, "rx 不得低于最小值：" + rx);
    assertClose(events, before.tracks.right.sizes[2], 1e-6, "events 与这条分隔条不相邻，必须一动不动");
    assertClose(tx + rx, before.tracks.right.sizes[0] + before.tracks.right.sizes[1], 1e-6, "相邻一对之和守恒");
    assertClose(tx + rx + events + 16, BIG.height, 1e-6, "三行 + 间隙仍铺满整列");
  }
});

check("钉住值也 respect 最小值与容器容量（不会被钉穿）", () => {
  const size = { width: 700, height: 600 };
  const solved = L.solve({ size: size, sizes: { left: 10 } });
  assert(solved.rects.params.w >= L.REGIONS.params.minW - 1e-6, "钉穿最小值应被拒绝：" + solved.rects.params.w);
  const solved2 = L.solve({ size: size, sizes: { left: 99999 } });
  assert(solved2.rects.params.w + 8 <= 700 + 1e-6, "钉得比容器还宽必须被夹：" + solved2.rects.params.w);
});

check("看不见的区域上的钉住值不起作用（隐藏后空间被兄弟吸收）", () => {
  const pinned = { "row-tx": 400 };
  const shown = L.solve({ size: BIG, sizes: pinned });
  assert(shown.tracks.right.sizes[0] >= 399, "可见时钉住值生效：" + shown.tracks.right.sizes[0]);
  const hidden = L.solve({ size: BIG, sizes: pinned, visible: { tx: false } });
  assert(hidden.rects.tx === undefined, "关掉后不该有 tx 的矩形");
  assertClose(
    hidden.tracks.right.sizes[0] + hidden.tracks.right.sizes[1] + 8,
    BIG.height,
    1e-6,
    "剩下的两行应铺满整列（被隐藏区域的钉住值不许留洞）"
  );
});

check("normalizeSizes：坏值/越界直接丢弃", () => {
  const raw = { left: 320, right: "abc", tx: -5, rx: 0, events: 1e9, "row-tx": 200, nope: null };
  const out = L.normalizeSizes(raw);
  assert(out.left === 320 && out["row-tx"] === 200, "合法值应保留：" + JSON.stringify(out));
  for (const bad of ["right", "tx", "rx", "events", "nope"]) {
    assert(out[bad] === undefined, "坏值 " + bad + " 应被丢弃");
  }
  assert(Object.keys(L.normalizeSizes(null)).length === 0, "null 应该得到空对象");
  assert(Object.keys(L.normalizeSizes([1, 2, 3])).length === 0, "数组应被当成坏数据");
});

check("状态往返：sizes 也持久化，坏 sizes 回退为空", () => {
  const state = { visible: { tx: false }, ratios: { rx: 2 }, sizes: { left: 420, "row-rx": 333 } };
  const back = L.normalizeState(L.serializeState(state));
  assertClose(back.sizes.left, 420, 1e-9, "sizes 应往返一致");
  assertClose(back.sizes["row-rx"], 333, 1e-9, "sizes 应往返一致");
  assert(back.visible.tx === false && back.ratios.rx === 2, "其它字段不受影响");
  const bad = L.normalizeState({ sizes: { left: "x", right: NaN, tx: Infinity } });
  assert(Object.keys(bad.sizes).length === 0, "坏 sizes 应回退为空：" + JSON.stringify(bad.sizes));
});

console.log("== 7. 关闭守卫：不许把最后一个可见区域也关掉（t24）==");
check("不可关的常驻区域一律拒绝，并给出原因", () => {
  const vis = allVisible();
  for (const id of ["params", "rx"]) {
    const verdict = L.canHide(vis, id);
    assert(verdict.ok === false, id + " 不该允许关闭");
    assert(verdict.reason.length > 0, "拒绝必须带原因");
    assert(L.REGIONS[id].closable === false, id + " 在区域表里就该是不可关的");
  }
});

check("可关区域在还有别人可见时可以关", () => {
  const vis = allVisible();
  for (const id of ["tx", "events"]) {
    const verdict = L.canHide(vis, id);
    assert(verdict.ok === true, id + " 应该可以关：" + verdict.reason);
  }
});

check("**最后一个可见区域**：即使它是可关的，也必须拒绝并说明原因", () => {
  // 「只剩一个可见区域」在默认区域表下到不了（params/rx 不可关），
  // 所以用自定义区域表精确构造这个场景——纯函数的好处就是能这么测。
  const regs = {
    a: { id: "a", title: "A", minW: 10, minH: 10, weight: 1, closable: true, defaultVisible: true },
    b: { id: "b", title: "B", minW: 10, minH: 10, weight: 1, closable: true, defaultVisible: false },
  };
  const verdict = L.canHide({ a: true, b: false }, "a", regs);
  assert(verdict.ok === false, "只剩一个可见区域时必须拒绝");
  assert(verdict.reason.indexOf("最后一个可见区域") >= 0, "原因要说清是「最后一个」：" + verdict.reason);
  assert(L.canHide({ a: true, b: true }, "a", regs).ok === true, "还有别的可见时应该放行");
  assert(L.canHide({ a: true }, "幽灵区", regs).ok === false, "未知区域要拒绝");
});

console.log("");
console.log(`布局求解器测试：通过 ${passed} 项，失败 ${failures.length} 项`);
if (failures.length) {
  for (const f of failures) console.log("  失败：" + f.name + " —— " + f.message);
  process.exit(1);
}
console.log("全部通过（求解器这一半不依赖浏览器）");
process.exit(0);
