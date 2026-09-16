import { createHash } from "node:crypto";
import { appendFileSync, readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const runNpm = (...args) => execFileSync("npm", args, { encoding: "utf8" });
const newer = (a, b) => {
    const parts = (v) => v.split(/[.-]/).slice(0, 5).map((s, i) => i === 3 ? s : Number(s));
    const left = parts(a), right = parts(b);
    for (const i of [0, 1, 2, 4])
        if ((left[i] ?? 0) !== (right[i] ?? 0)) return left[i] > right[i];
    return false;
};
async function lookupRegistry(name, version) {
    const response = await fetch(
        `https://registry.npmjs.org/${encodeURIComponent(name)}/${version}`,
        { signal: AbortSignal.timeout(30000) },
    );
    if (response.status === 404) return null;
    if (!response.ok)
        throw new Error(`npm lookup failed: HTTP ${response.status}`);
    return response.json();
}
/** Which packages a run covers: `--only=a,b` and `--exclude=c` narrow the scan. */
function selection(argv) {
    const values = (name) =>
        argv
            .filter((arg) => arg.startsWith(`${name}=`))
            .flatMap((arg) => arg.slice(name.length + 1).split(","))
            .filter(Boolean);
    return { only: values("--only"), exclude: values("--exclude") };
}

/** Publish exact packages once; injectable registry/CLI calls keep retry tests offline. */
export async function publishPackages(
    root,
    {
        npm = runNpm,
        lookup = lookupRegistry,
        sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
        availabilityAttempts = 80, // Twenty minutes per package at fifteen-second intervals.
        only = [],
        exclude = [],
    } = {},
) {
    const folders = readdirSync(root, { withFileTypes: true })
        .filter((entry) => entry.isDirectory())
        .map((entry) => entry.name)
        .filter(
            (name) =>
                (only.length === 0 || only.includes(name)) &&
                !exclude.includes(name),
        )
        .sort((a, b) => (a === "e" ? 1 : b === "e" ? -1 : a.localeCompare(b)));
    for (const folder of folders) {
        const path = resolve(root, folder);
        const manifest = JSON.parse(
            readFileSync(resolve(path, "package.json"), "utf8"),
        );
        const [packed] = Object.values(
            JSON.parse(
                npm(
                    "pack",
                    path,
                    "--json",
                    "--ignore-scripts",
                    "--pack-destination",
                    root,
                ),
            ),
        );
        const tarball = resolve(root, packed.filename);
        const integrity =
            "sha512-" +
            createHash("sha512").update(readFileSync(tarball)).digest("base64");
        let existing = await lookup(manifest.name, manifest.version);
        if (!existing) {
            console.log(`Publishing ${manifest.name}@${manifest.version}`);
            const channel = manifest.publishConfig?.tag ?? "latest";
            const latest = await lookup(manifest.name, channel);
            const tag = latest && newer(latest.version, manifest.version)
                ? `v${manifest.version}` : channel;
            try {
                npm("publish", tarball, "--access", "public", "--tag", tag, "--ignore-scripts");
            } catch (error) {
                // A previous attempt may have uploaded this immutable version already.
                if (!String(error.stderr ?? error.message).includes("previously staged version"))
                    throw error;
                console.log(`Already staged ${manifest.name}@${manifest.version}; waiting for npm`);
            }
            const deadline = Date.now() + 20 * 60 * 1000;
            for (let attempt = 0; attempt < availabilityAttempts && Date.now() < deadline; attempt++) {
                existing = await lookup(manifest.name, manifest.version);
                if (existing) break;
                console.log(`Waiting for npm processing: ${manifest.name}@${manifest.version}`);
                await sleep(15000);
            }
            if (!existing)
                throw new Error(`npm processing timed out for ${manifest.name}@${manifest.version}. Check npm package status, then rerun failed jobs to resume verification.`);
        }
        if (existing.dist?.integrity !== integrity)
            throw new Error(`Published content does not match ${manifest.name}@${manifest.version}`);
        console.log(`Verified ${manifest.name}@${manifest.version}`);
    }
}

if (
    process.argv[1] &&
    resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
    try {
        await publishPackages(
            resolve(process.argv[2]),
            selection(process.argv.slice(2)),
        );
    } catch (error) {
        const message = String(error.stderr ?? error.message);
        const reason = message.includes("npm processing timed out") ? "processing"
            : message.includes("Published content does not match") ? "integrity"
            : /E401|E403|ENEEDAUTH|EOTP/.test(message) ? "authorization" : "publication";
        if (process.env.GITHUB_OUTPUT)
            appendFileSync(process.env.GITHUB_OUTPUT, `failure=${reason}\n`);
        throw error;
    }
}
