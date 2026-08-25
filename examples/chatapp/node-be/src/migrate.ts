import { ForgeClient } from "forgelib";

const reports = await ForgeClient.migrate();
for (const report of reports) console.error(`${report.target}: ${report.state} (${report.message})`);
if (reports.some((report) => report.state !== "applied")) process.exitCode = 1;
