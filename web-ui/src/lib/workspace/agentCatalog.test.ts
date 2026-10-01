import { describe, expect, it } from "vitest";
import { normalizeAgentCatalog } from "./launcher";

describe("extensible agent catalog", () => {
  it("keeps extension identities and only their declared capabilities", () => {
    const [agent] = normalizeAgentCatalog([{id:"example/pi", name:"Pi", installed:true}]);
    expect(agent.id).toBe("example/pi");
    expect(agent.name).toBe("Pi");
    expect(agent.chatCapable).toBe(false);
    expect(agent.forkCapable).toBe(false);
    const [ready] = normalizeAgentCatalog([{id:"example/pi", chat_capable:true, fork_capable:true}]);
    expect(ready.chatCapable).toBe(true);
    expect(ready.forkCapable).toBe(true);
  });
  it("drops duplicate identities and malformed provider model rows", () => {
    const rows = normalizeAgentCatalog([null, {id:""}, {id:"agy", models:[null, {id:"a", label:"A"}, {id:"a", label:"Duplicate"}]}, {id:"agy"}]);
    expect(rows).toHaveLength(1);
    expect(rows[0].models).toEqual([{id:"a",label:"A"}]);
    expect(normalizeAgentCatalog(Array.from({length:100},(_,i)=>({id:`plugin/${i}`})))).toHaveLength(64);
  });
});
