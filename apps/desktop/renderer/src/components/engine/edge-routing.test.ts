import { expect, it } from "vitest";
import type { EngineNode } from "@/bridge";
import { buildFlowGraph, START_ID, TERMINAL_ID } from "./build-graph";

/**
 * Guard on EDGE ROUTING (N38 review). A smooth-step edge runs down, sideways, then down again; React
 * Flow puts that sideways run half-way between the two nodes, which laid it straight across unrelated
 * NODES — visually "the line goes through the box". A fixed offset only moved the collision, so
 * `build-graph` computes a CLEAR LANE per edge from the real post-layout geometry. This asserts the
 * property that matters, over the WHOLE graph rather than the one case that was reported: no edge's
 * sideways run may cross a node that is neither its source nor its target.
 *
 * Verified to FAIL without the lane computation (`grillskipped->finalq` crossed `judge`).
 */
function mk(id:string,p:string|null,o:number,t:string,l:string,c:Record<string,unknown>={}):EngineNode{
  return {id,workflow_id:"wf",parent_id:p,order_index:o,type_:t,label:l,config:JSON.stringify(c),created_at:0,updated_at:0};
}
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
  mk("grillsel","root",2,"selector","grill?",{branches:[{cond:{var:"entry_choice",eq:"ready"},child:0},{cond:{always:true},child:1}]}),
  mk("grillskipped","grillsel",0,"note","grill skipped"),
  mk("grill","grillsel",1,"loop","grill",{until:{var:"g",eq:"complete"}}),
  mk("spawnspec","grill",0,"spawn_subagent","author spec",{agent_id:"spec"}),
  mk("steer","grill",1,"ask_human","steer"),
  mk("judge","grill",2,"judge","spec complete?"),
  mk("finalq","root",3,"query","final status"),
];}

it("no edge's horizontal run crosses an unrelated NODE", async () => {
  const g = await buildFlowGraph(nspec(), "root");
  const byId = new Map(g.nodes.map(n=>[n.id,n]));
  const abs = (id:string) => {
    const n = byId.get(id); if(!n) return null;
    let x=n.position.x,y=n.position.y,p=n.parentId;
    while(p){const pn=byId.get(p); if(!pn)break; x+=pn.position.x; y+=pn.position.y; p=pn.parentId;}
    const st = n.style as {width?:number;height?:number}|undefined;
    const w = st?.width ?? (n.type==="enginePill"?210:254);
    const h = st?.height ?? (n.type==="enginePill"?42:70);
    return {x,y,w,h,type:n.type};
  };
  const leaves = g.nodes.filter(n=>n.type==="engineFlow").map(n=>({id:n.id, ...abs(n.id)!}));
  const bad:string[]=[];
  for (const e of g.edges) {
    if (e.source===START_ID||e.target===TERMINAL_ID) continue;
    const a = abs(e.source), b = abs(e.target);
    if(!a||!b) continue;
    const lane = (e.data as {laneY?:number}|undefined)?.laneY ?? (a.y + a.h + b.y)/2;
    const x1=Math.min(a.x,b.x), x2=Math.max(a.x+a.w,b.x+b.w);
    for (const r of leaves) {
      if (r.id===e.source||r.id===e.target) continue;
      if (lane > r.y && lane < r.y+r.h && x1 < r.x+r.w && r.x < x2)
        bad.push(`${e.source}->${e.target} lane y=${Math.round(lane)} crosses ${r.id}`);
    }
  }
  expect(bad, `EDGES CROSSING NODES:\n${bad.join("\n")}`).toEqual([]);
});
