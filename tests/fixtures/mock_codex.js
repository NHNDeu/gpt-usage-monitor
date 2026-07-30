"use strict";

const fs = require("fs");
const readline = require("readline");

const mode = process.env.MOCK_MODE || "success";
if (process.env.MOCK_PID_FILE) {
  fs.writeFileSync(process.env.MOCK_PID_FILE, String(process.pid));
}

function send(value, partial = false) {
  const line = JSON.stringify(value) + "\n";
  if (!partial) {
    process.stdout.write(line);
    return;
  }
  const split = Math.max(1, Math.floor(line.length / 2));
  process.stdout.write(line.slice(0, split));
  setTimeout(() => process.stdout.write(line.slice(split)), 8);
}

const input = readline.createInterface({
  input: process.stdin,
  crlfDelay: Infinity,
});

input.on("line", (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    process.stdout.write("{invalid json\n");
    return;
  }

  if (message.method === "initialize") {
    send({
      id: message.id,
      result: {
        userAgent: "mock-codex/1.0",
        codexHome: process.env.CODEX_HOME,
        platformFamily: "unix",
        platformOs: process.platform,
      },
    });
    return;
  }

  if (message.method === "initialized") {
    send({ method: "mock/ready", params: { ignored: true } });
    return;
  }

  if (message.method === "account/read") {
    if (mode === "unlogged") {
      send({
        id: message.id,
        result: { account: null, requiresOpenaiAuth: true },
      });
    } else {
      send({
        id: message.id,
        result: {
          account: {
            type: "chatgpt",
            email: "fixture@example.com",
            planType: "plus",
            unknownFutureField: 1,
          },
          requiresOpenaiAuth: true,
        },
      });
    }
    return;
  }

  if (message.method === "account/rateLimits/read") {
    if (mode === "timeout") {
      return;
    }
    if (mode === "early_exit") {
      process.exit(7);
    }
    if (mode === "invalid_json") {
      process.stdout.write("{not-json\n");
      return;
    }
    send(
      {
        id: message.id,
        result: {
          rateLimits: {
            primary: {
              usedPercent: 99,
              windowDurationMins: 300,
              resetsAt: null,
            },
          },
          rateLimitsByLimitId: {
            codex: {
              limitId: "codex",
              limitName: "Codex",
              primary: {
                usedPercent: 25,
                windowDurationMins: 300,
                resetsAt: 1893456000,
                future: true,
              },
              secondary: {
                usedPercent: 60.5,
                windowDurationMins: 10080,
                resetsAt: null,
              },
              rateLimitReachedType: null,
            },
          },
        },
      },
      mode === "partial",
    );
    return;
  }

  if (message.method === "account/usage/read") {
    send({
      id: message.id,
      result: {
        summary: {
          lifetimeTokens: 1234,
          currentStreakDays: 2,
        },
        dailyUsageBuckets: null,
      },
    });
    return;
  }

  send({
    id: message.id,
    error: { code: -32601, message: "method not found" },
  });
});

input.on("close", () => process.exit(0));
