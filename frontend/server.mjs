// Container entrypoint for the built portal. The Astro Node server installs no
// SIGTERM handler, and Node running as PID 1 ignores SIGTERM without one, so
// every pod stop ended in SIGKILL. This starts the same server and drains it:
// stop accepting, finish in-flight requests, force-close after a grace period.
process.env.ASTRO_NODE_AUTOSTART = "disabled";
const { startServer } = await import("./dist/server/entry.mjs");

const GRACE_MS = 10_000;
const { server, done } = startServer();

const shutdown = (signal) => {
  console.log(JSON.stringify({ level: "info", message: "shutdown signal received; draining", signal }));
  server.server.close();
  server.server.closeIdleConnections();
  setTimeout(() => server.stop().catch(() => {}), GRACE_MS).unref();
};
process.once("SIGTERM", shutdown);
process.once("SIGINT", shutdown);

try {
  await done;
  process.exit(0);
} catch (err) {
  console.error(err);
  process.exit(1);
}
