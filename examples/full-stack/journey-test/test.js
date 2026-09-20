import { createClient } from "redis";

function fail(message) {
  console.error(`FAIL: ${message}`);
  process.exit(1);
}

const response = await fetch("http://app:8080/hello");
if (response.status !== 200) {
  fail(`expected HTTP 200 from app, got ${response.status}`);
}

const body = await response.json();
if (body.message !== "Hello from Ratect!") {
  fail(`unexpected message: ${JSON.stringify(body)}`);
}
if (!(body.total_visits > 1000000)) {
  fail(`expected total_visits above the seeded 1,000,000, got ${body.total_visits}`);
}
console.log(`app responded: ${JSON.stringify(body)}`);

// The app itself already read (and populated) this key — checking it here
// directly is the point: two paths in this task's own dependency graph
// (app, and this container) both need `cache`, so it must be the same
// container either way, not started twice.
const redis = createClient({ url: process.env.REDIS_URL });
await redis.connect();
const cached = await redis.get("visits:count");
if (cached === null) {
  fail("expected app's own request to have populated the visits:count cache key");
}
console.log(`cache holds visits:count=${cached}, confirming app and this container share the same cache`);
await redis.quit();

console.log("journey test passed");
