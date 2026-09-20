import http from "node:http";
import pg from "pg";
import { createClient } from "redis";
import { formatVisitResponse } from "./lib.js";

const pool = new pg.Pool({ connectionString: process.env.DATABASE_URL });
// Both pg.Pool and node-redis emit 'error' on a backend/connection problem
// (an idle pool client losing its connection, Redis briefly dropping) —
// with no listener, that's an unhandled EventEmitter error, which crashes
// the whole process instead of just failing whichever request hit it.
pool.on("error", (err) => {
  console.error("Postgres pool error:", err);
});
const redis = createClient({ url: process.env.REDIS_URL });
redis.on("error", (err) => {
  console.error("Redis client error:", err);
});
await redis.connect();

const COUNT_CACHE_KEY = "visits:count";
const COUNT_CACHE_TTL_SECONDS = 5;

async function totalVisits() {
  const cached = await redis.get(COUNT_CACHE_KEY);
  if (cached !== null) {
    return { total: Number(cached), source: "cache" };
  }
  const { rows } = await pool.query("SELECT count(*) FROM visits");
  const total = Number(rows[0].count);
  await redis.set(COUNT_CACHE_KEY, total, { EX: COUNT_CACHE_TTL_SECONDS });
  return { total, source: "database" };
}

const server = http.createServer(async (req, res) => {
  try {
    // Separate from /hello deliberately: the health check polls this
    // repeatedly while waiting for the server to come up, and /hello has real
    // side effects (a database write, a cache read/write) — polling it as the
    // health check would count each poll as a visit.
    if (req.method === "GET" && req.url === "/healthz") {
      await pool.query("SELECT 1");
      res.writeHead(200);
      res.end();
      return;
    }
    if (req.method === "GET" && req.url === "/hello") {
      const { rows } = await pool.query(
        "INSERT INTO visits (message) VALUES ($1) RETURNING id",
        ["Hello from Ratect!"],
      );
      const { total, source } = await totalVisits();
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify(formatVisitResponse(rows[0].id, total, source)));
      return;
    }
    res.writeHead(404);
    res.end();
  } catch (err) {
    // Without this, a rejected `await` here (a transient Postgres/Redis
    // error mid-request) is an unhandled promise rejection — Node's default
    // since v15 is to crash the whole process over it, not just fail the
    // one request that hit it.
    console.error("Request failed:", err);
    res.writeHead(500);
    res.end();
  }
});

const port = process.env.PORT ?? 8080;
server.listen(port, () => {
  console.log(`Listening on port ${port}`);
});
