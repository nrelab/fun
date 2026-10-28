const readline = require("node:readline");

async function handler(request) {
  const payload = JSON.parse(request.body.toString("utf8") || "{}");
  const name = typeof payload.name === "string" ? payload.name : "world";
  const greeting = process.env.GREETING || "Hello";
  return {
    status: 200,
    headers: [["content-type", "application/json"]],
    body: Buffer.from(JSON.stringify({ message: `${greeting}, ${name}!` })),
  };
}

function respond(requestId, result) {
  if (result instanceof Error) {
    return {
      version: 1,
      request_id: requestId,
      outcome: "error",
      error: { kind: "user", message: result.message },
    };
  }
  return {
    version: 1,
    request_id: requestId,
    outcome: "response",
    status: result.status,
    headers: result.headers || [],
    body: Buffer.from(result.body || "").toString("base64"),
  };
}

async function main() {
  const lines = readline.createInterface({ input: process.stdin });
  for await (const line of lines) {
    let requestId = "";
    try {
      const request = JSON.parse(line);
      requestId = request.request_id;
      if (request.version !== 1) throw new Error("unsupported protocol version");
      request.body = Buffer.from(request.body, "base64");
      const result = await handler(request);
      process.stdout.write(`${JSON.stringify(respond(requestId, result))}\n`);
    } catch (error) {
      process.stdout.write(`${JSON.stringify(respond(requestId, error))}\n`);
    }
  }
}

main().catch((error) => {
  process.stderr.write(`${error.stack || error}\n`);
  process.exitCode = 1;
});
