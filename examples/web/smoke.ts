import { KV } from "hoke/kv/web";

const kv = new KV<number>({ name: "smoke" });
let observed: number | undefined;
const unsubscribe = kv.subscribe("count", () => {
  observed = kv.get("count");
});
kv.set("count", 1);
if (observed !== 1) throw new Error("Package import or subscription failed");
unsubscribe();
console.log("hoke/kv/web: package import and subscription OK");
