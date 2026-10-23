// Runs the panel page's script with stand-ins for the image and timer APIs and checks that it
// keeps asking for frames while the panel server is gone, backing off, and resumes once frames
// load again. Reads the rendered page from stdin; `page.rs` runs it under `cargo test`.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const page = readFileSync(0, "utf8");
const script = page.match(/<script>([\s\S]*)<\/script>/)[1];

const timers = [];
const preloaders = [];
const shown = { src: "", style: { visibility: "hidden" } };
const status = { textContent: "waiting for frames" };

class StandInImage {
    constructor() {
        this.src = "";
        preloaders.push(this);
    }
}

vm.runInNewContext(script, {
    Image: StandInImage,
    Date,
    String,
    Math,
    setTimeout: (callback, delay) => timers.push({ callback, delay }),
    document: {
        getElementById: (id) => ({ frame: shown, status })[id],
    },
});

assert.equal(preloaders.length, 1, "one detached preloader");
const preloader = preloaders[0];

function runNextTimer() {
    const timer = timers.shift();
    timer.callback();
    return timer.delay;
}

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
