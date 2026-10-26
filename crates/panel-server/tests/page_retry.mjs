// Checks that the panel page keeps asking for frames while the panel server is gone, backing
// off, and resumes once frames load again. Reads the rendered page from stdin; `page.rs` runs
// it under `cargo test`.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { runPage } from "./page_stand_ins.mjs";

const { elements, images, runNextTimer, timers } = runPage(readFileSync(0, "utf8"));
const shown = elements.frame;
const status = elements.status;

assert.equal(images.length, 1, "one detached preloader");
const preloader = images[0];

function failFrames(count) {
    const delays = [];
    for (let attempt = 0; attempt < count; attempt += 1) {
        preloader.onerror();
        delays.push(runNextTimer());
    }
    return delays;
}

const firstRequest = preloader.src;
assert.match(firstRequest, /^\/views\/camera\/frame\.png\?n=\d+-1$/);

assert.deepEqual(
    failFrames(6),
    [500, 1000, 2000, 4000, 4000, 4000],
    "no server: retries back off to every 4 s and never stop",
);
assert.match(preloader.src, /-7$/, "every retry is a new request");
assert.equal(shown.style.visibility, "hidden");

preloader.onload();
assert.equal(shown.src, preloader.src, "the server is back: the loaded frame is shown");
assert.equal(shown.style.visibility, "visible");
assert.equal(status.textContent, "");
assert.equal(runNextTimer(), 50, "and the next frame follows at once");

assert.deepEqual(failFrames(2), [500, 1000], "a later outage backs off from the start again");
assert.equal(timers.length, 0);

console.log("page retry: ok");
