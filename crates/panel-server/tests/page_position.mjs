// Checks that the timeline page's thumb follows the viewer: the page polls the position image,
// reads the cursor from its width, ignores answers to older polls, and leaves the thumb under
// the finger during a drag and for a moment after the last seek. Reads the rendered page from
// stdin; `page.rs` runs it under `cargo test`.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { TRACK_LEFT, TRACK_WIDTH, runPage } from "./page_stand_ins.mjs";

const { clock, document, elements, images, timers } = runPage(readFileSync(0, "utf8"));
const track = elements.track;
const thumb = elements.thumb;
const POSITION_INTERVAL = 250;

function positionProbes() {
    return images.filter((image) => image.src.startsWith("/timeline/position?n="));
}

function latestProbe() {
    const probes = positionProbes();
    return probes[probes.length - 1];
}

function answer(probe, width) {
    probe.naturalWidth = width;
    probe.onload();
}

function poll() {
    const index = timers.findIndex((timer) => timer.delay === POSITION_INTERVAL);
    const [timer] = timers.splice(index, 1);
    timer.callback();
}

function mouse(clientX) {
    return { clientX, preventDefault() {} };
}

assert.equal(positionProbes().length, 1, "the page asks for the position on load");
assert.equal(
    timers.filter((timer) => timer.delay === POSITION_INTERVAL).length,
    1,
    "and polls again a quarter second later",
);
answer(latestProbe(), 501);
assert.equal(thumb.style.left, "50.00%", "width 501 is the middle of the timeline");

poll();
assert.equal(positionProbes().length, 2);
assert.notEqual(positionProbes()[0].src, positionProbes()[1].src, "every poll has its own URL");
answer(latestProbe(), 751);
assert.equal(thumb.style.left, "75.00%");

const olderProbe = latestProbe();
poll();
answer(latestProbe(), 1001);
answer(olderProbe, 1);
assert.equal(thumb.style.left, "100.00%", "an answer to an older poll is ignored");

track.dispatch("mousedown", mouse(TRACK_LEFT + 0.25 * TRACK_WIDTH));
poll();
answer(latestProbe(), 901);
assert.equal(thumb.style.left, "25.00%", "the thumb stays under the finger during a drag");

clock.now += 200;
document.dispatch("mouseup", mouse(TRACK_LEFT + 0.3 * TRACK_WIDTH));
clock.now += 500;
poll();
answer(latestProbe(), 1);
assert.equal(thumb.style.left, "30.00%", "and for a moment after the last seek");

clock.now += 600;
poll();
answer(latestProbe(), 301);
assert.equal(thumb.style.left, "30.00%");
answer(latestProbe(), 1001);
assert.equal(thumb.style.left, "100.00%", "then it follows the viewer again");

elements.play.dispatch("click", mouse(0));
poll();
answer(latestProbe(), 101);
assert.equal(thumb.style.left, "10.00%", "after Play the thumb follows playback");

console.log("page position: ok");
