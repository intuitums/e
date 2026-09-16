import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { publishPackages } from "./publish-npm.mjs";

/** Model the immutable tarball and registry responses without publishing test packages. */
function fixture(t) {
    const root = mkdtempSync(join(tmpdir(), "e-npm-publish-"));
    t.after(() => rmSync(root, { recursive: true, force: true }));
    mkdirSync(join(root, "e"));
    writeFileSync(
        join(root, "e/package.json"),
        JSON.stringify({ name: "@intuitums/e", version: "1.2.3" }),
    );
    writeFileSync(join(root, "e.tgz"), "tarball");
    const integrity =
        "sha512-" + createHash("sha512").update("tarball").digest("base64");
    const calls = [];
    const npm = (...args) => {
        calls.push(args);
        return JSON.stringify([{ filename: "e.tgz" }]);
    };
    return { root, integrity, calls, npm };
}

test("a narrowed run only publishes the packages it names", async (t) => {
    const f = fixture(t);
    mkdirSync(join(f.root, "slack"));
    writeFileSync(
        join(f.root, "slack/package.json"),
        JSON.stringify({ name: "@intuitums/e-slack", version: "1.2.3" }),
    );
    writeFileSync(join(f.root, "slack.tgz"), "slack tarball");
    const integrity = {};
    for (const [name, bytes] of [
        ["@intuitums/e", "tarball"],
        ["@intuitums/e-slack", "slack tarball"],
    ])
        integrity[name] =
            "sha512-" + createHash("sha512").update(bytes).digest("base64");
    const publish = async (options) => {
        const packed = [];
        await publishPackages(f.root, {
            ...options,
            npm: (command, path) => {
                if (command === "pack") packed.push(path.split("/").pop());
                return JSON.stringify([
                    { filename: path.endsWith("slack") ? "slack.tgz" : "e.tgz" },
                ]);
            },
            lookup: async (name) => ({ dist: { integrity: integrity[name] } }),
        });
        return packed;
    };
    assert.deepEqual(await publish({ exclude: ["slack"] }), ["e"]);
    assert.deepEqual(await publish({ only: ["slack"] }), ["slack"]);
});

test("retry accepts the already-published tarball without publishing twice", async (t) => {
    const f = fixture(t);
    await publishPackages(f.root, {
        npm: f.npm,
        lookup: async () => ({ dist: { integrity: f.integrity } }),
    });
    assert.deepEqual(
        f.calls.map((args) => args[0]),
        ["pack"],
    );
});

test("publishing an older missing version cannot move latest backward", async (t) => {
    const f = fixture(t);
    let published = false;
    await publishPackages(f.root, {
        npm: (...args) => {
            if (args[0] === "publish") published = true;
            return f.npm(...args);
        },
        lookup: async (_, version) =>
            version === "latest"
                ? { version: "2.0.0" }
                : published
                  ? { dist: { integrity: f.integrity } }
                  : null,
    });
    const command = f.calls.find((args) => args[0] === "publish");
    assert.equal(command[command.indexOf("--tag") + 1], "v1.2.3");
});

test("a different tarball under the same version fails without overwriting it", async (t) => {
    const f = fixture(t);
    await assert.rejects(
        publishPackages(f.root, {
            npm: f.npm,
            lookup: async () => ({ dist: { integrity: "wrong" } }),
            sleep: async () => {},
        }),
        /does not match/,
    );
    assert.deepEqual(
        f.calls.map((args) => args[0]),
        ["pack"],
    );
});

 test("an older beta retry cannot move beta backward or touch latest", async (t) => {
    const f = fixture(t);
    const version = "1.2.3-beta.9.gabcdef012345";
    writeFileSync(join(f.root, "e/package.json"), JSON.stringify({name:"@intuitums/e", version, publishConfig:{tag:"beta"}}));
    let published = false;
    const lookups = [];
    await publishPackages(f.root, {
        npm: (...args) => {
            if (args[0] === "publish") published = true;
            return f.npm(...args);
        },
        lookup: async (_, requested) => {
            lookups.push(requested);
            if (requested === "beta") return {version:"1.2.3-beta.12.gabcdef012345"};
            return published ? {dist:{integrity:f.integrity}} : null;
        },
    });
    const command = f.calls.find(args => args[0] === "publish");
    assert.equal(command[command.indexOf("--tag") + 1], `v${version}`);
    assert.ok(!lookups.includes("latest"));
});

for (const staged of [false, true]) {
    test(`waits for registry visibility without republishing (staged=${staged})`, async (t) => {
        const f = fixture(t);
        let reads = 0;
        await publishPackages(f.root, {
            npm: (...args) => {
                const result = f.npm(...args);
                if (staged && args[0] === "publish")
                    throw new Error('Cannot publish over previously staged version');
                return result;
            },
            lookup: async (_, version) => version === 'latest' || ++reads < 4
                ? null : {dist: {integrity: f.integrity}},
            sleep: async () => {},
        });
        assert.equal(f.calls.filter(args => args[0] === 'publish').length, 1);
    });
}

test('pending publication times out with resume instructions', async (t) => {
    const f = fixture(t);
    await assert.rejects(publishPackages(f.root, {
        npm: f.npm, lookup: async () => null, sleep: async () => {}, availabilityAttempts: 2,
    }), /npm processing timed out.*rerun failed jobs/);
    assert.equal(f.calls.filter(args => args[0] === 'publish').length, 1);
});

test('authentication failure stops without waiting or republishing', async (t) => {
    const f = fixture(t);
    await assert.rejects(publishPackages(f.root, {
        npm: (...args) => {
            if (args[0] === 'publish') throw new Error('E403 forbidden');
            return f.npm(...args);
        },
        lookup: async () => null,
        sleep: async () => assert.fail('must not wait for a rejected upload'),
    }), /E403/);
});
