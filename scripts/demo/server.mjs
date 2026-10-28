import { constants } from "node:fs";
import { open, realpath } from "node:fs/promises";
import { createServer } from "node:http";
import { extname, isAbsolute, relative, resolve, sep } from "node:path";
import { pipeline } from "node:stream/promises";

const publicDirectories = ["examples", "packages/hoke/dist", "packages/hoke/wasm"];
const mimeTypes = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".map": "application/json; charset=utf-8",
  ".wasm": "application/wasm",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".ico": "image/x-icon",
};

function contains(parent, child) {
  const path = relative(parent, child);
  return path === "" || (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`));
}

function isControl(character) {
  const code = character.charCodeAt(0);
  return code < 32 || (code >= 127 && code <= 159) || code === 8232 || code === 8233;
}

function safeLabel(value) {
  return Array.from(String(value).slice(0, 512), (character) =>
    isControl(character) ? "?" : character,
  ).join("");
}

function reply(response, status, message) {
  if (response.destroyed) return;
  response.writeHead(status, {
    "Content-Type": "text/plain; charset=utf-8",
    "Cache-Control": "no-store",
    ...(status === 405 ? { Allow: "GET, HEAD" } : {}),
  });
  response.end(`${message}\n`);
}

async function serve(root, request, response) {
  if (request.method !== "GET" && request.method !== "HEAD") {
    reply(response, 405, "Method not allowed");
    return;
  }
  let pathname;
  try {
    pathname = decodeURIComponent((request.url ?? "/").split(/[?#]/, 1)[0]);
  } catch {
    reply(response, 400, "Invalid URL encoding");
    return;
  }
  if (
    !pathname.startsWith("/") ||
    pathname.includes("\\") ||
    Array.from(pathname).some(isControl) ||
    pathname.split("/").some((part) => part === ".." || part === ".")
  ) {
    reply(response, 403, "Path is not public");
    return;
  }
  if (pathname === "/" || pathname === "/examples" || pathname === "/examples/") {
    pathname = "/examples/index.html";
  }
  const directory = publicDirectories.find(
    (entry) => pathname === `/${entry}` || pathname.startsWith(`/${entry}/`),
  );
  if (!directory) {
    reply(response, 404, "Not found");
    return;
  }
  const allowedRoot = await realpath(resolve(root, directory));
  if (!contains(root, allowedRoot)) {
    reply(response, 403, "Path is not public");
    return;
  }
  let filename = await realpath(resolve(root, `.${pathname}`));
  if (!contains(allowedRoot, filename)) {
    reply(response, 403, "Path is not public");
    return;
  }
  let file = await open(filename, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    let info = await file.stat();
    if (info.isDirectory()) {
      await file.close();
      file = undefined;
      filename = await realpath(resolve(filename, "index.html"));
      if (!contains(allowedRoot, filename)) {
        reply(response, 403, "Path is not public");
        return;
      }
      file = await open(filename, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
      info = await file.stat();
    }
    if (!info.isFile()) {
      reply(response, 404, "Not found");
      return;
    }
    response.writeHead(200, {
      "Content-Type": mimeTypes[extname(filename).toLowerCase()] ?? "application/octet-stream",
      "Content-Length": info.size,
      "Cache-Control": "no-store",
      "X-Content-Type-Options": "nosniff",
    });
    if (request.method === "HEAD") response.end();
    else await pipeline(file.createReadStream({ autoClose: false }), response);
  } finally {
    await file?.close();
  }
}

function listen(server, port, host) {
  return new Promise((resolveListen, reject) => {
    const failed = (error) => {
      server.off("listening", listening);
      reject(error);
    };
    const listening = () => {
      server.off("error", failed);
      resolveListen();
    };
    server.once("error", failed);
    server.once("listening", listening);
    server.listen(port, host);
  });
}

export async function startDemoServer({
  root,
  port = 8000,
  host = "127.0.0.1",
  strictPort = false,
  log = () => {},
}) {
  if (!Number.isInteger(port) || port < 0 || port > 65535) {
    throw new RangeError("Demo port must be an integer between 0 and 65535");
  }
  const publicRoot = await realpath(root);
  const server = createServer((request, response) => {
    const started = performance.now();
    response.once("finish", () => {
      const level =
        response.statusCode >= 500 ? "error" : response.statusCode >= 400 ? "warn" : "info";
      log(
        level,
        `${response.statusCode} ${safeLabel(request.method)} ${safeLabel(request.url)} ${Math.round(performance.now() - started)}ms`,
      );
    });
    serve(publicRoot, request, response).catch((error) => {
      if (response.headersSent) {
        response.destroy();
        if (error.code !== "ERR_STREAM_PREMATURE_CLOSE" && error.code !== "ECONNRESET") {
          log("error", `Response interrupted: ${safeLabel(error.message)}`);
        }
        return;
      }
      if (["ENOENT", "ENOTDIR"].includes(error.code)) reply(response, 404, "Not found");
      else if (["EACCES", "EPERM", "ELOOP"].includes(error.code)) {
        reply(response, 403, "Path is not public");
      } else {
        log("error", `Cannot serve request: ${safeLabel(error.message)}`);
        reply(response, 500, "Cannot read demo file");
      }
    });
  });
  let actualPort;
  let lastCause;
  for (let offset = 0; offset < 20 && port + offset <= 65535; offset++) {
    const candidate = port === 0 ? 0 : port + offset;
    try {
      await listen(server, candidate, host);
      actualPort = server.address().port;
      break;
    } catch (cause) {
      lastCause = cause;
      if (cause.code !== "EADDRINUSE" || port === 0 || strictPort) {
        throw new Error(`Cannot start demo server at ${host}:${candidate}: ${cause.message}`, {
          cause,
        });
      }
      log("warn", `Port ${candidate} is occupied; trying the next port.`);
    }
  }
  if (actualPort === undefined) {
    throw new Error(`No available demo port starting at ${port}`, { cause: lastCause });
  }
  server.on("error", (error) => log("error", `Demo server error: ${safeLabel(error.message)}`));
  let closing;
  return {
    server,
    port: actualPort,
    origin: `http://${host.includes(":") ? `[${host}]` : host}:${actualPort}`,
    close() {
      closing ??= new Promise((resolveClose, reject) => {
        server.close((error) => (error ? reject(error) : resolveClose()));
        server.closeAllConnections();
      });
      return closing;
    },
  };
}
