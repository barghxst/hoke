import { createKV } from "stowtree/kv";

const kv = createKV<number>();
let observed: number | undefined;
const unsubscribe = kv.subscribe("count", () => {
  observed = kv.get("count");
});
kv.set("count", 1);
if (observed !== 1) throw new Error("Package import or subscription failed");
unsubscribe();
console.log("stowtree/kv: package import and subscription OK");
