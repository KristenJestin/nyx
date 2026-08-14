import { describe, expect, it } from "vitest";
import type { EngineNode } from "@/bridge";
import { buildFlowGraph, type EngineRFNode } from "./build-graph";

/**
 * Geometric guards on the ELK compound layout (the N34/N38 review). A screenshot shows clutter but
 * cannot say WHY: these assert the layout's actual numbers — sibling boxes disjoint, every child box
 * and every leaf inside its container — so a real containment regression fails loudly instead of being
 * argued about over a PNG. (When the render looks cluttered but these pass, the cause is edge ROUTING,
 * not box placement.)
 */
function mk(id:string,p:string|null,o:number,t:string,l:string,c:Record<string,unknown>={}):EngineNode{
  return {id,workflow_id:"wf",parent_id:p,order_index:o,type_:t,label:l,config:JSON.stringify(c),created_at:0,updated_at:0};
}
/** The real nspec shape (nested sequences under a selector + a loop) — where boxes overlapped. */
function nspec():EngineNode[]{return[
  mk("root",null,0,"sequence","/nspec"),
  mk("status","root",0,"query","spec status"),
  mk("entry","root",1,"selector","entry gate",{branches:[{cond:{var:"s",eq:"ready"},child:0},{cond:{var:"s",eq:"draft"},child:1},{cond:{always:true},child:2}]}),
  mk("revise","entry",0,"sequence","revise a ready spec"),
  mk("confirm","revise",0,"ask_human","confirm revision"),
  mk("fork","revise",1,"fork_spec","fork"),
  mk("resume","entry",1,"sequence","resume a draft"),
  mk("canready","resume",0,"query","can finalize?"),
  mk("fog","resume",1,"selector","finalize or grill"),
  mk("cont","fog",0,"ask_human","continue or finalize"),
  mk("incomplete","fog",1,"note","draft incomplete"),
  mk("createnew","entry",2,"note","create new"),
  mk("grill","root",2,"loop","grill",{until:{var:"g",eq:"complete"}}),
  mk("spawnspec","grill",0,"spawn_subagent","author spec",{agent_id:"spec"}),
  mk("steer","grill",1,"ask_human","steer"),
  mk("judge","grill",2,"judge","spec complete?"),
  mk("finalq","root",3,"query","final status"),
];}

interface Box { id:string; x:number; y:number; w:number; h:number; parent:string|null }

function absBoxes(nodes: EngineRFNode[]): Box[] {
  const byId = new Map(nodes.map(n=>[n.id,n]));
  const abs = (n: EngineRFNode): {x:number;y:number} => {
    let x = n.position.x, y = n.position.y, p = n.parentId;
    while (p) { const pn = byId.get(p); if (!pn) break; x += pn.position.x; y += pn.position.y; p = pn.parentId; }
    return {x,y};
  };
  return nodes.filter(n=>n.type==="engineGroup").map(n=>{
    const {x,y} = abs(n);
    const st = n.style as {width?:number;height?:number}|undefined;
    return { id:n.id, x, y, w: st?.width ?? 0, h: st?.height ?? 0, parent: n.parentId ?? null };
  });
}
const overlaps = (a:Box,b:Box) =>
  a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;

describe("ELK layout geometry", () => {
it("a child box must be CONTAINED in its parent box", async () => {
  const g = await buildFlowGraph(nspec(), "root");
  const boxes = absBoxes(g.nodes);
  const byId = new Map(boxes.map(b=>[b.id,b]));
  const bad: string[] = [];
  for (const b of boxes) {
    if (!b.parent) continue;
    const p = byId.get(b.parent);
    if (!p) continue;
    const inside = b.x >= p.x - 0.5 && b.y >= p.y - 0.5 && b.x + b.w <= p.x + p.w + 0.5 && b.y + b.h <= p.y + p.h + 0.5;
    if (!inside) bad.push(`${b.id}[${Math.round(b.x)},${Math.round(b.y)} ${Math.round(b.w)}x${Math.round(b.h)}] escapes ${p.id}[${Math.round(p.x)},${Math.round(p.y)} ${Math.round(p.w)}x${Math.round(p.h)}]`);
  }
  expect(bad, `CHILD BOX ESCAPES ITS PARENT:\n${bad.join("\n")}`).toEqual([]);
});

it("LEAF nodes must be contained in their group box", async () => {
  const g = await buildFlowGraph(nspec(), "root");
  const boxes = absBoxes(g.nodes);
  const byId = new Map(boxes.map(b=>[b.id,b]));
  const rf = new Map(g.nodes.map(n=>[n.id,n]));
  const bad: string[] = [];
  for (const n of g.nodes) {
    if (n.type !== "engineFlow" || !n.parentId) continue;
    const p = byId.get(n.parentId); if (!p) continue;
    // absolute leaf position
    let x = n.position.x, y = n.position.y, pid: string | null = n.parentId;
    while (pid) { const pn = rf.get(pid); if (!pn) break; x += pn.position.x; y += pn.position.y; pid = pn.parentId ?? null; }
    const w = 254, h = 70;
    const inside = x >= p.x - 0.5 && y >= p.y - 0.5 && x + w <= p.x + p.w + 0.5 && y + h <= p.y + p.h + 0.5;
    if (!inside) bad.push(`leaf ${n.id}[${Math.round(x)},${Math.round(y)}] escapes ${p.id}[${Math.round(p.x)},${Math.round(p.y)} ${Math.round(p.w)}x${Math.round(p.h)}]`);
  }
  expect(bad, `LEAF ESCAPES ITS BOX:\n${bad.join("\n")}`).toEqual([]);
});

it("sibling group boxes must NOT overlap", async () => {
  const g = await buildFlowGraph(nspec(), "root");
  const boxes = absBoxes(g.nodes);
  const bad: string[] = [];
  for (let i=0;i<boxes.length;i++) for (let j=i+1;j<boxes.length;j++) {
    const a=boxes[i]!,b=boxes[j]!;
    // Only SIBLINGS (same parent) must be disjoint; a child inside its parent is expected to overlap it.
    if (a.parent === b.parent && overlaps(a,b)) {
      bad.push(`${a.id}[${Math.round(a.x)},${Math.round(a.y)} ${Math.round(a.w)}x${Math.round(a.h)}] ∩ ${b.id}[${Math.round(b.x)},${Math.round(b.y)} ${Math.round(b.w)}x${Math.round(b.h)}]`);
    }
  }
  expect(bad, `OVERLAPPING SIBLING BOXES:\n${bad.join("\n")}`).toEqual([]);
});
});
